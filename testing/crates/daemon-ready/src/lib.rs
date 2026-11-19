//! Waiting for a `lemnosd` the check started until the daemon says it is ready.
//!
//! The daemon's socket file is no readiness signal: it exists from the socket's `bind`, and a
//! connect made before the `listen` that follows is refused (`std`'s `UnixListener::bind` is
//! `socket`, `bind`, `listen`). The daemon's own signal is `READY=1` on `$NOTIFY_SOCKET`, the one
//! systemd waits for (`Type=notify`): it is sent once the socket listens and every thread of the
//! service is started (`Service::start`). A check gives the daemon a notify socket of its own and
//! waits for that.
//!
//! The socket is in the abstract namespace: no file, so the daemon can send to it after it drops
//! to its own user, and nothing is left behind.

use std::io::ErrorKind;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// How often the wait looks whether the daemon has exited while no message comes.
const LOOK: Duration = Duration::from_millis(50);

/// A notify socket for one daemon start.
pub struct Ready {
    socket: UnixDatagram,
    name: String,
}

impl Ready {
    /// A notify socket with a name no other start in this process or another has.
    pub fn new() -> Result<Self, String> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "lemnos-check-ready-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let addr = SocketAddr::from_abstract_name(name.as_bytes())
            .map_err(|error| format!("the notify socket's name: {error}"))?;
        let socket = UnixDatagram::bind_addr(&addr)
            .map_err(|error| format!("make the notify socket: {error}"))?;
        socket
            .set_read_timeout(Some(LOOK))
            .map_err(|error| format!("the notify socket's timeout: {error}"))?;
        Ok(Self { socket, name })
    }

    /// Gives `command` this socket as its `$NOTIFY_SOCKET`.
    pub fn give(&self, command: &mut Command) {
        command.env("NOTIFY_SOCKET", format!("@{}", self.name));
    }

    /// Waits until the daemon sends `READY=1`. An error if it exits first, or says nothing of it
    /// within `within`.
    pub fn wait(&self, daemon: &mut Child, within: Duration) -> Result<(), String> {
        let until = Instant::now() + within;
        let mut message = [0u8; 256];
        loop {
            match self.socket.recv(&mut message) {
                Ok(n) if says_ready(&message[..n]) => return Ok(()),
                Ok(_) => {}
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("read the notify socket: {error}")),
            }
            if let Some(status) = daemon
                .try_wait()
                .map_err(|error| format!("poll lemnosd: {error}"))?
            {
                return Err(format!("lemnosd exited before it was ready: {status}"));
            }
            if Instant::now() >= until {
                return Err(format!(
                    "lemnosd did not say it was ready (READY=1) within {} s",
                    within.as_secs()
                ));
            }
        }
    }
}

/// Whether a notify message carries `READY=1`: one assignment per line, as systemd reads them.
fn says_ready(message: &[u8]) -> bool {
    message
        .split(|b| *b == b'\n')
        .any(|line| line == b"READY=1")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send(ready: &Ready, message: &str) {
        let to = SocketAddr::from_abstract_name(ready.name.as_bytes()).unwrap();
        UnixDatagram::unbound()
            .unwrap()
            .send_to_addr(message.as_bytes(), &to)
            .unwrap();
    }

    #[test]
    fn ready_is_one_line_of_the_message() {
        assert!(says_ready(b"READY=1"));
        assert!(says_ready(b"STATUS=up\nREADY=1\n"));
        assert!(!says_ready(b"READY=0"));
        assert!(!says_ready(b"STOPPING=1"));
        assert!(!says_ready(b"WATCHDOG=1\nNOT_READY=1"));
    }

    #[test]
    fn ready_ends_the_wait_and_other_messages_do_not() {
        let ready = Ready::new().unwrap();
        let mut daemon = Command::new("sleep").arg("10").spawn().unwrap();
        send(&ready, "WATCHDOG=1");
        assert!(ready.wait(&mut daemon, Duration::from_millis(200)).is_err());
        send(&ready, "READY=1");
        let waited = ready.wait(&mut daemon, Duration::from_secs(5));
        let _ = daemon.kill();
        let _ = daemon.wait();
        assert_eq!(waited, Ok(()));
    }

    #[test]
    fn a_daemon_that_exits_without_ready_fails_the_wait_at_once() {
        let ready = Ready::new().unwrap();
        let mut daemon = Command::new("true").spawn().unwrap();
        let started = Instant::now();
        let waited = ready.wait(&mut daemon, Duration::from_secs(20));
        assert!(waited.is_err_and(|e| e.starts_with("lemnosd exited before it was ready")));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_child_is_given_the_abstract_name_and_its_ready_is_heard() {
        let ready = Ready::new().unwrap();
        let mut command = Command::new("sh");
        // The child lives only if it was given the abstract name; the test sends its READY=1.
        command.arg("-c").arg(
            "test \"${NOTIFY_SOCKET#@lemnos-check-ready-}\" != \"$NOTIFY_SOCKET\" && sleep 10",
        );
        ready.give(&mut command);
        let mut daemon = command.spawn().unwrap();
        send(&ready, "READY=1");
        let waited = ready.wait(&mut daemon, Duration::from_secs(5));
        let _ = daemon.kill();
        let _ = daemon.wait();
        assert_eq!(waited, Ok(()));
    }

    #[test]
    fn two_starts_have_two_sockets() {
        let a = Ready::new().unwrap();
        let b = Ready::new().unwrap();
        assert_ne!(a.name, b.name);
    }
}
