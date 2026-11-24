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
        Self::from_values(
            std::env::var_os("NOTIFY_SOCKET").as_deref(),
            std::env::var("WATCHDOG_USEC").ok().as_deref(),
        )
    }

    /// From `NOTIFY_SOCKET` and `WATCHDOG_USEC`'s values. A socket that can't be reached is no
    /// manager, as an unset one is.
    fn from_values(notify: Option<&std::ffi::OsStr>, watchdog_usec: Option<&str>) -> Self {
        let socket = notify.and_then(|path| {
            let path = path.to_string_lossy().into_owned();
            let addr = match path.strip_prefix('@') {
                Some(name) => SocketAddr::from_abstract_name(name.as_bytes()).ok()?,
                None => SocketAddr::from_pathname(&path).ok()?,
            };
            let socket = UnixDatagram::unbound().ok()?;
            socket.connect_addr(&addr).ok()?;
            Some(socket)
        });
        let watchdog = watchdog_usec
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

    /// Whether there is a service manager to talk to.
    pub(crate) fn has_manager(&self) -> bool {
        self.socket.is_some()
    }
}

/// The service manager's file descriptor store, for the USB gadgets' files (`lemnos_linux`'s
/// `gadget/keep.rs`).
impl lemnos_linux::FdStore for Notifier {
    fn keep(&self, name: &str, fd: BorrowedFd<'_>) -> std::io::Result<bool> {
        self.store(name, fd)
    }

    /// `FDSTOREREMOVE=1`: the manager closes every descriptor it keeps under `name`.
    fn let_go(&self, name: &str) {
        self.send(&format!("FDSTOREREMOVE=1\nFDNAME={name}\n"));
    }

    /// `BARRIER=1` with a pipe's write end: the manager closes it once it has acted on every
    /// message before, so the read end then polls as hung up (`sd_notify_barrier`).
    fn barrier(&self) -> Option<std::os::fd::OwnedFd> {
        let socket = self.socket.as_ref()?;
        let (reader, writer) = std::io::pipe().ok()?;
        lemnos_linux_sys::scm::send_with_fds(socket.as_fd(), b"BARRIER=1\n", &[writer.as_fd()])
            .ok()?;
        drop(writer);
        Some(std::os::fd::OwnedFd::from(reader))
    }
}

#[cfg(test)]
mod tests {
    use super::Notifier;
    use std::ffi::OsStr;
    use std::os::fd::AsFd;
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};

    #[test]
    fn without_a_notify_socket_there_is_no_manager_and_nothing_is_stored() {
        // A BusyBox init: no NOTIFY_SOCKET. The gadgets get no store (`notify_for_gadgets`).
        let notifier = Notifier::from_values(None, None);
        assert!(!notifier.has_manager());
        let file = std::fs::File::open("/dev/null").unwrap();
        assert!(!notifier.store("lemnos-ffs.udc0.ep0", file.as_fd()).unwrap());
    }

    #[test]
    fn a_notify_socket_nobody_serves_is_no_manager() {
        let name = format!("lemnosd-notify-test-none-{}", std::process::id());
        let notifier = Notifier::from_values(Some(OsStr::new(&format!("@{name}"))), None);
        assert!(!notifier.has_manager());
    }

    #[test]
    fn a_served_notify_socket_is_the_manager() {
        let name = format!("lemnosd-notify-test-{}", std::process::id());
        let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let _served = UnixDatagram::bind_addr(&addr).unwrap();
        let notifier = Notifier::from_values(Some(OsStr::new(&format!("@{name}"))), Some("0"));
        assert!(notifier.has_manager());
        assert_eq!(notifier.watchdog_interval(), None);
    }
}
