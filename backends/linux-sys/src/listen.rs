//! The descriptors a service manager passes to a process: socket activation and the file descriptor
//! store. systemd sets `LISTEN_PID` to the process that gets them, `LISTEN_FDS` to how many there
//! are, and `LISTEN_FDNAMES` to their names (colon-separated), and they start at fd 3.
//!
//! [`take`] is the one place the process takes them. The store's descriptors are how a restarted
//! daemon gets back a watchdog it must not open by name (opening a running watchdog feeds it).

use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};

/// The first descriptor a service manager passes (`SD_LISTEN_FDS_START`).
const FIRST_FD: RawFd = 3;
/// The most a count may say: systemd's own limit on passed descriptors is far below this.
const MAX_FDS: usize = 1024;

static TAKEN: AtomicBool = AtomicBool::new(false);

/// The descriptors this process was passed, each with its name, owned and close-on-exec. Empty when
/// `LISTEN_PID` is not this process's id, or when they were taken already: the first call takes
/// them all, and a later call finds none, so no descriptor has two owners.
#[must_use]
pub fn take() -> Vec<(String, OwnedFd)> {
    if TAKEN.swap(true, Ordering::SeqCst) {
        return Vec::new();
    }
    let Some(count) = passed_count(std::process::id()) else {
        return Vec::new();
    };
    let names = std::env::var("LISTEN_FDNAMES").unwrap_or_default();
    let mut names = names.split(':');
    let mut taken = Vec::with_capacity(count);
    for index in 0..count {
        let fd = FIRST_FD + index as RawFd;
        let name = names
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("unknown")
            .to_owned();
        // SAFETY: F_GETFD only asks whether `fd` is open, and changes nothing.
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
            continue;
        }
        // SAFETY: the service manager passed `fd` open to this process (LISTEN_PID is our id), and
        // `TAKEN` makes this the one call that takes it, so nothing else owns it.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: `fd` is open and owned by `owned` now; F_SETFD only sets its close-on-exec flag.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            continue;
        }
        taken.push((name, owned));
    }
    taken
}

/// How many descriptors `LISTEN_PID` and `LISTEN_FDS` say `pid` was passed.
fn passed_count(pid: u32) -> Option<usize> {
    count_for(
        pid,
        std::env::var("LISTEN_PID").ok().as_deref(),
        std::env::var("LISTEN_FDS").ok().as_deref(),
    )
}

/// The count for `pid` from the two variables' values: none unless `listen_pid` is `pid`, and none when
/// `listen_fds` is not a count. A count above [`MAX_FDS`] is cut to it.
fn count_for(pid: u32, listen_pid: Option<&str>, listen_fds: Option<&str>) -> Option<usize> {
    if listen_pid?.parse::<u32>().ok()? != pid {
        return None;
    }
    let count = listen_fds?.parse::<usize>().ok()?;
    Some(count.min(MAX_FDS))
}

/// Makes the process `command` starts be passed `fds` the way a service manager passes them: at fd 3
/// onward, in order, with `LISTEN_PID` its own id, `LISTEN_FDS` their count and `LISTEN_FDNAMES` their
/// names. For a test harness that stands in for the manager (its file descriptor store, on a restart).
/// The descriptors are the caller's, open until the spawn; the child gets duplicates. A source number
/// that is itself one of the target numbers is not supported (the harness's descriptors are high).
pub fn pass_on(command: &mut std::process::Command, fds: Vec<(String, OwnedFd)>) {
    use std::os::unix::process::CommandExt;
    let names: Vec<&str> = fds.iter().map(|(name, _)| name.as_str()).collect();
    command.env("LISTEN_FDS", fds.len().to_string());
    command.env("LISTEN_FDNAMES", names.join(":"));
    // LISTEN_PID is not set here: it names the process that execs the service, which is the one that knows its
    // own id. The launcher (`lemnos-unit-check exec`) sets it to its own pid just before it execs lemnosd, as
    // systemd sets it for the main process. A value set in `pre_exec` would be lost: Rust replaces the child's
    // environment with the one built for `command.env` after `pre_exec` has run.
    let sources: Vec<RawFd> = fds
        .iter()
        .map(|(_, fd)| std::os::fd::AsRawFd::as_raw_fd(fd))
        .collect();
    // Runs in the child between fork and exec. `fds` stays alive in the closure until the exec, so every source it
    // duplicates is open. dup2 leaves the new descriptor without close-on-exec, so it survives the exec.
    let hook = move || -> io::Result<()> {
        let _keep = &fds;
        for (index, source) in sources.iter().enumerate() {
            let target = FIRST_FD + index as RawFd;
            // SAFETY: duplicates `source`, a descriptor the parent holds open (`fds`, kept above), onto
            // `target`; one call.
            if unsafe { libc::dup2(*source, target) } < 0 {
                return Err(io::Error::last_os_error());
            }
            // dup2 does not clear close-on-exec when source and target are the same descriptor, and a descriptor
            // opened by std is close-on-exec: clear it on the target, so it survives the exec in every case.
            // SAFETY: F_SETFD on the target only changes its flags; fcntl is async-signal-safe.
            if unsafe { libc::fcntl(target, libc::F_SETFD, 0) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    };
    // SAFETY: `pre_exec` is unsafe because `hook` runs in the forked child before exec. `hook` only duplicates
    // descriptors, which is async-signal-safe, so the child cannot block on a lock another thread held at fork.
    unsafe {
        command.pre_exec(hook);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_are_taken_only_by_the_process_they_were_passed_to() {
        assert_eq!(count_for(42, Some("42"), Some("2")), Some(2));
        assert_eq!(
            count_for(42, Some("41"), Some("2")),
            None,
            "another process's"
        );
        assert_eq!(
            count_for(42, None, Some("2")),
            None,
            "no LISTEN_PID: none passed"
        );
        assert_eq!(
            count_for(42, Some("42"), None),
            None,
            "no LISTEN_FDS: none passed"
        );
    }

    #[test]
    fn a_count_that_is_not_a_number_takes_nothing_and_a_huge_one_is_cut() {
        assert_eq!(count_for(7, Some("7"), Some("two")), None);
        assert_eq!(count_for(7, Some("x"), Some("1")), None);
        assert_eq!(count_for(7, Some("7"), Some("100000")), Some(MAX_FDS));
        assert_eq!(count_for(7, Some("7"), Some("0")), Some(0));
    }

    #[test]
    fn a_passed_descriptor_is_at_three_and_its_names_are_passed() {
        use std::os::fd::AsRawFd;
        let file = std::fs::File::open("/dev/null").unwrap();
        let fd = OwnedFd::from(file);
        let source = fd.as_raw_fd();
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("echo fds=$LISTEN_FDS names=$LISTEN_FDNAMES; test -e /proc/self/fd/3 && echo fd3-open")
            .stdout(std::process::Stdio::piped());
        pass_on(&mut command, vec![("lemnos-watchdog".to_owned(), fd)]);
        let out = command.spawn().unwrap().wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            text.contains("fds=1 names=lemnos-watchdog"),
            "{text} (source fd {source})"
        );
        assert!(text.contains("fd3-open"), "{text}");
    }
}
