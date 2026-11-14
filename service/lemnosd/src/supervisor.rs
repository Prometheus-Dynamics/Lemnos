//! The gadget helper's root parent: a process that does one thing, which is to fork the helper
//! again when it dies.
//!
//! The helper has to be root (the FunctionFS mount, configfs) and the daemon is not, so nothing the
//! daemon does can start a helper again. This parent is forked by the root phase, before the drop,
//! and keeps root for as long as the daemon runs. It is deliberately small:
//!
//! - it holds the helper's end of the socket pair across forks, so the daemon's end never changes
//!   and a request the daemon wrote while the helper was gone is read by the next one;
//! - it forks the helper (`helper::run_child`), waits for it, and when it ends says how
//!   (`lemnosd: the gadget helper (pid N) was killed by signal 31; starting it again in 1000 ms`),
//!   throws away what the dead helper never read, waits 1, 2, 4 and then 8 seconds (the failure rule's
//!   cap; the wait starts over after the helper has run for 30 s) and forks again. It never gives up;
//! - it ends when the helper ends with status 0, which is what the helper does when the daemon's end
//!   of the socket closes, and when the daemon dies (`PR_SET_PDEATHSIG`);
//! - it reads no request and acts on none: its only input is the child's exit status;
//! - it drops to `CAP_SYS_ADMIN` and the two capabilities the helper needs to drop the rest
//!   (`CAP_SETPCAP` for the bounding set, `CAP_SETGID` for the supplementary groups; no
//!   `CAP_SETUID`: the helper is root like its parent, so no user id changes) and runs under a seccomp allowlist
//!   ([`PARENT_CALLS`]: the helper's calls, which it passes on, and the calls that fork, wait and
//!   sleep).

use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use crate::helper::{self, Daemon, Gadget};
use lemnos_linux_sys::privilege::{self, CAP_SETGID, CAP_SETPCAP, CAP_SYS_ADMIN, ChildEnd, Plan};
use lemnos_linux_sys::seccomp::{self, nr};
use lemnos_linux_sys::unix::set_nonblocking;

/// The process name of the parent: what `ps` shows for it.
pub const PROCESS_NAME: &str = "lemnosd-gsuper";
/// The wait before the first fork again; each further death doubles it.
pub const FIRST_WAIT_MS: u64 = 1_000;
/// The longest wait, the failure rule's cap.
pub const CAP_WAIT_MS: u64 = 8_000;
/// How long a helper must run for the wait to start over.
pub const HEALTHY_MS: u64 = 30_000;

/// The capabilities the parent keeps: the helper's own (`CAP_SYS_ADMIN`) and the two it needs to
/// drop everything else (`CAP_SETPCAP`, `CAP_SETGID`). The helper drops the last two the moment it
/// starts. No `CAP_SETUID`: neither process changes its user id.
pub const KEEP: &[u32] = &[CAP_SYS_ADMIN, CAP_SETPCAP, CAP_SETGID];

/// The system calls the parent may make: everything the helper makes (the filter stacks, so the
/// parent's must allow what the helper's start-up and verbs call), the calls that make and wait for
/// a process, the capability and identity calls the helper's drop makes, and the sleep.
#[must_use]
pub fn calls() -> Vec<i64> {
    let mut calls = helper::BASE_CALLS.to_vec();
    calls.extend_from_slice(&[
        nr::CLONE,
        nr::CLONE3,
        nr::WAIT4,
        nr::WAITID,
        nr::CLOCK_NANOSLEEP,
        nr::NANOSLEEP,
        nr::PRCTL,
        nr::CAPGET,
        nr::CAPSET,
        nr::GETPID,
        nr::GETTID,
        nr::SET_ROBUST_LIST,
        nr::RSEQ,
        nr::SET_TID_ADDRESS,
        nr::GETPPID,
        nr::GETUID,
        nr::GETEUID,
        nr::GETGID,
        nr::GETEGID,
        nr::GETGROUPS,
        nr::SETGROUPS,
        nr::FUTEX,
        nr::SCHED_YIELD,
        nr::SECCOMP,
        nr::POLL,
    ]);
    seccomp::calls_with_fork(&calls)
}

/// How a helper ended, for the parent's log.
fn how(end: ChildEnd) -> String {
    match end {
        ChildEnd::Exit(code) => format!("ended with status {code}"),
        ChildEnd::Signal(signal) => format!("was killed by signal {signal}"),
    }
}

/// The wait before fork number `streak` (0 for the first death): 1, 2, 4, then 8 seconds.
#[must_use]
pub fn wait_ms(streak: u32) -> u64 {
    (FIRST_WAIT_MS << streak.min(3)).min(CAP_WAIT_MS)
}

/// What the parent does, so that a test can stand in for the system.
pub trait Process {
    /// Forks a helper, and returns its process id.
    ///
    /// # Errors
    ///
    /// The fork fails.
    fn start(&mut self) -> io::Result<i32>;
    /// Waits for the helper `pid` to end.
    ///
    /// # Errors
    ///
    /// The wait fails.
    fn wait(&mut self, pid: i32) -> io::Result<ChildEnd>;
    /// Throws away what the dead helper never read.
    fn clean(&mut self);
    /// Waits `ms` milliseconds.
    fn pause(&mut self, ms: u64);
    /// Says one line to the log.
    fn log(&mut self, line: &str);
    /// Milliseconds on a clock that only goes forward.
    fn now_ms(&mut self) -> u64;
}

/// Runs the helper, and again whenever it dies, `rounds` times (`None` for as long as the daemon
/// runs). A helper that ends with status 0 is the daemon's end closing: the parent ends too.
pub fn supervise(process: &mut dyn Process, rounds: Option<u32>) {
    let mut streak: u32 = 0;
    let mut round: u32 = 0;
    loop {
        if rounds.is_some_and(|limit| round >= limit) {
            return;
        }
        round = round.saturating_add(1);
        let pid = match process.start() {
            Ok(pid) => pid,
            Err(error) => {
                let wait = wait_ms(streak);
                streak = streak.saturating_add(1);
                process.log(&format!(
                    "lemnosd: the gadget helper cannot be started ({error}); trying again in {wait} ms"
                ));
                process.pause(wait);
                continue;
            }
        };
        process.log(&format!("lemnosd: the gadget helper started (pid {pid})"));
        let started = process.now_ms();
        let end = process.wait(pid);
        if matches!(end, Ok(ChildEnd::Exit(0))) {
            process.log(&format!(
                "lemnosd: the gadget helper (pid {pid}) ended with status 0: the daemon's end of its socket closed"
            ));
            return;
        }
        if process.now_ms().saturating_sub(started) >= HEALTHY_MS {
            streak = 0;
        }
        process.clean();
        let wait = wait_ms(streak);
        streak = streak.saturating_add(1);
        let what = match end {
            Ok(end) => how(end),
            Err(error) => format!("cannot be waited for ({error})"),
        };
        process.log(&format!(
            "lemnosd: the gadget helper (pid {pid}) {what}; starting it again in {wait} ms (restart {round})"
        ));
        process.pause(wait);
    }
}

/// The system's parent: forks the helper on the socket, which it keeps.
struct System<'a> {
    stream: UnixStream,
    gadgets: &'a [Gadget],
    daemon_pid: i32,
    began: Instant,
}

impl Process for System<'_> {
    fn start(&mut self) -> io::Result<i32> {
        let child = self.stream.try_clone()?;
        let pid = privilege::fork()?;
        if pid == 0 {
            helper::run_child(
                child,
                self.gadgets,
                Daemon {
                    pid: self.daemon_pid,
                    uid: 0,
                },
            );
        }
        Ok(pid)
    }

    fn wait(&mut self, pid: i32) -> io::Result<ChildEnd> {
        privilege::wait_child(pid)
    }

    fn clean(&mut self) {
        // Reads what is waiting and no more: the socket is switched to non-blocking for the reads
        // and back before the next fork, which shares its file description.
        // With `fcntl`, not std's `ioctl`, which the parent's filter does not allow.
        if set_nonblocking(self.stream.as_fd(), true).is_err() {
            return;
        }
        let mut sink = [0u8; 256];
        while matches!(self.stream.read(&mut sink), Ok(n) if n > 0) {}
        let _ = set_nonblocking(self.stream.as_fd(), false);
    }

    fn pause(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn log(&mut self, line: &str) {
        let _ = writeln!(io::stderr(), "{line}");
    }

    fn now_ms(&mut self) -> u64 {
        u64::try_from(self.began.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// The parent's body, run in the forked child of the root phase: names itself, ignores the signals
/// of the daemon's process group, drops to what it keeps, narrows its system calls, and runs the
/// helper. Never returns.
pub fn run(stream: UnixStream, gadgets: &[Gadget]) -> ! {
    // The daemon's process id, as the helper must see it as the socket pair's maker. The parent is
    // forked by the daemon, so its parent is the daemon.
    let daemon_pid = privilege::parent_pid();
    let mut ok = privilege::set_name(PROCESS_NAME).is_ok() && privilege::die_with_parent().is_ok();
    for signal in [1, 2, 15] {
        ok &= privilege::ignore_signal(signal).is_ok();
    }
    let plan = Plan {
        uid: 0,
        gid: 0,
        groups: Vec::new(),
        keep: KEEP.to_vec(),
    };
    ok &= privilege::drop_to(&plan).is_ok();
    ok &= privilege::observe()
        .map(|now| privilege::verify(&plan, &now).is_ok())
        .unwrap_or(false);
    if !ok {
        let _ = writeln!(
            io::stderr(),
            "{PROCESS_NAME}: cannot take its name, its signals or its capabilities: ending"
        );
        std::process::exit(1);
    }
    if let Err(error) = seccomp::install(&calls()) {
        let _ = writeln!(
            io::stderr(),
            "{PROCESS_NAME}: cannot install its filter ({error}): ending"
        );
        std::process::exit(1);
    }
    let _ = writeln!(
        io::stderr(),
        "{PROCESS_NAME}: running, for daemon pid {daemon_pid}"
    );
    let mut system = System {
        stream,
        gadgets,
        daemon_pid,
        began: Instant::now(),
    };
    supervise(&mut system, None);
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn the_parent_keeps_no_capability_and_allows_no_call_that_changes_a_user_or_group_id() {
        assert_eq!(KEEP, [CAP_SYS_ADMIN, CAP_SETPCAP, CAP_SETGID]);
        let calls = calls();
        for call in [nr::SETUID, nr::SETRESUID, nr::SETGID, nr::SETRESGID] {
            assert!(!calls.contains(&call), "{call}");
        }
        // The groups are cleared with `setgroups` (only when they are not empty already), which stays.
        assert!(calls.contains(&nr::SETGROUPS));
    }

    /// A parent with a script: each start gives the next pid, each wait the next ending and the
    /// time the helper ran for. Everything it does is written down.
    struct Fake {
        endings: VecDeque<(io::Result<ChildEnd>, u64)>,
        starts_failing: u32,
        next_pid: i32,
        now: u64,
        events: Vec<String>,
        pauses: Vec<u64>,
        log: Vec<String>,
    }

    impl Fake {
        fn new(endings: Vec<(ChildEnd, u64)>) -> Self {
            Self {
                endings: endings.into_iter().map(|(e, ran)| (Ok(e), ran)).collect(),
                starts_failing: 0,
                next_pid: 100,
                now: 0,
                events: Vec::new(),
                pauses: Vec::new(),
                log: Vec::new(),
            }
        }
    }

    impl Process for Fake {
        fn start(&mut self) -> io::Result<i32> {
            if self.starts_failing > 0 {
                self.starts_failing -= 1;
                self.events.push("start failed".into());
                return Err(io::Error::from_raw_os_error(11));
            }
            self.next_pid += 1;
            self.events.push(format!("start {}", self.next_pid));
            Ok(self.next_pid)
        }

        fn wait(&mut self, pid: i32) -> io::Result<ChildEnd> {
            self.events.push(format!("wait {pid}"));
            let (end, ran) = self.endings.pop_front().expect("a scripted ending");
            self.now += ran;
            end
        }

        fn clean(&mut self) {
            self.events.push("clean".into());
        }

        fn pause(&mut self, ms: u64) {
            self.pauses.push(ms);
        }

        fn log(&mut self, line: &str) {
            // The "started" lines say nothing the events do not.
            if !line.contains("helper started") {
                self.log.push(line.to_owned());
            }
        }

        fn now_ms(&mut self) -> u64 {
            self.now
        }
    }

    #[test]
    fn the_wait_is_one_two_four_then_eight_seconds_and_stays_there() {
        let got: Vec<u64> = (0..7).map(wait_ms).collect();
        assert_eq!(got, [1000, 2000, 4000, 8000, 8000, 8000, 8000]);
        assert_eq!(wait_ms(u32::MAX), CAP_WAIT_MS);
    }

    #[test]
    fn a_helper_that_dies_is_forked_again_and_a_dead_one_is_never_given_up_on() {
        // Six deaths in a row, each soon after its start: the waits back off to the cap and stay.
        let mut fake = Fake::new(vec![(ChildEnd::Signal(31), 10); 6]);
        supervise(&mut fake, Some(6));
        assert_eq!(fake.pauses, [1000, 2000, 4000, 8000, 8000, 8000]);
        assert_eq!(
            fake.events
                .iter()
                .filter(|e| e.starts_with("start"))
                .count(),
            6
        );
        // The pids are new each time, and the leftovers are thrown away before each wait.
        assert_eq!(
            &fake.events[..4],
            ["start 101", "wait 101", "clean", "start 102"]
        );
        assert!(
            fake.log[0].contains("(pid 101) was killed by signal 31"),
            "{:?}",
            fake.log
        );
        assert!(
            fake.log[0].contains("starting it again in 1000 ms (restart 1)"),
            "{:?}",
            fake.log
        );
        assert!(
            fake.log[3].contains("in 8000 ms (restart 4)"),
            "{:?}",
            fake.log
        );
    }

    #[test]
    fn a_helper_that_ran_long_starts_the_wait_over() {
        let mut fake = Fake::new(vec![
            (ChildEnd::Exit(1), 10),
            (ChildEnd::Exit(1), 10),
            (ChildEnd::Signal(9), HEALTHY_MS + 1),
            (ChildEnd::Exit(1), 10),
        ]);
        supervise(&mut fake, Some(4));
        assert_eq!(fake.pauses, [1000, 2000, 1000, 2000]);
    }

    #[test]
    fn a_fork_that_fails_is_tried_again_on_the_same_backoff() {
        let mut fake = Fake::new(vec![(ChildEnd::Exit(1), 10), (ChildEnd::Exit(0), 10)]);
        fake.starts_failing = 2;
        supervise(&mut fake, None);
        assert_eq!(fake.pauses, [1000, 2000, 4000]);
        assert!(fake.log[0].contains("cannot be started"), "{:?}", fake.log);
        // The helper that did start ended with 1, and was started again after that.
        assert_eq!(
            fake.events
                .iter()
                .filter(|e| e.starts_with("start 1"))
                .count(),
            2
        );
    }

    #[test]
    fn a_helper_that_ends_clean_is_the_daemons_end_closing_and_the_parent_ends_too() {
        let mut fake = Fake::new(vec![(ChildEnd::Exit(0), 5)]);
        supervise(&mut fake, None);
        assert!(fake.pauses.is_empty());
        assert_eq!(fake.events, ["start 101", "wait 101"]);
    }

    #[test]
    fn a_wait_that_fails_is_a_death_like_any_other() {
        let mut fake = Fake::new(vec![]);
        fake.endings
            .push_back((Err(io::Error::from_raw_os_error(10)), 3));
        fake.endings.push_back((Ok(ChildEnd::Exit(0)), 3));
        supervise(&mut fake, None);
        assert_eq!(fake.pauses, [1000]);
        assert!(
            fake.log[0].contains("cannot be waited for"),
            "{:?}",
            fake.log
        );
    }

    #[test]
    fn the_filter_allows_what_the_helper_does_and_the_parent_forks_with() {
        let parent = calls();
        for call in helper::calls() {
            assert!(
                parent.contains(&call),
                "the helper's call {call} is not in the parent's list"
            );
        }
        for call in [
            nr::CLONE,
            nr::WAIT4,
            nr::CLOCK_NANOSLEEP,
            nr::PRCTL,
            nr::CAPSET,
        ] {
            assert!(parent.contains(&call), "{call}");
        }
        // Not a way to start another program, or to reach the network.
        assert!(!parent.contains(&(libc_execve())));
    }

    fn libc_execve() -> i64 {
        59
    }

    #[test]
    fn the_parents_cleaning_runs_under_its_own_filter() {
        // `clean` reads the socket (std: `recvfrom`) and switches it with `fcntl`: a call the
        // parent's list lacked once, which killed the parent at the first death of a helper.
        let (mut daemon, parent) = UnixStream::pair().unwrap();
        daemon.write_all(b"setup 0 1\n").unwrap();
        let exit = seccomp::run_forked(|| {
            let mut system = System {
                stream: parent,
                gadgets: &[],
                daemon_pid: 1,
                began: Instant::now(),
            };
            if seccomp::install(&calls()).is_err() {
                return 2;
            }
            system.clean();
            system.log("lemnosd: the parent logs under its filter");
            let _ = system.now_ms();
            system.pause(1);
            0
        })
        .unwrap();
        assert_eq!(exit, seccomp::Exit::Code(0));
    }

    /// An ops that does nothing and says ok.
    struct Nothing;
    impl helper::Ops for Nothing {
        fn setup(&mut self, _: &Gadget) -> io::Result<lemnos_linux::SerialString> {
            Ok(lemnos_linux::SerialString::Absent)
        }
        fn teardown(&mut self, _: &Gadget) -> io::Result<()> {
            Ok(())
        }
        fn bind(&mut self, _: &Gadget) -> io::Result<()> {
            Ok(())
        }
        fn unbind(&mut self, _: &Gadget) -> io::Result<()> {
            Ok(())
        }
    }

    struct Anyone;
    impl helper::Holder for Anyone {
        fn check(&self, _: &UnixStream) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn a_helper_forked_by_a_parent_under_the_parents_filter_installs_its_own_and_serves() {
        // The two filters stack, as in production (without the drop, which needs root): the parent
        // installs its list, forks, and the child names itself, ignores signals, installs the helper's
        // list and serves one request. Every call of the fork and of the start-up must be in the
        // parent's list; a call that is not kills the child, and this test says which step.
        let (mut daemon, helper_end) = UnixStream::pair().unwrap();
        daemon.write_all(b"bind 0 1\n").unwrap();
        daemon.shutdown(std::net::Shutdown::Write).unwrap();
        let gadget = Gadget {
            udc: "udc0".into(),
            owner: lemnos_linux::Owner { uid: 1, gid: 1 },
            board: "b".into(),
            config: Vec::new(),
            identity: None,
        };
        let exit = seccomp::run_forked(|| {
            let stream = helper_end;
            if seccomp::install(&calls()).is_err() {
                return 2;
            }
            let child_stream = match stream.try_clone() {
                Ok(s) => s,
                Err(_) => return 3,
            };
            let Ok(pid) = privilege::fork() else { return 4 };
            if pid == 0 {
                let mut stream = child_stream;
                let mut ok = privilege::set_name(helper::PROCESS_NAME).is_ok();
                ok &= privilege::die_with_parent().is_ok();
                for signal in [1, 2, 15] {
                    ok &= privilege::ignore_signal(signal).is_ok();
                }
                if !ok || seccomp::install(&helper::calls()).is_err() {
                    std::process::exit(5);
                }
                let gadgets = [gadget];
                let code = match helper::serve(&mut stream, &gadgets, &mut Nothing, &Anyone) {
                    Ok(()) => 0,
                    Err(_) => 6,
                };
                std::process::exit(code);
            }
            drop(stream);
            match privilege::wait_child(pid) {
                Ok(ChildEnd::Exit(0)) => 0,
                Ok(ChildEnd::Exit(code)) => 10 + code,
                Ok(ChildEnd::Signal(signal)) => 100 + signal,
                Err(_) => 7,
            }
        })
        .unwrap();
        assert_eq!(
            exit,
            seccomp::Exit::Code(0),
            "100 + N is the helper killed by signal N"
        );
        let mut reply = String::new();
        daemon.read_to_string(&mut reply).unwrap();
        assert_eq!(reply, "1 ok\n");
    }

    #[test]
    fn what_a_dead_helper_never_read_is_thrown_away_and_the_socket_is_blocking_again() {
        use std::os::unix::net::UnixStream;
        let (mut daemon, parent) = UnixStream::pair().unwrap();
        daemon.write_all(b"setup 0 1\nbind 0 2\n").unwrap();
        let mut system = System {
            stream: parent,
            gadgets: &[],
            daemon_pid: 1,
            began: Instant::now(),
        };
        system.clean();
        // Nothing is left to read, and a read now waits (blocking mode is back) instead of failing.
        system
            .stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let mut byte = [0u8; 1];
        let error = system.stream.read(&mut byte).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
        // A request after the clean-up is the next helper's.
        daemon.write_all(b"unbind 0 3\n").unwrap();
        system.stream.set_read_timeout(None).unwrap();
        let mut line = [0u8; 11];
        system.stream.read_exact(&mut line).unwrap();
        assert_eq!(&line, b"unbind 0 3\n");
    }
}
