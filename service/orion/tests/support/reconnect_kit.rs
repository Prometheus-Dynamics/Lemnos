//! The bridge test kit shared by the Unix-socket reconnect tests and the TCP ones: a comm core and
//! lemnosd in-process, the bridge's three threads, and the helpers that wait on what the bridge
//! publishes. Each test binary includes this file with `#[path]`; it has no test of its own.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use lemnos_driver_system_identity::Identity;
use lemnos_fake_devices::{FakeAccelerometer, FakeCounter};
use lemnos_host::board::parse;
use lemnos_host::{Options as HostOptions, entry};
use lemnos_orion::config::Config;
use lemnos_orion::{Shared, calls, publish, source};
use lemnos_sim::SimBoard;
use lemnosd::{Config as LemnosdConfig, Service};
use orion::actions::{ActionCall, Finished, Outcome, Target};
use orion::provider::Found;
use orion::topics::{FieldId, Subscriber};
use orion::{Address, Orion};
use orion_node_app::{Node, NodeSettings};
use orion_stubs::{Board, Options as BoardOptions};
use tokio::sync::oneshot;

pub const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
    [[devices]]\nid = \"imu\"\ndriver = \"fake-accelerometer\"\n\
    [devices.config]\nx = 1.5\ny = 0\nz = 2\n\
    [[devices]]\nid = \"thing\"\ndriver = \"fake-counter\"\n[devices.config]\nstart = 41\n";

pub const WAIT: Duration = Duration::from_secs(20);

/// Short enough that a loaded machine misses the three heartbeats now and then; the restarts below
/// do not depend on it.
pub const HEARTBEAT_MS: u32 = 200;
/// The comm core's idle time for these runs: `comm.idle_ms` is zero, so it is three heartbeats. A
/// session the comm core has not heard from for this long is closed, and its names freed.
pub const IDLE: Duration = Duration::from_millis(3 * HEARTBEAT_MS as u64);
/// The comm core's options for every start: a short heartbeat, and the same place each time.
pub fn board_options() -> BoardOptions {
    BoardOptions {
        for_node: true,
        heartbeat_ms: HEARTBEAT_MS,
        ..BoardOptions::default()
    }
}

pub static NEXT: AtomicU32 = AtomicU32::new(0);

/// A scratch directory under TMPDIR, removed when the test ends, whether it passed or panicked.
pub struct Scratch(std::path::PathBuf);

impl std::ops::Deref for Scratch {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn dir(_test: &str) -> Scratch {
    let dir = lemnos_socket_dir::scratch_dir().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test directory");
    Scratch(dir)
}

pub fn start_lemnosd(socket: &Path) -> Service {
    Service::start(LemnosdConfig {
        socket: socket.to_path_buf(),
        name: "t".to_string(),
        board: parse(BOARD).expect("board"),
        drivers: vec![
            entry::<FakeAccelerometer>(),
            entry::<FakeCounter>(),
            entry::<Identity>(),
        ],
        backend: Box::new(SimBoard::new()),
        options: HostOptions::default(),
    })
    .expect("lemnosd")
}

/// Orion node logic in a thread of its own, until dropped.
pub struct Logic {
    kill: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Logic {
    pub fn start(dir: &Path) -> Self {
        let mut source = orion_config::Source::parse("", []).expect("no settings");
        let mut settings = NodeSettings::from_source(&mut source).expect("defaults");
        settings.config.socket = lemnos_socket_dir::socket(dir, "orion.sock").unwrap();
        settings.config.state_dir = dir.join("state");
        settings.files_dir = dir.join("files");
        settings.audit = None;
        settings.config.node_name = "board".parse().expect("name");
        let (kill, killed) = oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let mut node = Node::load(settings).expect("node logic starts");
                tokio::select! {
                    () = node.run(std::future::pending()) => {}
                    _ = killed => {}
                }
            });
        });
        Self {
            kill: Some(kill),
            thread: Some(thread),
        }
    }
}

impl Drop for Logic {
    fn drop(&mut self) {
        if let (Some(kill), Some(thread)) = (self.kill.take(), self.thread.take()) {
            let _ = kill.send(());
            let _ = thread.join();
        }
    }
}

pub fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// The bridge's three threads.
pub struct Bridge {
    pub shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

pub fn start_bridge(lemnosd: &Path, orion: &Path) -> Bridge {
    start_bridge_to(lemnosd, Address::Local(orion.to_path_buf()))
}

/// The bridge's three threads, reaching Orion at `orion`: a socket, or a network address.
pub fn start_bridge_to(lemnosd: &Path, orion: Address) -> Bridge {
    let shared = Arc::new(Shared::new().expect("shared"));
    let config = Config {
        lemnosd_socket: lemnosd.to_path_buf(),
        orion,
        node_id: None,
        run_as: None,
        files: Vec::new(),
        links: Vec::new(),
        streams: Vec::new(),
        lease: Some(lemnos_orion::config::LEASE),
        stall: lemnos_orion::files::IDLE,
    };
    let (jobs, queue) = mpsc::channel();
    let threads = vec![
        {
            let (socket, shared) = (lemnosd.to_path_buf(), Arc::clone(&shared));
            std::thread::spawn(move || source::run(&socket, &shared))
        },
        {
            let (socket, shared) = (lemnosd.to_path_buf(), Arc::clone(&shared));
            std::thread::spawn(move || {
                calls::run(&socket, &queue, &shared, Some(lemnos_orion::config::LEASE))
            })
        },
        {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || publish::run(&config, &shared, &jobs))
        },
    ];
    Bridge { shared, threads }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.shared.stop();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Polls `check` until it returns `Some`, or panics at the deadline.
pub fn wait_for<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn dashboard(orion: &Path) -> Orion {
    wait_for("a dashboard session", || {
        Orion::connect(&Address::Local(orion.to_path_buf()), "dashboard").ok()
    })
}

pub fn resource(dashboard: &Orion, name: &str) -> Option<Found> {
    dashboard
        .resources(name)
        .ok()?
        .into_iter()
        .find(|found| found.name == name)
}

pub fn latest(subscriber: &mut Subscriber, field: u16) -> Option<Vec<u8>> {
    let _ = subscriber.wait(Duration::from_millis(50));
    subscriber
        .latest(FieldId(field))
        .map(|sample| sample.payload.to_vec())
}

/// The generation the imu's description topic carries, once it has one.
pub fn topic_generation(subscriber: &mut Subscriber) -> Option<i64> {
    let bytes = latest(subscriber, 0)?;
    Some(i64::from_le_bytes(bytes.try_into().ok()?))
}

pub fn run(dashboard: &Orion, action: &str, resource: &str, args: &str) -> Finished {
    let mut run = dashboard
        .run(&ActionCall {
            name: action,
            target: Target::Resource(resource.parse().expect("name")),
            args: args.as_bytes(),
            deadline_ms: 0,
            detached: false,
        })
        .expect("run");
    run.finish(WAIT)
        .unwrap_or_else(|error| panic!("{action} {args:?} on {resource}: {error:?}"))
}

/// The generation `describe` gives the imu, from its device record.
pub fn described_generation(dashboard: &Orion) -> i64 {
    let done = run(dashboard, "lemnos.imu.call", "lemnos.imu", "describe");
    assert_eq!(done.outcome, Outcome::Succeeded, "{done:?}");
    generation_of(&done.output).expect("a generation")
}

pub fn generation_of(output: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(output).ok()?;
    let records = lemnos_orion::description::parse(text).ok()?;
    let device = records.iter().find(|r| r.word == "device")?;
    device.get("generation")?.parse().ok()
}

/// A session the bridge loses while the comm core still holds it (the link is stalled, not closed):
/// the bridge connects again, and its topics, its resources and `describe` answer again without any
/// change in the device. Each step waits on the event it depends on: the relay accepting the new
/// connection, and then the topic, describe and a call.
/// The counter's value, from its topic's latest sample.
pub fn counter(subscriber: &mut Subscriber) -> Option<f64> {
    let bytes = latest(subscriber, 0)?;
    Some(f64::from_le_bytes(bytes.as_slice().try_into().ok()?))
}

/// The board, its comm core, lemnosd, and a relay the bridge reaches Orion through.
pub fn start_relayed(dir: &Path) -> (Board, Logic, Service, PathBuf) {
    let lemnosd = lemnos_socket_dir::socket(dir, "lemnosd.sock").unwrap();
    let board = Board::start_with(dir, board_options());
    let logic = Logic::start(dir);
    let service = start_lemnosd(&lemnosd);
    (board, logic, service, lemnosd)
}
