//! A helper on a socket for other crates' tests: the real [`serve`] over a real Unix socket, with a
//! service manager whose answers the test chooses. What the caller sees is what the packaged helper
//! sends; only the service manager behind it is chosen, including the ways it fails.

use crate::serve::{Pid1, Policy, Watch, serve};
use std::io::BufReader;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// What the stand-in's service manager does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Manager {
    /// The reboot target loads, the reboot starts and the system is stopping at once.
    Stops,
    /// The check is refused with this reason (the reboot target does not load).
    Refuses(String),
    /// The check passes and starting the reboot is refused with this reason.
    StartFails(String),
    /// The reboot starts and the system never starts stopping: failed at the bound.
    NeverStops,
}

struct Chosen {
    manager: Manager,
    requests: Arc<Mutex<Vec<&'static str>>>,
}

impl Pid1 for Chosen {
    fn check(&mut self) -> Result<(), String> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("check");
        match &self.manager {
            Manager::Refuses(why) => Err(why.clone()),
            _ => Ok(()),
        }
    }

    fn start_reboot(&mut self) -> Result<String, String> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("start");
        match &self.manager {
            Manager::StartFails(why) => Err(why.clone()),
            _ => Ok("/org/freedesktop/systemd1/job/1".into()),
        }
    }

    fn system_state(&mut self) -> Result<String, String> {
        Ok(match self.manager {
            Manager::Stops => "stopping",
            _ => "running",
        }
        .into())
    }
}

/// A helper serving at `<dir>/restart.sock` until the test ends. `requests` lists what its service
/// manager was asked, in order (`check`, `start`).
pub struct StandIn {
    pub socket: PathBuf,
    pub requests: Arc<Mutex<Vec<&'static str>>>,
}

impl StandIn {
    /// Starts serving: every connection is admitted as root, and a reboot that never stops is
    /// failed after `bound`.
    pub fn start(dir: &Path, manager: Manager, bound: Duration) -> std::io::Result<Self> {
        let socket = dir.join("restart.sock");
        let listener = UnixListener::bind(&socket)?;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { return };
                let Ok(writer) = stream.try_clone() else {
                    return;
                };
                let mut conn = Duplex {
                    reader: BufReader::new(stream),
                    writer,
                };
                let mut pid1 = Chosen {
                    manager: manager.clone(),
                    requests: seen.clone(),
                };
                let watch = Watch {
                    bound,
                    every: Duration::from_millis(10),
                };
                let _ = serve(
                    &mut conn,
                    0,
                    Policy { allowed_uid: 0 },
                    &mut pid1,
                    watch,
                    &mut || false,
                );
            }
        });
        Ok(Self { socket, requests })
    }

    /// What the service manager was asked, in order.
    pub fn asked(&self) -> Vec<&'static str> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

struct Duplex {
    reader: BufReader<std::os::unix::net::UnixStream>,
    writer: std::os::unix::net::UnixStream,
}

impl std::io::Read for Duplex {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl std::io::BufRead for Duplex {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.reader.fill_buf()
    }
    fn consume(&mut self, n: usize) {
        self.reader.consume(n);
    }
}

impl std::io::Write for Duplex {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writer.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}
