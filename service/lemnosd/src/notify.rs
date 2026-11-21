//! systemd notifications: readiness, the watchdog, and the file descriptor store.

use std::os::fd::{AsFd, BorrowedFd};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::time::Duration;

/// The `$NOTIFY_SOCKET` connection, if systemd gave one. The socket is connected to it, so a message
/// with descriptors (`SCM_RIGHTS`) can be sent without an address.
pub(crate) struct Notifier {
    socket: Option<UnixDatagram>,
    watchdog: Option<Duration>,
}

impl Notifier {
    pub(crate) fn from_env() -> Self {
        let socket = std::env::var_os("NOTIFY_SOCKET").and_then(|path| {
            let path = path.to_string_lossy().into_owned();
            let addr = match path.strip_prefix('@') {
                Some(name) => SocketAddr::from_abstract_name(name.as_bytes()).ok()?,
                None => SocketAddr::from_pathname(&path).ok()?,
            };
            let socket = UnixDatagram::unbound().ok()?;
            socket.connect_addr(&addr).ok()?;
            Some(socket)
        });
        let watchdog = std::env::var("WATCHDOG_USEC")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|us| *us > 0)
            .map(Duration::from_micros);
        Self { socket, watchdog }
    }

    fn send(&self, message: &str) {
        if let Some(socket) = &self.socket {
            let _ = socket.send(message.as_bytes());
        }
    }

    pub(crate) fn ready(&self) {
        self.send("READY=1");
    }

    pub(crate) fn stopping(&self) {
        self.send("STOPPING=1");
    }

    /// How often to ping the watchdog (half its timeout), if enabled.
    pub(crate) fn watchdog_interval(&self) -> Option<Duration> {
        self.watchdog.map(|w| w / 2)
    }

    pub(crate) fn ping(&self) {
        self.send("WATCHDOG=1");
    }

    /// Gives `fd` to the service manager's file descriptor store under `name` (`FDSTORE=1`), to be
    /// passed back to the next start of this service. Returns `false` when there is no manager to
    /// take it (no `NOTIFY_SOCKET`: a BusyBox init, or a run by hand).
    ///
    /// # Errors
    /// The send's error, when there is a manager and it did not take the descriptor.
    pub(crate) fn store(&self, name: &str, fd: BorrowedFd<'_>) -> std::io::Result<bool> {
        let Some(socket) = &self.socket else {
            return Ok(false);
        };
        let message = format!("FDSTORE=1\nFDNAME={name}\n");
        lemnos_linux_sys::scm::send_with_fds(socket.as_fd(), message.as_bytes(), &[fd])?;
        Ok(true)
    }
}
