//! Claims: a second claimant is refused, closing releases, a killed holder releases, malformed
//! names are refused, and their number has an end.
//!
//! Usage: `lemnos-claim-check /dev/lemnos`. Port of `kernel/test/lemnos_claim_check.c` (CZ-GUEST).
//! Static musl, so the image needs nothing else. The holders are forked processes that keep a
//! claim until they are killed, as the C original's are.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::process::ExitCode;
use std::thread;

use lemnos_linux_sys::guest;
use lemnos_linux_sys::privilege::{self, ChildEnd};

/// The number of claims the test holds at once (the kernel's limit is what it checks).
const LIMIT: usize = 4096;
/// Room for a claim's name: the kernel's limit less its terminating NUL.
const NAME_ROOM: usize = guest::CLAIM_NAME_MAX - 1;

fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Prints one check's line and counts a failure.
fn check(failures: &mut u32, ok: bool, what: &str) {
    say(&format!("{} {what}", if ok { "ok   " } else { "FAIL " }));
    *failures += u32::from(!ok);
}

/// The claim on `name` made through `fd`, or the errno the kernel refused it with.
fn claim(fd: BorrowedFd<'_>, name: &str) -> Result<OwnedFd, i32> {
    let bytes = &name.as_bytes()[..name.len().min(NAME_ROOM)];
    guest::claim_raw(fd, bytes, 0).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))
}

/// The raw request, with a name and flags the helper above does not send: the kernel's answer.
fn claim_errno(fd: BorrowedFd<'_>, resource: &[u8], flags: u32) -> Option<i32> {
    guest::claim_raw(fd, resource, flags)
        .err()
        .and_then(|e| e.raw_os_error())
}

/// A forked child that takes `name` (through `fd`, or through its own open of `node` when `fd`
/// is `None`), says so on the pipe, and keeps the claim until it is killed.
/// Returns the child's pid and whether it got the claim.
fn holder(fd: Option<BorrowedFd<'_>>, node: &str, name: &str) -> (libc::pid_t, bool) {
    let Ok((mut reader, writer)) = io::pipe() else {
        return (-1, false);
    };
    let Ok(child) = privilege::fork() else {
        return (-1, false);
    };
    if child == 0 {
        // The child: its own open, when it was not given the parent's.
        let own: Option<File> = if fd.is_none() {
            OpenOptions::new().read(true).write(true).open(node).ok()
        } else {
            None
        };
        let held = match (fd, own.as_ref()) {
            (Some(fd), _) => claim(fd, name).ok(),
            (None, Some(file)) => claim(file.as_fd(), name).ok(),
            (None, None) => None,
        };
        let mut writer = writer;
        let _ = writer.write(&[u8::from(held.is_some())]);
        loop {
            // Keeps the claim and the node for as long as it lives; SIGKILL ends it.
            thread::park();
        }
    }
    drop(writer);
    let mut ok = [0u8; 1];
    let got = matches!(reader.read(&mut ok), Ok(1)) && ok[0] != 0;
    (child, got)
}

/// Kills the holder with SIGKILL and says it was killed by that signal. A pid that is not a
/// holder's (the fork failed, so none exists) is never signalled: `kill` with 0 or -1 would reach
/// other processes.
fn killed(child: libc::pid_t) -> bool {
    if child <= 0 || guest::kill(child, libc::SIGKILL).is_err() {
        return false;
    }
    matches!(privilege::wait_child(child), Ok(ChildEnd::Signal(sig)) if sig == libc::SIGKILL)
}

fn main() -> ExitCode {
    let node = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/lemnos".to_owned());
    let mut failures = 0;

    let mut fd = match OpenOptions::new().read(true).write(true).open(&node) {
        Ok(file) => file,
        Err(e) => {
            let _ = writeln!(io::stderr(), "{node}: {e}");
            return ExitCode::from(2);
        }
    };

    let a = claim(fd.as_fd(), "bus-1/target-72");
    let first_fd = a.as_ref().map_or(-1, AsRawFd::as_raw_fd);
    check(
        &mut failures,
        a.is_ok(),
        &format!("first claim held (fd {first_fd})"),
    );
    check(
        &mut failures,
        claim(fd.as_fd(), "bus-1/target-72").err() == Some(libc::EBUSY),
        "second claimant refused (EBUSY)",
    );
    let b = claim(fd.as_fd(), "bus-1/target-73");
    check(&mut failures, b.is_ok(), "a different name is free");
    let a = match a {
        Ok(a) => {
            check(
                &mut failures,
                guest::close_on_exec(a.as_fd()).unwrap_or(false),
                "the claim is close-on-exec",
            );
            drop(a);
            claim(fd.as_fd(), "bus-1/target-72").ok()
        }
        Err(_) => None,
    };
    check(&mut failures, a.is_some(), "closing released it");
    drop(a);
    drop(b);

    // A copy of the file keeps the claim.
    let a = claim(fd.as_fd(), "copied").ok();
    let b = a.as_ref().and_then(|a| a.try_clone().ok());
    drop(a);
    check(
        &mut failures,
        claim(fd.as_fd(), "copied").err() == Some(libc::EBUSY),
        "a copy of the file still holds it",
    );
    drop(b);
    let a = claim(fd.as_fd(), "copied");
    check(
        &mut failures,
        a.is_ok(),
        "the last copy closing released it",
    );
    drop(a);

    check(
        &mut failures,
        claim_errno(fd.as_fd(), &[], 0) == Some(libc::EINVAL),
        "empty name: EINVAL",
    );
    check(
        &mut failures,
        claim_errno(fd.as_fd(), &[b'x'; guest::CLAIM_NAME_MAX], 0) == Some(libc::EINVAL),
        "unterminated name: EINVAL",
    );
    check(
        &mut failures,
        claim_errno(fd.as_fd(), b"flagged", 1) == Some(libc::EINVAL),
        "unknown flags: EINVAL",
    );

    // A crash: another process holds it, is killed, and the claim ends.
    let (child, got) = holder(Some(fd.as_fd()), &node, "crash");
    check(&mut failures, got, "a second process holds a claim");
    check(
        &mut failures,
        claim(fd.as_fd(), "crash").err() == Some(libc::EBUSY),
        "refused while that process lives",
    );
    check(&mut failures, killed(child), "that process killed");
    let a = claim(fd.as_fd(), "crash");
    check(&mut failures, a.is_ok(), "the crash released it");
    drop(a);

    // The holder of the control node crashes: the node and its claims come free.
    drop(fd);
    let (child, got) = holder(None, &node, "whole-process");
    check(
        &mut failures,
        got,
        "another process has the node and a claim",
    );
    check(
        &mut failures,
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&node)
            .err()
            .and_then(|e| e.raw_os_error())
            == Some(libc::EBUSY),
        "the node is refused while it lives",
    );
    check(&mut failures, killed(child), "that process killed");
    let reopened = OpenOptions::new().read(true).write(true).open(&node);
    check(&mut failures, reopened.is_ok(), "the crash freed the node");
    let Ok(file) = reopened else {
        return finish(&node, failures);
    };
    fd = file;
    let a = claim(fd.as_fd(), "whole-process");
    check(&mut failures, a.is_ok(), "and the claim");
    drop(a);

    // Claims are kernel memory: their number has an end.
    check(
        &mut failures,
        guest::set_nofile_limit(8192).is_ok(),
        "room for every claim",
    );
    let mut held: Vec<OwnedFd> = Vec::new();
    while held.len() < LIMIT {
        match claim(fd.as_fd(), &format!("many-{}", held.len())) {
            Ok(f) => held.push(f),
            Err(_) => break,
        }
    }
    check(
        &mut failures,
        held.len() == LIMIT,
        &format!("{} claims held", held.len()),
    );
    check(
        &mut failures,
        claim(fd.as_fd(), "one-too-many").err() == Some(libc::ENOSPC),
        "one more: ENOSPC",
    );
    drop(held);
    let a = claim(fd.as_fd(), "one-too-many");
    check(&mut failures, a.is_ok(), "closing them made room");
    drop(a);
    drop(fd);

    finish(&node, failures)
}

/// Prints the summary line and returns the exit code: 0 when every check passed.
fn finish(node: &str, failures: u32) -> ExitCode {
    say(&format!("{node}: {failures} failed"));
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
