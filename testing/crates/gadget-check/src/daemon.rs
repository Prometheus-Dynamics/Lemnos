//! The daemon as its own process in the guest: `lemnosd`, started by this check on its board file,
//! dropped to the daemon's user by its own privileged start, and stopped by the PID this check
//! recorded when it started it. The check never stops anything it did not start.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use lemnos_client::{Error, Session};

use crate::{GADGET_DIR, UDC};

/// The user and group the daemon drops to. Numbers, so no account database is needed.
pub const DAEMON_ID: &str = "999";
/// How long the daemon has to make its socket.
const SOCKET_WAIT: Duration = Duration::from_secs(20);

/// One `lemnosd` process this check started, with the board and socket it runs on.
pub struct Lemnosd {
    program: PathBuf,
    board: PathBuf,
    socket: PathBuf,
    child: Option<Child>,
}

impl Lemnosd {
    /// Starts the daemon as root, which its privileged start needs, and waits for its socket.
    pub fn start(program: &Path, board: &Path, socket: &Path) -> Result<Self, String> {
        let mut daemon = Self {
            program: program.to_path_buf(),
            board: board.to_path_buf(),
            socket: socket.to_path_buf(),
            child: None,
        };
        daemon.spawn()?;
        Ok(daemon)
    }

    fn spawn(&mut self) -> Result<(), String> {
        // The socket is the one this check's last daemon made, and that daemon is gone (or killed):
        // the new daemon binds the path itself.
        let _ = fs::remove_file(&self.socket);
        let child = Command::new(&self.program)
            .arg("--board")
            .arg(&self.board)
            .arg("--socket")
            .arg(&self.socket)
            .arg("--user")
            .arg(DAEMON_ID)
            .arg("--group")
            .arg(DAEMON_ID)
            .spawn()
            .map_err(|error| format!("start lemnosd: {error}"))?;
        self.child = Some(child);
        let until = Instant::now() + SOCKET_WAIT;
        loop {
            if self.socket.exists() {
                return Ok(());
            }
            if let Some(child) = self.child.as_mut()
                && let Some(status) = child
                    .try_wait()
                    .map_err(|error| format!("poll lemnosd: {error}"))?
            {
                return Err(format!("lemnosd exited before its socket: {status}"));
            }
            if Instant::now() >= until {
                return Err("lemnosd made no socket".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The PID of the daemon this check started, while it runs.
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// Kills the daemon this check started, by the PID it recorded, and reaps it. A daemon that is
    /// already gone is not an error here.
    pub fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Kills the running daemon and starts it again on the same board and socket.
    pub fn restart(&mut self) -> Result<(), String> {
        self.kill();
        self.spawn()
    }

    /// A client connection to the daemon's socket.
    pub fn connect(&self) -> Result<Session, Error> {
        Session::connect(&self.socket, "gadget-check")
    }
}

impl Drop for Lemnosd {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Whether the gadget is bound to the dummy controller: configfs names it in the `UDC` attribute.
pub fn enabled() -> bool {
    fs::read_to_string(format!("{GADGET_DIR}/UDC")).is_ok_and(|text| text.trim() == UDC)
}

/// Asks `ready` until it holds, or `within` runs out. Whether it held.
pub fn wait_until(within: Duration, ready: impl Fn() -> bool) -> bool {
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
