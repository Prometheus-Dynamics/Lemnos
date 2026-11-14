//! The daemon as its own process in the guest: `lemnosd`, started as root on a board file, dropped to
//! the daemon's user by its own privileged start. Nothing here stops it: the guest powers off when
//! the checks end, which is what ends it (this check runs last, so nothing after it minds).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use lemnos_client::{Error, Session};

use crate::{GADGET_DIR, UDC};

/// The user and group the daemon drops to. Numbers, so no account database is needed.
const DAEMON_ID: &str = "999";
const SOCKET_WAIT: Duration = Duration::from_secs(20);

pub struct Lemnosd {
    socket: PathBuf,
    child: Option<Child>,
}

impl Lemnosd {
    /// Starts the daemon and waits for its socket.
    pub fn start(program: &Path, board: &Path, socket: &Path) -> Result<Self, String> {
        let _ = fs::remove_file(socket);
        let child = Command::new(program)
            .arg("--board")
            .arg(board)
            .arg("--socket")
            .arg(socket)
            .arg("--user")
            .arg(DAEMON_ID)
            .arg("--group")
            .arg(DAEMON_ID)
            .spawn()
            .map_err(|error| format!("start lemnosd: {error}"))?;
        let mut daemon = Self {
            socket: socket.to_path_buf(),
            child: Some(child),
        };
        let until = Instant::now() + SOCKET_WAIT;
        loop {
            if socket.exists() {
                return Ok(daemon);
            }
            if let Some(child) = daemon.child.as_mut()
                && let Some(status) = child
                    .try_wait()
                    .map_err(|error| format!("poll lemnosd: {error}"))?
            {
                daemon.child = None;
                return Err(format!("lemnosd exited before its socket: {status}"));
            }
            if Instant::now() >= until {
                return Err("lemnosd made no socket".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
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
