//! The Lemnos service: owns the hardware and serves every client over one
//! socket. `docs/wire.md` is what it speaks. Nothing here names a kind of
//! device.
//!
//! Each client gets a small reader thread and writer thread. The readers
//! thread runs subscriptions, and one more keeps devices retrying.

pub mod builtin;
mod clock;
mod conn;
mod events;
mod notify;
mod readers;
mod requests;

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
use conn::Conn;
use notify::Notifier;
use readers::Readers;
use requests::Client;

/// Where a service listens unless told otherwise.
pub const DEFAULT_SOCKET: &str = "/run/lemnos/lemnosd.sock";
/// The board file a service reads unless told otherwise.
pub const DEFAULT_BOARD: &str = "/etc/lemnos/board.toml";

const CLIENT_STACK: usize = 128 * 1024;
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
    stop: AtomicBool,
    stop_lock: Mutex<()>,
    stop_wake: Condvar,
}

impl Core {
    pub(crate) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    pub(crate) fn wait_stop(&self, wait: Duration) {
        let guard = lock(&self.stop_lock);
        if !self.stopping() {
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

fn serve_client(core: &Arc<Core>, stream: UnixStream) -> io::Result<()> {
    let (read, write) = (stream.try_clone()?, stream.try_clone()?);
    let conn = Arc::new(Conn::new(stream));
    lock(&core.clients).push(Arc::clone(&conn));
    let writer = Arc::clone(&conn);
    let spawned = thread::Builder::new()
        .name("lemnosd-write".to_string())
        .stack_size(CLIENT_STACK)
        .spawn(move || writer.write_loop(write));
    if let Err(error) = spawned {
        conn.close();
        lock(&core.clients).retain(|c| !Arc::ptr_eq(c, &conn));
        return Err(error);
    }
    let mut client = Client::new(Arc::clone(core), Arc::clone(&conn));
    let spawned = thread::Builder::new()
        .name("lemnosd-read".to_string())
        .stack_size(CLIENT_STACK)
        .spawn(move || {
            client.read_loop(read);
            client.finish();
        });
    if let Err(error) = spawned {
        conn.close();
        lock(&core.clients).retain(|c| !Arc::ptr_eq(c, &conn));
        return Err(error);
    }
    Ok(())
}

impl Service {
    pub fn start(config: Config) -> Result<Self, StartError> {
        let listener = bind(&config.socket).map_err(StartError::Socket)?;
        let clock = Arc::new(MonotonicClock::new());
        let host = Host::start(
            &config.board,
            &config.drivers,
            config.backend,
            Arc::clone(&clock) as Arc<dyn lemnos_core::HostClock>,
            config.options,
        )
        .map_err(StartError::Host)?;
        let core = Arc::new(Core {
            host,
            clock,
            name: config.name,
            readers: Readers::new(),
            clients: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            stop_lock: Mutex::new(()),
            stop_wake: Condvar::new(),
        });
        let notifier = Arc::new(Notifier::from_env());
        let mut threads = Vec::new();

        let spawn = |name: &str, run: Box<dyn FnOnce() + Send>| {
            thread::Builder::new()
                .name(name.to_string())
                .spawn(run)
                .map_err(StartError::Socket)
        };
        let accepting = Arc::clone(&core);
        threads.push(spawn(
            "lemnosd-accept",
            Box::new(move || {
                for stream in listener.incoming() {
                    if accepting.stopping() {
                        return;
                    }
                    let Ok(stream) = stream else {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    };
                    // Out of threads costs this client, not the service.
                    let _ = serve_client(&accepting, stream);
                }
            }),
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
        // Wakes the acceptor, which then sees it should stop.
        let _ = UnixStream::connect(&self.socket);
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
