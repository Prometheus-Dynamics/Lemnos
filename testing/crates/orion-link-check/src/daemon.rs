//! The daemon as its own process in the guest: `lemnosd`, started as root on a board file, dropped to
//! the daemon's user by its own privileged start. Nothing here stops it: the guest powers off when
//! the checks end, which is what ends it (this check runs last, so nothing after it minds).

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lemnos_client::{Error, Session};
use lemnos_daemon_ready::Ready;

use crate::{GADGET_DIR, UDC};

/// The user and group the daemon drops to. Numbers, so no account database is needed.
const DAEMON_ID: &str = "999";
/// How long the daemon has to say it is ready.
const READY_WAIT: Duration = Duration::from_secs(20);

pub struct Lemnosd {
    socket: PathBuf,
    /// The daemon this check started. Held, never stopped here: the power-off ends it.
    child: Child,
    /// The lines it logged so far (each is on the console too).
    lines: Arc<Mutex<Vec<String>>>,
}

impl Lemnosd {
    /// Starts the daemon and waits until it says it is ready (its socket listens).
    pub fn start(program: &Path, board: &Path, socket: &Path) -> Result<Self, String> {
        let _ = fs::remove_file(socket);
        let ready = Ready::new()?;
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
            .arg(DAEMON_ID);
        ready.give(&mut command);
        command.stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| format!("start lemnosd: {error}"))?;
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let lines = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&lines);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                say!("     lemnosd: {line}");
                kept.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(line);
            }
        });
        ready.wait(&mut child, READY_WAIT)?;
        Ok(Self {
            socket: socket.to_path_buf(),
            child,
            lines,
        })
    }

    /// How many lines logged so far contain `text`.
    pub fn count(&self, text: &str) -> usize {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|line| line.contains(text))
            .count()
    }

    /// How many lines logged so far contain both `one` and `other`.
    pub fn count_both(&self, one: &str, other: &str) -> usize {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|line| line.contains(one) && line.contains(other))
            .count()
    }

    /// Whether `text` has been logged at least `times` times within `within`.
    pub fn wait_for(&self, text: &str, times: usize, within: Duration) -> bool {
        wait_until(within, || self.count(text) >= times)
    }

    /// The ids of the daemon's lane threads, which run its drivers' calls and ticks (named
    /// `lemnos-lane-N` by the host). The process is the one this check started (the sandbox start
    /// execs the daemon) or, if that is not the daemon, its child that is.
    pub fn lane_threads(&self) -> Vec<i32> {
        let comm = |pid: &str| {
            fs::read_to_string(format!("/proc/{pid}/comm")).map(|text| text.trim().to_string())
        };
        let started = self.child.id().to_string();
        let pid = if comm(&started).is_ok_and(|name| name == "lemnosd") {
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
        };
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
