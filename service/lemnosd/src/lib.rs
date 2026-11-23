//! The Lemnos service: owns the hardware and serves every client over one
//! socket. `docs/wire.md` is what it speaks. Nothing here names a kind of
//! device.
//!
//! One thread polls the socket and every client. The readers thread runs
//! subscriptions, and one more keeps devices retrying. A backend that can say
//! when hardware comes and goes gets a thread too: see [`Service::watch`].

pub mod builtin;
mod clock;
mod conn;
mod events;
pub mod gadget;
pub mod helper;
mod liveness;
mod notify;
mod outcomes;
pub mod packages;
pub mod privilege;
mod readers;
mod requests;
mod serve;
mod streams;
pub mod supervisor;

/// The daemon's `--check` of a board file: the file as the daemon reads it (a device whose own
/// entry is wrong is kept and refused), each gadget's board serial, then each driver's settings
/// check. `Ok` when nothing is refused, else the first refusal, by device and key.
///
/// # Errors
///
/// The file does not parse, or a device's settings are refused (the first one, as its reason).
pub fn check(text: &str, drivers: &[lemnos_host::DriverEntry]) -> Result<(), String> {
    let mut board_file = lemnos_host::board::parse_lenient(text).map_err(|e| e.to_string())?;
    gadget::resolve(&mut board_file);
    match lemnos_host::settings_refusals(&board_file, drivers)
        .into_iter()
        .next()
    {
        Some((_, reason)) => Err(reason),
        None => Ok(()),
    }
}

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use lemnos_core::{HintSource, Provider};
use lemnos_host::board::BoardFile;
use lemnos_host::{DriverEntry, Host, HostError, Liveness as HostLiveness, Options};

use clock::MonotonicClock;
use conn::{Conn, Waker};
use liveness::DaemonLiveness;
use notify::Notifier;
use packages::PackageReport;
use readers::Readers;
use streams::ServiceStreams;
pub use streams::{BufferSource, DmaHeaps};

/// Where a service listens unless told otherwise.
pub const DEFAULT_SOCKET: &str = "/run/lemnos/lemnosd.sock";
/// The board file a service reads unless told otherwise.
pub const DEFAULT_BOARD: &str = "/etc/lemnos/board.toml";

const SOCKET_MODE: u32 = 0o660;
/// The longest a hint source is left waiting before the service looks whether it is stopping.
const WATCH_WAIT: Duration = Duration::from_millis(250);

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
    /// Not sent to clients: a client's board is the board file's `[board] id`, which the host reports.
    pub name: String,
    pub board: BoardFile,
    pub drivers: Vec<DriverEntry>,
    pub backend: Box<dyn Provider + Send>,
    /// How the host treats its drivers. What the board file's `[host]` section sets wins over
    /// these; `lemnosd` passes the defaults.
    pub options: Options,
}

impl Config {
    /// A config with the parts that have no default, and every other field at its default
    /// (`options` is [`Options::default`]). Build a config as
    /// `Config { options, ..Config::new(socket, name, board, drivers, backend) }` so a field added
    /// later keeps compiling.
    pub fn new(
        socket: PathBuf,
        name: String,
        board: BoardFile,
        drivers: Vec<DriverEntry>,
        backend: Box<dyn Provider + Send>,
    ) -> Self {
        Self {
            socket,
            name,
            board,
            drivers,
            backend,
            options: Options::default(),
        }
    }
}

/// Takes one log line, without its newline. The daemon's log lines are the bring-up line of each
/// device and the notes its drivers report; they go to stderr unless the service was started with
/// [`Service::start_with_buffers_logged`].
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

pub(crate) struct Core {
    /// Where log lines go (see [`LogSink`]); `None` is stderr.
    pub(crate) log: Option<LogSink>,
    /// What became of the driver packages, for a client that asks.
    pub(crate) packages: PackageReport,
    pub(crate) host: Host,
    /// The daemon's liveness: the loop stamps it, and the host asks it (see [`liveness`]).
    pub(crate) liveness: Arc<DaemonLiveness>,
    pub(crate) clock: Arc<MonotonicClock>,
    pub(crate) readers: Readers,
    /// The outcomes of calls made with a token, for a client that asks after a timeout.
    pub(crate) outcomes: Arc<outcomes::Outcomes>,
    pub(crate) clients: Mutex<Vec<Arc<Conn>>>,
    pub(crate) waker: Arc<Waker>,
    stop: AtomicBool,
    stop_lock: Mutex<()>,
    stop_wake: Condvar,
    /// A wake arrived while the event loop wasn't waiting. Guarded by `stop_lock`.
    woken: AtomicBool,
}

impl Core {
    /// Writes one log line: to the configured sink, or to stderr.
    pub(crate) fn log_line(&self, line: &str) {
        match &self.log {
            Some(sink) => sink(line),
            None => {
                let _ = std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("{line}\n"));
            }
        }
    }

    pub(crate) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Ends the event loop's current wait, or the next one if it isn't waiting.
    pub(crate) fn wake(&self) {
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
    /// Starts a service whose streams of dma-bufs come from the kernel's dma-heaps.
    pub fn start(config: Config) -> Result<Self, StartError> {
        Self::start_with_buffers(config, Arc::new(DmaHeaps::default()))
    }

    /// [`Service::start`], for a service that looked for driver packages: `packages` is what
    /// became of them, which it tells a client that asks. The packages' drivers are already in
    /// `config.drivers` (see [`packages::add_packages`]).
    pub fn start_with_packages(
        config: Config,
        packages: PackageReport,
    ) -> Result<Self, StartError> {
        Self::start_whole(config, Arc::new(DmaHeaps::default()), packages, None)
    }

    /// [`Service::start`], with `buffers` giving the buffers of every stream the board file
    /// declares as dma-bufs.
    pub fn start_with_buffers(
        config: Config,
        buffers: Arc<dyn BufferSource>,
    ) -> Result<Self, StartError> {
        Self::start_whole(config, buffers, PackageReport::default(), None)
    }

    /// [`Service::start_with_buffers`], with the daemon's log lines going to `log` instead of
    /// stderr.
    pub fn start_with_buffers_logged(
        config: Config,
        buffers: Arc<dyn BufferSource>,
        log: LogSink,
    ) -> Result<Self, StartError> {
        Self::start_whole(config, buffers, PackageReport::default(), Some(log))
    }

    fn start_whole(
        config: Config,
        buffers: Arc<dyn BufferSource>,
        packages: PackageReport,
        log: Option<LogSink>,
    ) -> Result<Self, StartError> {
        let listener = bind(&config.socket).map_err(StartError::Socket)?;
        // A board with a keep-alive device (a hardware watchdog) needs the daemon's liveness.
        let keeps_alive = config.board.devices.iter().any(|device| device.keepalive);
        let liveness = Arc::new(DaemonLiveness::new());
        let socket = config.socket.clone();
        let (wake_tx, wake_rx) = UnixStream::pair().map_err(StartError::Socket)?;
        for socket in [&wake_tx, &wake_rx] {
            socket.set_nonblocking(true).map_err(StartError::Socket)?;
        }
        listener.set_nonblocking(true).map_err(StartError::Socket)?;
        let clock = Arc::new(MonotonicClock::new());
        // What the board file's `[host]` sets wins, as the host itself applies it.
        let read_floor = config.board.host.apply(config.options).read_floor();
        let host = Host::start_versioned(
            env!("CARGO_PKG_VERSION"),
            &config.board,
            &config.drivers,
            config.backend,
            Arc::clone(&clock) as Arc<dyn lemnos_core::HostClock>,
            config.options,
            Arc::new(ServiceStreams { source: buffers }),
        )
        .map_err(StartError::Host)?;
        if keeps_alive {
            host.set_liveness(Arc::clone(&liveness) as Arc<dyn HostLiveness>);
        }
        // Readers may import dma-bufs only under the daemon's signed mode with the stream's say-so;
        // open packages are the one setting that lifts that.
        host.set_signed_mode(packages.mode != packages::PackageMode::Open);
        let core = Arc::new(Core {
            log,
            packages,
            host,
            liveness: Arc::clone(&liveness),
            clock,
            readers: Readers::new(read_floor),
            outcomes: Arc::new(outcomes::Outcomes::new()),
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
        if keeps_alive {
            let probing = Arc::clone(&core);
            let probed = Arc::clone(&liveness);
            threads.push(spawn(
                "lemnosd-liveness",
                Box::new(move || liveness::probe(&probing, &probed, &socket)),
            )?);
        }
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

    /// Has the host hear `source`'s hints that hardware came or went, on a thread of its own,
    /// until the service stops or the source ends. Without one, a device that is absent binds
    /// when its retry comes round and no sooner.
    pub fn watch(&mut self, mut source: Box<dyn HintSource>) -> io::Result<()> {
        let core = Arc::clone(&self.core);
        let thread = thread::Builder::new()
            .name("lemnosd-hotplug".to_string())
            .spawn(move || {
                while !core.stopping() && source.wait(WATCH_WAIT, &mut |hint| core.host.hint(hint))
                {
                }
            })?;
        self.threads.push(thread);
        Ok(())
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

/// Whether the start trace is on (`LEMNOSD_START_TRACE`).
static TRACING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A host warning as a journal line at warning priority: systemd reads a leading `<4>` as the
/// level when the unit's stderr is the journal (`SyslogLevelPrefix`, on by default).
pub fn warning_line(text: &str) {
    let _ = io::Write::write_fmt(&mut io::stderr(), format_args!("<4>lemnosd: {text}\n"));
}

/// One line of the start trace, when it is on: seconds on CLOCK_MONOTONIC (the clock the boot path's own stamps
/// use), then the event. When the start was given a stop point (see [`StopAfter`]) and this line ends the stage
/// it names, the process stops itself right after the line: nothing after the stop point runs until something
/// continues it.
pub fn start_trace_line(event: &str) {
    if !TRACING.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let now = lemnos_linux_sys::time::monotonic_now().map_or(0.0, |d| d.as_secs_f64());
    let _ = io::Write::write_fmt(
        &mut io::stderr(),
        format_args!("lemnosd: start trace: {now:.6} {event}\n"),
    );
    // The line is written (stderr is unbuffered) before the stop, so a reader sees it whatever it does next.
    let stopping = STOP_AFTER.get().is_some_and(|stop| stop.ends(event));
    if stopping && let Err(error) = lemnos_linux_sys::signal::stop_self() {
        let _ = io::Write::write_fmt(
            &mut io::stderr(),
            format_args!("lemnosd: stopping after {event} failed: {error}\n"),
        );
    }
}

/// Where a start stops itself, for the QEMU watchdog check (`LEMNOSD_STOP_AFTER`, honoured only with the start
/// trace on). The stop lands right after the start-trace line that ends the stage, so a checker that reads the
/// stopped process's stderr sees every line up to the stop point and nothing after it, with no race against the
/// process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopAfter {
    /// The root phase's end (`root phase done`): before any device is bound.
    RootPhase,
    /// The end of one device's bind, healthy or not (`bind <id> ok` or `bind <id> failed`), named by the device's id.
    Bind(String),
}

impl StopAfter {
    /// Reads `LEMNOSD_STOP_AFTER`: `root-phase`, or `bind:<device id>`. `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        if text == "root-phase" {
            return Some(Self::RootPhase);
        }
        match text.strip_prefix("bind:") {
            Some(id) if !id.is_empty() => Some(Self::Bind(id.to_owned())),
            _ => None,
        }
    }

    /// Whether the start-trace `event` ends the stage this names.
    fn ends(&self, event: &str) -> bool {
        match self {
            Self::RootPhase => event.starts_with("root phase done"),
            Self::Bind(id) => {
                event == format!("bind {id} ok") || event == format!("bind {id} failed")
            }
        }
    }
}

/// The stop point of this start, set once by the daemon when `LEMNOSD_STOP_AFTER` asks (see [`StopAfter`]).
static STOP_AFTER: OnceLock<StopAfter> = OnceLock::new();

/// Gives this start its stop point. A second call is ignored.
pub fn stop_after_on(point: StopAfter) {
    let _ = STOP_AFTER.set(point);
}

/// Turns the start trace on (`LEMNOSD_START_TRACE`).
pub fn start_trace_on() {
    TRACING.store(true, std::sync::atomic::Ordering::Relaxed);
}
