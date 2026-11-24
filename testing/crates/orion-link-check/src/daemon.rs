//! The daemon as its own process in the guest: `lemnosd`, started as root on a board file, dropped to
//! the daemon's user by its own privileged start. The restart step stops it as the service manager's
//! restart does (SIGTERM to the process this check started, by its PID) and starts it again;
//! otherwise the guest's power-off ends it (this check runs last, so nothing after it minds).

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lemnos_client::{Error, Session};

use crate::manager::Manager;
use crate::{GADGET_DIR, UDC};

/// SIGKILL.
const KILL: i32 = 9;

/// The user and group the daemon drops to. Numbers, so no account database is needed.
const DAEMON_ID: &str = "999";
/// How long the daemon has to say it is ready.
const READY_WAIT: Duration = Duration::from_secs(20);
/// The unit's stop bound (`TimeoutStopSec=5s`) and restart delay (`RestartSec=1s`),
/// packaging/systemd/lemnosd.service.
const STOP_BOUND: Duration = Duration::from_secs(5);
const RESTART_DELAY: Duration = Duration::from_secs(1);

pub struct Lemnosd {
    socket: PathBuf,
    /// How it was started, for a restart.
    program: PathBuf,
    board: PathBuf,
    /// The service manager it runs under: its notify socket and file descriptor store.
    manager: Arc<Manager>,
    /// The daemon this check started. Stopped only by [`Lemnosd::restart`] and [`Lemnosd::kill_at`].
    child: Child,
    /// The lines it logged so far, each with when it came (each is on the console too). A restart
    /// keeps them: the new daemon's lines follow the old one's.
    lines: Arc<Mutex<Vec<(Instant, String)>>>,
    /// How many `READY=1` the manager had when the running start was ready.
    ready_mark: u64,
}

/// Where [`Lemnosd::kill_at`] kills a start of the daemon: right after the start-trace line that
/// ends a step, or after it is ready.
#[derive(Clone, Copy, Debug)]
pub enum KillAt {
    /// The first line of the start (`exec`): the root phase under way.
    Exec,
    /// The root phase's end, the process stopped there by its own stop point
    /// (`LEMNOSD_STOP_AFTER=root-phase`): the kept files taken, nothing bound.
    RootPhase,
    /// The host's start begins: the gadgets adopted, no device bound.
    HostStart,
    /// The gadget's bind begins.
    BindBegins,
    /// The gadget's bind ended, the process stopped there (`LEMNOSD_STOP_AFTER=bind:gadget`).
    Bind,
    /// Every bind made.
    HostStarted,
    /// Just after `READY=1`.
    Ready,
}

impl KillAt {
    pub const ALL: [Self; 7] = [
        Self::Exec,
        Self::RootPhase,
        Self::HostStart,
        Self::BindBegins,
        Self::Bind,
        Self::HostStarted,
        Self::Ready,
    ];

    /// The daemon's own stop point for this step, when it has one.
    fn stop_after(self) -> Option<&'static str> {
        match self {
            Self::RootPhase => Some("root-phase"),
            Self::Bind => Some("bind:gadget"),
            _ => None,
        }
    }

    /// The start-trace event this kill follows, or `None` for [`KillAt::Ready`].
    fn event(self) -> Option<&'static str> {
        match self {
            Self::Exec => Some("exec"),
            Self::RootPhase => Some("root phase done"),
            Self::HostStart => Some("host start begins"),
            Self::BindBegins => Some("bind gadget (usb-gadget) begins"),
            Self::Bind => Some("bind gadget ok"),
            Self::HostStarted => Some("host started"),
            Self::Ready => None,
        }
    }
}

impl Lemnosd {
    /// Starts the daemon under `manager` and waits until it says it is ready (its socket listens).
    pub fn start(
        program: &Path,
        board: &Path,
        socket: &Path,
        manager: Arc<Manager>,
    ) -> Result<Self, String> {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let child = spawn(program, board, socket, &manager, &lines, None)?;
        let mut daemon = Self {
            socket: socket.to_path_buf(),
            program: program.to_path_buf(),
            board: board.to_path_buf(),
            manager,
            child,
            lines,
            ready_mark: 0,
        };
        daemon.wait_ready()?;
        Ok(daemon)
    }

    /// Stops the daemon as `systemctl restart` does (SIGTERM, then the unit's 5 s stop bound, then
    /// SIGKILL), waits the unit's 1 s restart delay and starts it again. When it stopped, and how
    /// (`Ok(true)`: on the SIGTERM; `Ok(false)`: only the SIGKILL ended it).
    pub fn restart(&mut self, stopped: impl FnOnce()) -> Result<(Instant, bool), String> {
        let pid = self.pid()?;
        let stopped_at = Instant::now();
        lemnos_linux_sys::process::terminate(pid)
            .map_err(|error| format!("SIGTERM to the daemon ({pid}): {error}"))?;
        let mut clean = false;
        let until = stopped_at + STOP_BOUND;
        while Instant::now() < until {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                clean = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !clean {
            // Past the unit's stop bound: the service manager kills it. The child is this check's own.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // Between the stop and the start: what the old daemon left.
        stopped();
        self.start_again()?;
        Ok((stopped_at, clean))
    }

    /// Kills the running daemon (SIGKILL, as a crash), then starts it again as the unit's restart
    /// does, kills that start at `at`, and starts it once more, to ready. When the first kill was,
    /// and the last start-trace line the killed start logged.
    pub fn kill_at(&mut self, at: KillAt) -> Result<(Instant, String), String> {
        let killed_at = Instant::now();
        self.kill_now()?;
        std::thread::sleep(RESTART_DELAY);
        // A start killed at its ready still counts it: the next wait is for the one after.
        self.ready_mark = self.manager.ready();
        let started = Instant::now();
        self.child = self.spawn(at.stop_after())?;
        let reached = || match at.event() {
            Some(event) => self.traced_since(started, event),
            None => self.manager.ready() > self.ready_mark,
        };
        if !wait_until(READY_WAIT, reached) {
            return Err(format!("the start never reached {at:?}"));
        }
        self.kill_now()?;
        self.ready_mark = self.manager.ready();
        let last = self
            .lines_since(started, "start trace:")
            .last()
            .map_or_else(
                || "no start-trace line".to_string(),
                |(_, line)| line.clone(),
            );
        self.start_again()?;
        Ok((killed_at, last))
    }

    /// SIGKILL to the daemon's process (the one this check started, by its PID), and waits for it.
    fn kill_now(&mut self) -> Result<(), String> {
        let pid = self.pid()?;
        lemnos_linux_sys::guest::kill(pid, KILL)
            .map_err(|error| format!("SIGKILL to the daemon ({pid}): {error}"))?;
        let _ = self.child.wait();
        Ok(())
    }

    /// Waits the unit's restart delay and starts the daemon again, to ready.
    fn start_again(&mut self) -> Result<(), String> {
        std::thread::sleep(RESTART_DELAY);
        self.child = self.spawn(None)?;
        self.wait_ready()
    }

    fn spawn(&self, stop_after: Option<&str>) -> Result<Child, String> {
        spawn(
            &self.program,
            &self.board,
            &self.socket,
            &self.manager,
            &self.lines,
            stop_after,
        )
    }

    fn wait_ready(&mut self) -> Result<(), String> {
        let past = self.ready_mark;
        self.manager.wait_ready(past, &mut self.child, READY_WAIT)?;
        self.ready_mark = self.manager.ready();
        Ok(())
    }

    fn pid(&self) -> Result<i32, String> {
        self.daemon_pid()
            .and_then(|pid| pid.parse::<i32>().ok())
            .ok_or_else(|| "the daemon's process is not found".to_string())
    }

    /// The lines logged since `since` that contain `text`, each with its time after `since`.
    pub fn lines_since(&self, since: Instant, text: &str) -> Vec<(Duration, String)> {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(at, line)| *at >= since && line.contains(text))
            .map(|(at, line)| (at.duration_since(since), line.clone()))
            .collect()
    }

    /// Whether the daemon logged the start-trace event `event` since `since`: a line
    /// `start trace: <stamp> <event>`, the event whole or followed by its details. Only the
    /// daemon's trace lines count (the sandbox's `lemnos-unit-check exec:` line is not `exec`).
    fn traced_since(&self, since: Instant, event: &str) -> bool {
        self.lines_since(since, "start trace: ")
            .iter()
            .any(|(_, line)| traced(line, event))
    }

    /// How many lines logged so far contain `text`.
    pub fn count(&self, text: &str) -> usize {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, line)| line.contains(text))
            .count()
    }

    /// How many lines logged so far contain both `one` and `other`.
    pub fn count_both(&self, one: &str, other: &str) -> usize {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, line)| line.contains(one) && line.contains(other))
            .count()
    }

    /// Whether `text` has been logged at least `times` times within `within`.
    pub fn wait_for(&self, text: &str, times: usize, within: Duration) -> bool {
        wait_until(within, || self.count(text) >= times)
    }

    /// The daemon's process: the one this check started (the sandbox start execs the daemon) or,
    /// if that is not the daemon, its child that is.
    fn daemon_pid(&self) -> Option<String> {
        let comm = |pid: &str| {
            fs::read_to_string(format!("/proc/{pid}/comm")).map(|text| text.trim().to_string())
        };
        let started = self.child.id().to_string();
        if comm(&started).is_ok_and(|name| name == "lemnosd") {
            Some(started)
        } else {
            fs::read_dir("/proc").ok().and_then(|entries| {
                entries.flatten().find_map(|entry| {
                    let pid = entry.file_name().to_string_lossy().into_owned();
                    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                    let parent = stat.rsplit_once(") ")?.1.split(' ').nth(1)?.to_string();
                    (parent == started && comm(&pid).is_ok_and(|name| name == "lemnosd"))
                        .then_some(pid)
                })
            })
        }
    }

    /// The ids of the daemon's lane threads, which run its drivers' calls and ticks (named
    /// `lemnos-lane-N` by the host).
    pub fn lane_threads(&self) -> Vec<i32> {
        let comm = |pid: &str| {
            fs::read_to_string(format!("/proc/{pid}/comm")).map(|text| text.trim().to_string())
        };
        let pid = self.daemon_pid();
        let Some(pid) = pid else {
            return Vec::new();
        };
        let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
            return Vec::new();
        };
        tasks
            .flatten()
            .filter_map(|task| {
                let tid = task.file_name().to_string_lossy().into_owned();
                let name = comm(&format!("{pid}/task/{tid}")).ok()?;
                if name.starts_with("lemnos-lane-") {
                    tid.parse().ok()
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn connect(&self, name: &str) -> Result<Session, Error> {
        Session::connect(&self.socket, name)
    }
}

/// Whether the gadget is bound to the dummy controller.
pub fn enabled() -> bool {
    fs::read_to_string(format!("{GADGET_DIR}/UDC")).is_ok_and(|text| text.trim() == UDC)
}

/// Asks `ready` until it holds, or `within` runs out. Whether it held.
/// Whether `line` is the start-trace line of `event`: `start trace: <stamp> <event>`, the event
/// whole, or followed by a space or a colon and its details.
fn traced(line: &str, event: &str) -> bool {
    line.split_once("start trace: ")
        .and_then(|(_, rest)| rest.split_once(' '))
        .and_then(|(_, what)| what.strip_prefix(event))
        .is_some_and(|after| after.is_empty() || after.starts_with([' ', ':']))
}

pub fn wait_until(within: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + within;
    loop {
        if ready() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Starts the daemon once, as the packaged unit starts it, under `manager`, its lines kept in
/// `lines`. With `stop_after`, the daemon stops itself there (`LEMNOSD_STOP_AFTER`).
fn spawn(
    program: &Path,
    board: &Path,
    socket: &Path,
    manager: &Manager,
    lines: &Arc<Mutex<Vec<(Instant, String)>>>,
    stop_after: Option<&str>,
) -> Result<Child, String> {
    let _ = fs::remove_file(socket);
    // Started as the packaged unit with the gadget drop-in starts it: root with only the unit's
    // bounding set, its system call filter and its file system sandbox (lemnos-unit-check exec),
    // then the daemon's own drop to --user. The socket's directory stands for /run/lemnos.
    let mut command = Command::new(program.with_file_name("lemnos-unit-check"));
    command
        .args(["exec", "--with", "gadget", "--runtime-dir"])
        .arg(socket.parent().unwrap_or(Path::new("/run")))
        .arg("--")
        .arg(program)
        .arg("--board")
        .arg(board)
        .arg("--socket")
        .arg(socket)
        .arg("--user")
        .arg(DAEMON_ID)
        .arg("--group")
        .arg(DAEMON_ID)
        .env("LEMNOSD_START_TRACE", "1");
    if let Some(point) = stop_after {
        command.env("LEMNOSD_STOP_AFTER", point);
    }
    manager.give(&mut command)?;
    command.stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("start lemnosd: {error}"))?;
    let stderr = child.stderr.take().ok_or("no stderr")?;
    let kept = Arc::clone(lines);
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            say!("     lemnosd: {line}");
            kept.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((Instant::now(), line));
        }
    });
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::{KillAt, traced};

    /// Lines as the daemon and its sandbox logged them in a guest run.
    const LINES: [&str; 9] = [
        "lemnos-unit-check exec: ProtectSystem=strict, ProtectKernelTunables=yes: 13 mounts read-only",
        "lemnosd: start trace: 175.527908 exec",
        "lemnosd: start trace: 175.601000 root phase done (the drop included, when asked)",
        "lemnosd: start trace: 175.700000 host start begins: binds in board order",
        "lemnosd: start trace: 175.710000 bind identity (system-identity) begins on lane device:identity",
        "lemnosd: start trace: 175.720000 bind gadget (usb-gadget) begins on lane device:gadget",
        "lemnosd: start trace: 175.800000 bind gadget ok",
        "lemnosd: start trace: 175.900000 host started: every bind made",
        "lemnosd: start trace: 176.000000 SLOT-TICK device gadget us 36",
    ];

    #[test]
    fn each_kill_point_matches_its_own_trace_line_and_no_other() {
        for at in KillAt::ALL {
            let Some(event) = at.event() else { continue };
            let hits: Vec<&str> = LINES
                .into_iter()
                .filter(|line| traced(line, event))
                .collect();
            assert_eq!(hits.len(), 1, "{at:?} ({event}) matched {hits:?}");
            assert!(hits[0].contains("start trace:"), "{at:?} matched {hits:?}");
        }
    }

    #[test]
    fn the_sandboxs_exec_line_is_not_the_daemons_exec() {
        assert!(!traced(LINES[0], "exec"));
        assert!(traced(LINES[1], "exec"));
        assert!(!traced("lemnosd: start trace: 1.0 executed", "exec"));
    }
}
