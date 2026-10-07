//! systemd notifications (`sd_notify`): readiness and the watchdog.

use std::os::unix::net::UnixDatagram;
use std::time::Duration;

/// The `$NOTIFY_SOCKET` connection, if systemd gave one.
pub(crate) struct Notifier {
    socket: Option<(UnixDatagram, std::os::unix::net::SocketAddr)>,
    watchdog: Option<Duration>,
}

impl Notifier {
    /// From `$NOTIFY_SOCKET` and `$WATCHDOG_USEC`.
    pub fn from_env() -> Self {
        let socket = std::env::var_os("NOTIFY_SOCKET").and_then(|path| {
            let path = path.to_string_lossy().into_owned();
            let addr = if let Some(name) = path.strip_prefix('@') {
                use std::os::linux::net::SocketAddrExt;
                std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).ok()?
            } else {
                std::os::unix::net::SocketAddr::from_pathname(&path).ok()?
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

    /// No notifications (tests, running by hand).
    pub fn none() -> Self {
        Self {
            socket: None,
            watchdog: None,
        }
    }

    fn send(&self, message: &str) {
        if let Some((socket, addr)) = &self.socket {
            let _ = socket.send_to_addr(message.as_bytes(), addr);
        }
    }

    pub fn ready(&self) {
        self.send("READY=1");
    }

    pub fn stopping(&self) {
        self.send("STOPPING=1");
    }

    pub fn status(&self, text: &str) {
        self.send(&format!("STATUS={text}"));
    }

    /// How often to ping the watchdog (half its timeout), if enabled.
    pub fn watchdog_interval(&self) -> Option<Duration> {
        self.watchdog.map(|w| w / 2)
    }

    pub fn ping(&self) {
        self.send("WATCHDOG=1");
    }
}
