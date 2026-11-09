//! systemd notifications: readiness and the watchdog.

use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::time::Duration;

/// The `$NOTIFY_SOCKET` connection, if systemd gave one.
pub(crate) struct Notifier {
    socket: Option<(UnixDatagram, SocketAddr)>,
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
            Some((UnixDatagram::unbound().ok()?, addr))
        });
        let watchdog = std::env::var("WATCHDOG_USEC")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|us| *us > 0)
            .map(Duration::from_micros);
        Self { socket, watchdog }
    }

    fn send(&self, message: &str) {
        if let Some((socket, addr)) = &self.socket {
            let _ = socket.send_to_addr(message.as_bytes(), addr);
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
}
