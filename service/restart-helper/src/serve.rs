//! The helper's side of one connection: who may ask, reading the request, and carrying it out
//! against the service manager.

use crate::dbus::{CallError, Manager};
use crate::proto::{self, Answer, Request, Verb};
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The target a reboot starts, and the job mode: the reboot cannot be cancelled once queued.
pub const REBOOT_TARGET: &str = "reboot.target";
pub const REBOOT_MODE: &str = "replace-irreversibly";

/// What the helper needs from the service manager.
pub trait Pid1 {
    /// Whether a reboot would be accepted: the reboot target loads. `Err` says why not.
    fn check(&mut self) -> Result<(), String>;
    /// Starts the reboot: the job's path, or why it did not start.
    fn start_reboot(&mut self) -> Result<String, String>;
    /// The manager's system state (`running`, `stopping`, ...).
    fn system_state(&mut self) -> Result<String, String>;
}

/// The service manager at its private socket, a new connection per step, as uid 0.
pub struct Systemd {
    pub socket: PathBuf,
}

impl Systemd {
    fn connect(&self) -> Result<Manager, String> {
        Manager::connect(&self.socket, 0).map_err(|e| format!("the service manager: {e}"))
    }
}

fn said(e: CallError) -> String {
    format!("the service manager: {e}")
}

impl Pid1 for Systemd {
    fn check(&mut self) -> Result<(), String> {
        let mut m = self.connect()?;
        let unit = m.load_unit(REBOOT_TARGET).map_err(said)?;
        let state = m
            .string_property(&unit, "org.freedesktop.systemd1.Unit", "LoadState")
            .map_err(said)?;
        if state != "loaded" {
            return Err(format!("the service manager has {REBOOT_TARGET} {state}"));
        }
        Ok(())
    }

    fn start_reboot(&mut self) -> Result<String, String> {
        self.connect()?
            .start_unit(REBOOT_TARGET, REBOOT_MODE)
            .map_err(said)
    }

    fn system_state(&mut self) -> Result<String, String> {
        self.connect()?.system_state().map_err(said)
    }
}

/// BusyBox's init, which restarts the board on `SIGTERM`. It has no state to read: the helper's
/// own `SIGTERM`, which init sends every process as it stops, is the system stopping.
pub struct InitSignal;

impl Pid1 for InitSignal {
    fn check(&mut self) -> Result<(), String> {
        lemnos_linux_sys::process::may_signal(1)
            .map_err(|e| format!("the init process can't be signalled: {e}"))
    }

    fn start_reboot(&mut self) -> Result<String, String> {
        lemnos_linux_sys::process::terminate(1)
            .map(|()| "SIGTERM to process 1".to_string())
            .map_err(|e| format!("the init process can't be signalled: {e}"))
    }

    fn system_state(&mut self) -> Result<String, String> {
        Ok("running (init gives no state; its stop reaches this helper as SIGTERM)".to_string())
    }
}

/// Who may ask: root, and the one user the unit names (the daemon's). Membership of the socket's
/// group only lets a process connect; the helper answers it by its uid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub allowed_uid: u32,
}

impl Policy {
    /// `Err` with the reason when `uid` may not ask.
    pub fn admit(&self, uid: u32) -> Result<(), String> {
        if uid == 0 || uid == self.allowed_uid {
            Ok(())
        } else {
            Err(format!(
                "uid {uid} may not restart this board: only uid {} (the daemon's user) and root may",
                self.allowed_uid
            ))
        }
    }
}

/// How long the helper watches for the system to start stopping after it started the reboot, and
/// how often it looks.
#[derive(Debug, Clone, Copy)]
pub struct Watch {
    pub bound: Duration,
    pub every: Duration,
}

impl Default for Watch {
    fn default() -> Self {
        Self {
            bound: Duration::from_secs(20),
            every: Duration::from_millis(100),
        }
    }
}

/// Reads the request line: at most [`proto::MAX_REQUEST`] bytes up to a newline.
fn read_request<R: BufRead>(input: &mut R) -> Result<Request, String> {
    let mut line = Vec::with_capacity(proto::MAX_REQUEST);
    let mut limited = input.by_ref().take(proto::MAX_REQUEST as u64);
    limited
        .read_until(b'\n', &mut line)
        .map_err(|e| format!("the request could not be read: {e}"))?;
    if line.pop() != Some(b'\n') {
        return Err(format!(
            "a request is one line of at most {} bytes",
            proto::MAX_REQUEST
        ));
    }
    let text = String::from_utf8(line).map_err(|_| "the request is not text".to_string())?;
    proto::parse_request(&text)
}

fn send<W: Write>(out: &mut W, nonce: &str, answer: &Answer) -> io::Result<()> {
    out.write_all(proto::answer_line(nonce, answer).as_bytes())?;
    out.flush()
}

/// Serves one connection from a peer of uid `peer_uid`: refuses a peer the policy does not admit,
/// reads one request and carries it out. `stop_seen` says whether this process has been asked to
/// stop (its own `SIGTERM`), which after a started reboot means the system is stopping.
pub fn serve<S: BufRead + Write>(
    conn: &mut S,
    peer_uid: u32,
    policy: Policy,
    pid1: &mut dyn Pid1,
    watch: Watch,
    stop_seen: &mut dyn FnMut() -> bool,
) -> io::Result<()> {
    // The request is read before the policy is applied: a refused caller then gets its answer, with its own nonce
    // when its request parses, and not a closed connection under a request it has already written.
    let request = read_request(conn);
    if let Err(why) = policy.admit(peer_uid) {
        let nonce = request.as_ref().map_or("-", |r| r.nonce.as_str());
        return send(conn, nonce, &Answer::Refused(why));
    }
    let request = match request {
        Ok(r) => r,
        Err(why) => return send(conn, "-", &Answer::Refused(why)),
    };
    let nonce = request.nonce.as_str();
    match request.verb {
        Verb::Check => match pid1.check() {
            Ok(()) => send(conn, nonce, &Answer::Ok),
            Err(why) => send(conn, nonce, &Answer::Refused(why)),
        },
        Verb::Reboot => {
            if let Err(why) = pid1.check() {
                return send(conn, nonce, &Answer::Refused(why));
            }
            send(conn, nonce, &Answer::Accepted)?;
            if let Err(why) = pid1.start_reboot() {
                return send(conn, nonce, &Answer::Failed(why));
            }
            let start = Instant::now();
            loop {
                if stop_seen() {
                    return send(conn, nonce, &Answer::Stopping);
                }
                let last = match pid1.system_state() {
                    Ok(state) if state == "stopping" => {
                        return send(conn, nonce, &Answer::Stopping);
                    }
                    Ok(state) => format!("the system is {state}"),
                    Err(why) => why,
                };
                if start.elapsed() >= watch.bound {
                    return send(
                        conn,
                        nonce,
                        &Answer::Failed(format!(
                            "{REBOOT_TARGET} was started and the system did not start stopping within {} ms: {last}",
                            watch.bound.as_millis()
                        )),
                    );
                }
                std::thread::sleep(watch.every);
            }
        }
    }
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;
