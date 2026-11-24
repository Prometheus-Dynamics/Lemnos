//! The service manager, as far as the daemon's unit uses it: the notify socket (`READY=1`, and the
//! file descriptor store's `FDSTORE=1` with `FDNAME=`, `FDSTOREREMOVE=1` and `BARRIER=1`) and the
//! store itself. The store is kept across the daemon's restarts and kills, and handed to each start
//! (`LISTEN_FDS`, `LISTEN_FDNAMES`), as systemd does under the unit's `FileDescriptorStoreMax=64`
//! and its default `FileDescriptorStorePreserve=restart`. There is no systemd in this guest; the
//! real one is checked by `testing/qemu/run-systemd-restart.sh`.
//!
//! Like systemd, it closes the descriptor a `BARRIER=1` carries once it has acted on every message
//! before it, removes every descriptor of a name on `FDSTOREREMOVE=1`, and refuses descriptors past
//! the store's bound without telling the sender. Unlike it, it does not drop a stored descriptor
//! that polls `EPOLLHUP` or `EPOLLERR` (FunctionFS files and memfds never do) and does not fold a
//! descriptor stored twice into one (the daemon stores each file once, when the gadget is bound).

use std::os::fd::{AsFd, OwnedFd};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::process::{Child, Command};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The unit's `FileDescriptorStoreMax=` (packaging/systemd/lemnosd.service).
pub const STORE_MAX: usize = 64;
/// How often a wait looks whether the daemon has exited.
const LOOK: Duration = Duration::from_millis(50);

#[derive(Default)]
struct State {
    store: Vec<(String, OwnedFd)>,
    /// How many `READY=1` came.
    ready: u64,
}

pub struct Manager {
    name: String,
    state: Mutex<State>,
    changed: Condvar,
}

impl Manager {
    /// Binds the notify socket (abstract, so the daemon can send to it after its drop) and starts
    /// the thread that serves it.
    pub fn start() -> Result<Arc<Self>, String> {
        let name = format!("lemnos-link-manager-{}", std::process::id());
        let addr = SocketAddr::from_abstract_name(name.as_bytes())
            .map_err(|error| format!("the notify socket's name: {error}"))?;
        let socket = UnixDatagram::bind_addr(&addr)
            .map_err(|error| format!("make the notify socket: {error}"))?;
        let manager = Arc::new(Self {
            name,
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        });
        let serving = Arc::clone(&manager);
        std::thread::spawn(move || serving.serve(&socket));
        Ok(manager)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn serve(&self, socket: &UnixDatagram) {
        let mut buf = vec![0u8; 4096];
        let mut fds = Vec::new();
        loop {
            fds.clear();
            let Ok(read) = lemnos_linux_sys::scm::recv_with_fds(socket.as_fd(), &mut buf, &mut fds)
            else {
                continue;
            };
            let message = String::from_utf8_lossy(&buf[..read]).into_owned();
            self.take(&message, &mut fds);
        }
    }

    /// Acts on one message, as systemd does, and says what it did with the store.
    fn take(&self, message: &str, fds: &mut Vec<OwnedFd>) {
        let has = |line: &str| message.lines().any(|l| l == line);
        let name = message
            .lines()
            .find_map(|line| line.strip_prefix("FDNAME="))
            .unwrap_or("stored")
            .to_owned();
        let mut state = self.lock();
        if has("BARRIER=1") {
            // Every message before it has been acted on: closing its descriptor says so.
            fds.clear();
        }
        if has("FDSTORE=1") && !fds.is_empty() {
            let count = fds.len();
            if state.store.len() + count > STORE_MAX {
                say!(
                    "     manager: the store is full ({STORE_MAX}): {count} descriptor(s) as {name} closed"
                );
                fds.clear();
            } else {
                state
                    .store
                    .extend(fds.drain(..).map(|fd| (name.clone(), fd)));
                say!("     manager: stored {count} descriptor(s) as {name}");
            }
        }
        if has("FDSTOREREMOVE=1") {
            let before = state.store.len();
            state.store.retain(|(kept, _)| *kept != name);
            say!(
                "     manager: removed {} descriptor(s) named {name}",
                before - state.store.len()
            );
        }
        if has("READY=1") {
            state.ready += 1;
        }
        fds.clear();
        drop(state);
        self.changed.notify_all();
    }

    /// Gives `command` the notify socket, and a copy of every stored descriptor with its name.
    pub fn give(&self, command: &mut Command) -> Result<(), String> {
        command.env("NOTIFY_SOCKET", format!("@{}", self.name));
        let passed = self
            .lock()
            .store
            .iter()
            .map(|(name, fd)| fd.try_clone().map(|fd| (name.clone(), fd)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("copy the store's descriptors: {error}"))?;
        lemnos_linux_sys::listen::pass_on(command, passed);
        Ok(())
    }

    /// How many `READY=1` came so far.
    pub fn ready(&self) -> u64 {
        self.lock().ready
    }

    /// Waits until a `READY=1` past the `past`th comes. An error if `daemon` exits first, or none
    /// comes within `within`.
    pub fn wait_ready(
        &self,
        past: u64,
        daemon: &mut Child,
        within: Duration,
    ) -> Result<(), String> {
        let until = Instant::now() + within;
        let mut state = self.lock();
        while state.ready <= past {
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
            state = self
                .changed
                .wait_timeout(state, LOOK)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        Ok(())
    }

    /// The names in the store, one per descriptor, in the order they came.
    pub fn names(&self) -> Vec<String> {
        self.lock()
            .store
            .iter()
            .map(|(name, _)| name.clone())
            .collect()
    }
}
