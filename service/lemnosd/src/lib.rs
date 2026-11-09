//! The Lemnos service: owns the hardware and serves every client over one
//! socket. `docs/wire.md` is what it speaks. Nothing here names a kind of
//! device.
//!
//! One thread polls the socket and every client. The readers thread runs
//! subscriptions, and one more keeps devices retrying.

pub mod builtin;
mod clock;
mod conn;
mod events;
mod notify;
mod readers;
mod requests;
mod serve;
mod streams;

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use lemnos_core::Provider;
use lemnos_host::board::BoardFile;
use lemnos_host::{DriverEntry, Host, HostError, Options};

use clock::MonotonicClock;
use conn::{Conn, Waker};
use notify::Notifier;
use readers::Readers;
use streams::MemfdStreams;

/// Where a service listens unless told otherwise.
pub const DEFAULT_SOCKET: &str = "/run/lemnos/lemnosd.sock";
/// The board file a service reads unless told otherwise.
pub const DEFAULT_BOARD: &str = "/etc/lemnos/board.toml";

const SOCKET_MODE: u32 = 0o660;

pub(crate) fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug)]
pub enum StartError {
    Host(HostError),
    Socket(io::Error),
}

impl core::fmt::Display for StartError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Host(error) => write!(f, "{error}"),
            Self::Socket(error) => write!(f, "the socket: {error}"),
        }
    }
}

impl std::error::Error for StartError {}

/// What a service is started with.
pub struct Config {
    pub socket: PathBuf,
    pub name: String,
    pub board: BoardFile,
    pub drivers: Vec<DriverEntry>,
    pub backend: Box<dyn Provider + Send>,
    pub options: Options,
}

pub(crate) struct Core {
    pub(crate) host: Host,
    pub(crate) clock: Arc<MonotonicClock>,
    pub(crate) name: String,
    pub(crate) readers: Readers,
    pub(crate) clients: Mutex<Vec<Arc<Conn>>>,
    pub(crate) waker: Arc<Waker>,
    stop: AtomicBool,
    stop_lock: Mutex<()>,
    stop_wake: Condvar,
    /// A wake arrived while the event loop wasn't waiting. Guarded by `stop_lock`.
    woken: AtomicBool,
}

impl Core {
    pub(crate) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Ends the event loop's current wait, or the next one if it isn't waiting.
    fn wake(&self) {
        let _guard = lock(&self.stop_lock);
        self.woken.store(true, Ordering::Relaxed);
        self.stop_wake.notify_all();
    }

    pub(crate) fn wait_stop(&self, wait: Duration) {
        let guard = lock(&self.stop_lock);
        if !self.stopping() && !self.woken.swap(false, Ordering::Relaxed) {
            drop(
                self.stop_wake
                    .wait_timeout(guard, wait)
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
    }
}

/// A running service. Dropping it stops it.
pub struct Service {
    core: Arc<Core>,
    socket: PathBuf,
    threads: Vec<JoinHandle<()>>,
    notifier: Arc<Notifier>,
}

fn bind(path: &Path) -> io::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // A socket left by a service that was killed. A live one still answers.
    if UnixStream::connect(path).is_ok() {
        return Err(io::ErrorKind::AddrInUse.into());
    }
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE))?;
    Ok(listener)
}

impl Service {
    pub fn start(config: Config) -> Result<Self, StartError> {
        let listener = bind(&config.socket).map_err(StartError::Socket)?;
        let (wake_tx, wake_rx) = UnixStream::pair().map_err(StartError::Socket)?;
        for socket in [&wake_tx, &wake_rx] {
            socket.set_nonblocking(true).map_err(StartError::Socket)?;
        }
        listener.set_nonblocking(true).map_err(StartError::Socket)?;
        let clock = Arc::new(MonotonicClock::new());
        let host = Host::start_with_streams(
            &config.board,
            &config.drivers,
            config.backend,
            Arc::clone(&clock) as Arc<dyn lemnos_core::HostClock>,
            config.options,
            Arc::new(MemfdStreams),
        )
        .map_err(StartError::Host)?;
        let core = Arc::new(Core {
            host,
            clock,
            name: config.name,
            readers: Readers::new(),
            clients: Mutex::new(Vec::new()),
            waker: Arc::new(Waker::new(wake_tx)),
            stop: AtomicBool::new(false),
            stop_lock: Mutex::new(()),
            stop_wake: Condvar::new(),
            woken: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&core);
        core.host.set_waker(move || {
            if let Some(core) = weak.upgrade() {
                core.wake();
            }
        });
        let notifier = Arc::new(Notifier::from_env());
        let mut threads = Vec::new();

        let spawn = |name: &str, run: Box<dyn FnOnce() + Send>| {
            thread::Builder::new()
                .name(name.to_string())
                .spawn(run)
                .map_err(StartError::Socket)
        };
        let serving = Arc::clone(&core);
        threads.push(spawn(
            "lemnosd-serve",
            Box::new(move || serve::run(&serving, &listener, &wake_rx)),
        )?);
        let reading = Arc::clone(&core);
        threads.push(spawn(
            "lemnosd-readers",
            Box::new(move || reading.readers.run(&reading.host, &reading.clock)),
        )?);
        let keeping = Arc::clone(&core);
        let watching = Arc::clone(&notifier);
        threads.push(spawn(
            "lemnosd-events",
            Box::new(move || events::run(&keeping, &watching)),
        )?);

        notifier.ready();
        Ok(Self {
            core,
            socket: config.socket,
            threads,
            notifier,
        })
    }

    pub const fn socket(&self) -> &PathBuf {
        &self.socket
    }

    /// The host it runs, for software producers in the same process and for inspection.
    #[must_use]
    pub fn host(&self) -> &Host {
        &self.core.host
    }

    /// Lets everything go. Clients see the socket close.
    pub fn stop(mut self) {
        self.shut();
    }

    /// Runs until the process ends.
    pub fn wait(mut self) {
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }

    fn shut(&mut self) {
        if self.threads.is_empty() {
            return;
        }
        self.notifier.stopping();
        self.core.stop.store(true, Ordering::Release);
        {
            let _guard = lock(&self.core.stop_lock);
            self.core.stop_wake.notify_all();
        }
        self.core.readers.stop();
        self.core.waker.wake();
        for client in lock(&self.core.clients).iter() {
            client.close();
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        self.core.host.shutdown();
        let _ = fs::remove_file(&self.socket);
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.shut();
    }
}
