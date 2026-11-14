//! The service against a real lemnosd (its service, over the simulated board, with the fake
//! accelerometer and counter), a real Orion comm core and real Orion node logic, in-process.
//!
//! Generic: the service has no code for either device. The devices are named only in the board
//! text and in the names the checks read. What the checks see is what any Orion program sees:
//! resources found by prefix, topics read by subscribing, actions run by name.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use lemnos_contract_self_test::{
    Outcome as SelfOutcome, RECORD_BYTES, Record, read_last, read_tests,
};
use lemnos_core::Reader;
use lemnos_driver_board_self_test::BoardSelfTest;
use lemnos_driver_system_identity::Identity;
use lemnos_driver_system_slots::SystemSlots;
use lemnos_fake_devices::{FakeAccelerometer, FakeCounter, FakeProbe};
use lemnos_host::board::parse;
use lemnos_host::{Options as HostOptions, entry};
use lemnos_orion::config::Config;
use lemnos_orion::{Shared, calls, publish, source};
use lemnos_sim::SimBoard;
use lemnosd::{Config as LemnosdConfig, Service};
use orion::actions::{ActionCall, Finished, Outcome, Target, Update};
use orion::provider::{Found, ResourceState};
use orion::topics::{FieldId, SampleTime, Status, Subscriber};
use orion::types::Error as Refusal;
use orion::{Address, ClientError, Orion};
use orion_node_app::{Node, NodeSettings};
use orion_stubs::{Board, Options as BoardOptions};
use tokio::sync::oneshot;

const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
    [[devices]]\nid = \"imu\"\ndriver = \"fake-accelerometer\"\n\
    [devices.config]\nx = 1.5\ny = 0\nz = 2\n\
    [[devices]]\nid = \"thing\"\ndriver = \"fake-counter\"\n[devices.config]\nstart = 41\n";

const WAIT: Duration = Duration::from_secs(20);

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A fresh directory for one test. Unix socket paths are short, so the name is short.
fn dir(test: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("lo-{test}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test directory");
    dir
}

/// lemnosd, over the simulated board with the fake devices.
fn start_lemnosd(socket: &Path) -> Service {
    Service::start(LemnosdConfig {
        socket: socket.to_path_buf(),
        name: "t".to_string(),
        board: parse(BOARD).expect("board"),
        drivers: vec![entry::<FakeAccelerometer>(), entry::<FakeCounter>()],
        backend: Box::new(SimBoard::new()),
        options: HostOptions::default(),
    })
    .expect("lemnosd")
}

/// A board with one device that cannot come up: its driver refuses it (`refuse = true`), so its bind
/// fails, and lemnosd keeps it retrying with the reason.
const REFUSED_BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
    [[devices]]\nid = \"bad\"\ndriver = \"fake-accelerometer\"\n[devices.config]\nrefuse = true\n";

fn start_lemnosd_refused(socket: &Path) -> Service {
    Service::start(LemnosdConfig {
        socket: socket.to_path_buf(),
        name: "t".to_string(),
        board: parse(REFUSED_BOARD).expect("board"),
        drivers: vec![entry::<FakeAccelerometer>()],
        backend: Box::new(SimBoard::new()),
        options: HostOptions::default(),
    })
    .expect("lemnosd")
}

/// Orion node logic in a thread of its own, until dropped.
struct Logic {
    kill: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Logic {
    fn start(dir: &Path) -> Self {
        let mut source = orion_config::Source::parse("", []).expect("no settings");
        let mut settings = NodeSettings::from_source(&mut source).expect("defaults");
        settings.config.socket = dir.join("orion.sock");
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

/// The service: its three threads.
struct Bridge {
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

fn start_bridge(lemnosd: &Path, orion: &Path, node: Option<&str>) -> Bridge {
    start_bridge_leased(lemnosd, orion, node, lemnos_orion::config::LEASE)
}

fn start_bridge_leased(
    lemnosd: &Path,
    orion: &Path,
    node: Option<&str>,
    lease: Duration,
) -> Bridge {
    start_bridge_opt(lemnosd, orion, node, Some(lease))
}

/// A bridge whose lease is `lease`, or off when it is `None` (`LEMNOS_ORION_LEASE=0`).
fn start_bridge_opt(
    lemnosd: &Path,
    orion: &Path,
    node: Option<&str>,
    lease: Option<Duration>,
) -> Bridge {
    let shared = Arc::new(Shared::new().expect("shared"));
    let config = Config {
        lemnosd_socket: lemnosd.to_path_buf(),
        orion: Address::Local(orion.to_path_buf()),
        node_id: node.map(str::to_owned),
        run_as: None,
        files: Vec::new(),
        links: Vec::new(),
        streams: Vec::new(),
        lease,
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
            std::thread::spawn(move || calls::run(&socket, &queue, &shared, lease))
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

/// Polls `check` until it returns `Some`, or panics.
fn wait_for<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn dashboard(orion: &Path) -> Orion {
    wait_for("a dashboard session", || {
        Orion::connect(&Address::Local(orion.to_path_buf()), "dashboard").ok()
    })
}

fn resource(dashboard: &Orion, name: &str) -> Option<Found> {
    dashboard
        .resources(name)
        .ok()?
        .into_iter()
        .find(|found| found.name == name)
}

/// The newest value of a field, as bytes.
fn latest(subscriber: &mut Subscriber, field: u16) -> Option<Vec<u8>> {
    let _ = subscriber.wait(Duration::from_millis(50));
    subscriber
        .latest(FieldId(field))
        .map(|sample| sample.payload.to_vec())
}

fn float(bytes: &[u8]) -> Option<f64> {
    Some(f64::from_le_bytes(bytes.try_into().ok()?))
}

fn run(dashboard: &Orion, action: &str, resource: &str, args: &str) -> Finished {
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

/// A run of an action, asked again while the comm core does not run it yet. After a comm core
/// restart Orion lists a resource with its actions before it takes the actions back, so until the
/// bridge has said them all a run is refused as not found. Asked again for at most [`WAIT`].
// ORION-R2: Orion restores actions before records at R2, so this retry is removed and every caller
// uses `run`. The bridge no longer waits on Orion's restore order: after a comm core restart it
// opens a new session that registers each device's actions before its resource, so a resource is
// never listed without them (`a_comm_core_restart_gets_every_device_and_value_again` checks this
// with `run`, not this helper).
fn run_once_said(dashboard: &Orion, action: &str, resource: &str, args: &str) -> Finished {
    let deadline = Instant::now() + WAIT;
    loop {
        let mut run = dashboard
            .run(&ActionCall {
                name: action,
                target: Target::Resource(resource.parse().expect("name")),
                args: args.as_bytes(),
                deadline_ms: 0,
                detached: false,
            })
            .expect("run");
        match run.finish(WAIT) {
            Err(ClientError::Refused(Refusal::NotFound)) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            // The device is not running (-19): refused before the call ran, so it is asked again
            // once it runs, within the same bound. A bind that misses under load leaves it retrying.
            Ok(done)
                if done.outcome == Outcome::Failed
                    && done.detail.contains("refused (-19)")
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            other => {
                return other
                    .unwrap_or_else(|error| panic!("{action} {args:?} on {resource}: {error:?}"));
            }
        }
    }
}

#[test]
fn devices_are_resources_their_values_and_status_are_topics() {
    let dir = dir("pushed");
    let lemnosd = dir.join("lemnosd.sock");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd(&lemnosd);
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());

    let thing = wait_for("the counter's resource", || {
        resource(&dashboard, "lemnos.thing")
    });
    assert_eq!(thing.kind, "lemnos.device");
    assert_eq!(thing.node, "board");
    assert_eq!(thing.state, ResourceState::Ready);
    assert!(
        thing
            .labels
            .contains(&("lemnos.driver".to_owned(), "fake-counter".to_owned()))
    );
    assert!(
        thing
            .labels
            .contains(&("lemnos.control.target".to_owned(), "u32".to_owned()))
    );
    assert_eq!(
        thing.topics,
        [
            "lemnos.thing.values",
            "lemnos.thing.status",
            "lemnos.thing.description"
        ]
    );
    assert_eq!(
        thing.actions,
        [
            "lemnos.thing.set",
            "lemnos.thing.call",
            "lemnos.thing.power"
        ]
    );
    let imu = wait_for("the imu's resource", || resource(&dashboard, "lemnos.imu"));
    assert_eq!(
        imu.actions,
        ["lemnos.imu.call", "lemnos.imu.power"],
        "no controls, no set"
    );

    // The accelerometer's fields are its values, published as they are read.
    let mut values = dashboard.subscribe("lemnos.imu.values").expect("subscribe");
    wait_for("the imu's x and z", || {
        let x = latest(&mut values, 0).and_then(|b| float(&b));
        let z = latest(&mut values, 2).and_then(|b| float(&b));
        (x == Some(1.5) && z == Some(2.0)).then_some(())
    });
    let mut status = dashboard.subscribe("lemnos.imu.status").expect("subscribe");
    wait_for("the imu's status", || {
        (latest(&mut status, 0).as_deref() == Some(b"running".as_slice())
            && latest(&mut status, 1).as_deref() == Some(b"ok".as_slice()))
        .then_some(())
    });

    // The counter counts on every read, so its value moves with no timer of ours.
    let mut count = dashboard
        .subscribe("lemnos.thing.values")
        .expect("subscribe");
    let first = wait_for("the counter's first value", || {
        latest(&mut count, 0).and_then(|b| float(&b))
    });
    wait_for("a later counter value", || {
        latest(&mut count, 0)
            .and_then(|b| float(&b))
            .filter(|value| *value > first + 2.0)
    });
}

#[test]
fn actions_set_call_and_switch_a_device() {
    let dir = dir("actions");
    let lemnosd = dir.join("lemnosd.sock");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd(&lemnosd);
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    wait_for("the counter's resource", || {
        resource(&dashboard, "lemnos.thing")
    });

    // A control, then any method by name: the reply is the output, as text.
    let set = run(
        &dashboard,
        "lemnos.thing.set",
        "lemnos.thing",
        "target 1000",
    );
    assert_eq!(set.outcome, Outcome::Succeeded, "{set:?}");
    let peek = run(&dashboard, "lemnos.thing.call", "lemnos.thing", "peek");
    assert_eq!(peek.outcome, Outcome::Succeeded, "{peek:?}");
    let value: u32 = String::from_utf8(peek.output).unwrap().parse().unwrap();
    assert!(value >= 1000, "the counter was set: {value}");

    // Words the device does not take are refused without a call; a device that says no fails.
    let read_only = run(&dashboard, "lemnos.thing.set", "lemnos.thing", "count 1");
    assert_eq!(read_only.outcome, Outcome::Rejected);
    assert!(read_only.detail.contains("read-only"), "{read_only:?}");
    let bad = run(
        &dashboard,
        "lemnos.thing.set",
        "lemnos.thing",
        "target lots",
    );
    assert_eq!(bad.outcome, Outcome::Rejected);
    let failed = run(&dashboard, "lemnos.thing.call", "lemnos.thing", "fail");
    assert_eq!(failed.outcome, Outcome::Failed, "{failed:?}");
    // An action is the one resource's: Orion finds no such action against another.
    let mut elsewhere = dashboard
        .run(&ActionCall {
            name: "lemnos.thing.call",
            target: Target::Resource("lemnos.imu".parse().expect("name")),
            args: b"peek",
            deadline_ms: 0,
            detached: false,
        })
        .expect("run");
    let ended = elsewhere.finish(WAIT);
    assert!(
        !matches!(&ended, Ok(finished) if finished.outcome == Outcome::Succeeded),
        "{ended:?}"
    );

    // Switched off: the status topic and the resource follow, and back on.
    let off = run(&dashboard, "lemnos.thing.power", "lemnos.thing", "off");
    assert_eq!(off.outcome, Outcome::Succeeded, "{off:?}");
    let mut status = dashboard
        .subscribe("lemnos.thing.status")
        .expect("subscribe");
    wait_for("the counter off", || {
        (latest(&mut status, 0).as_deref() == Some(b"off".as_slice())).then_some(())
    });
    wait_for("the counter's resource pending", || {
        resource(&dashboard, "lemnos.thing").filter(|found| found.state == ResourceState::Pending)
    });
    let on = run(&dashboard, "lemnos.thing.power", "lemnos.thing", "on");
    assert_eq!(on.outcome, Outcome::Succeeded, "{on:?}");
    wait_for("the counter ready again", || {
        resource(&dashboard, "lemnos.thing").filter(|found| found.state == ResourceState::Ready)
    });
    let refused = run(&dashboard, "lemnos.thing.power", "lemnos.thing", "maybe");
    assert_eq!(refused.outcome, Outcome::Rejected);
}

#[test]
fn a_comm_core_restart_gets_every_device_and_value_again() {
    let dir = dir("restart");
    let lemnosd = dir.join("lemnosd.sock");
    let mut board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd(&lemnosd);
    let socket = board.socket().to_path_buf();
    let _bridge = start_bridge(&lemnosd, &socket, None);
    {
        let dashboard = dashboard(&socket);
        wait_for("the imu's resource", || resource(&dashboard, "lemnos.imu"));
    }

    // The comm core goes away and comes back empty. The service connects again and says
    // every topic, action and resource again.
    drop(board);
    std::thread::sleep(Duration::from_millis(300));
    board = Board::start_for_node(&dir);
    let dashboard = dashboard(board.socket());
    wait_for("the imu's resource after the restart", || {
        resource(&dashboard, "lemnos.imu").filter(|found| found.state == ResourceState::Ready)
    });
    // The counter's actions are said after the imu's, one by one: wait until they are there.
    // A resource listed Ready is never listed without its call action: the bridge registers the
    // actions before it publishes the resource, so a run asked as soon as it is listed is answered.
    // Polled without a pause, so that a listing in the middle of a restore is seen.
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(found) =
            resource(&dashboard, "lemnos.thing").filter(|found| found.state == ResourceState::Ready)
        {
            assert!(
                found.actions.contains(&"lemnos.thing.call".to_owned()),
                "listed Ready without its call action: {found:?}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out: the counter's resource after the restart"
        );
    }
    let mut values = dashboard.subscribe("lemnos.imu.values").expect("subscribe");
    wait_for("the imu's x after the restart", || {
        (latest(&mut values, 0).and_then(|b| float(&b)) == Some(1.5)).then_some(())
    });
    // No retry: the first run asked after the counter is listed is answered, not refused.
    let peek = run(&dashboard, "lemnos.thing.call", "lemnos.thing", "peek");
    assert_eq!(peek.outcome, Outcome::Succeeded, "{peek:?}");
}

#[test]
fn a_node_that_is_not_the_configured_one_gets_nothing() {
    let dir = dir("node");
    let lemnosd = dir.join("lemnosd.sock");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd(&lemnosd);
    // The comm core is the node `board`; the service is told to publish on `node-b`.
    let _bridge = start_bridge(&lemnosd, board.socket(), Some("node-b"));
    let dashboard = dashboard(board.socket());
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        // Node logic may not be up yet: a refused list holds nothing either.
        assert!(
            dashboard
                .resources("lemnos.")
                .unwrap_or_default()
                .is_empty(),
            "nothing is published to a node that is not ORION_NODE_ID"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_stopped_lemnosd_takes_its_devices_and_topics_away_and_brings_them_back() {
    let dir = dir("lemnosd");
    let lemnosd = dir.join("lemnosd.sock");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let service = start_lemnosd(&lemnosd);
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    wait_for("the imu's resource", || resource(&dashboard, "lemnos.imu"));
    let mut status = dashboard.subscribe("lemnos.imu.status").expect("subscribe");
    wait_for("the imu's status live", || {
        let _ = status.wait(Duration::from_millis(50));
        (status.status() == Status::Live).then_some(())
    });

    // A call, so the call thread holds a connection to this lemnosd.
    let peek = run(&dashboard, "lemnos.thing.call", "lemnos.thing", "peek");
    assert_eq!(peek.outcome, Outcome::Succeeded, "{peek:?}");

    // lemnosd stops: the resources are deleted, the status says `gone`, not `running`, and the
    // topics' publisher is gone. The service keeps running.
    drop(service);
    wait_for("the resources withdrawn", || {
        dashboard
            .resources("lemnos.")
            .ok()
            .filter(Vec::is_empty)
            .map(drop)
    });
    wait_for("the status topic's publisher gone, saying gone", || {
        let _ = status.wait(Duration::from_millis(50));
        (status.status() == Status::PublisherGone
            && latest(&mut status, 0).as_deref() == Some(b"gone".as_slice()))
        .then_some(())
    });

    // lemnosd comes back: the devices are published again, in the same topics.
    let _service = start_lemnosd(&lemnosd);
    wait_for("the imu back", || resource(&dashboard, "lemnos.imu"));
    wait_for("the counter back, with its call action", || {
        resource(&dashboard, "lemnos.thing").filter(|found| {
            found.state == ResourceState::Ready
                && found.actions.contains(&"lemnos.thing.call".to_owned())
        })
    });
    wait_for("the status topic live again", || {
        let _ = status.wait(Duration::from_millis(50));
        (status.status() == Status::Live
            && latest(&mut status, 0).as_deref() == Some(b"running".as_slice()))
        .then_some(())
    });
    // The call thread's old connection is dead; the first call after the return still works.
    let peek = run_once_said(&dashboard, "lemnos.thing.call", "lemnos.thing", "peek");
    assert_eq!(peek.outcome, Outcome::Succeeded, "{peek:?}");
}

#[test]
fn a_device_that_starts_running_later_is_described_then() {
    let dir = dir("later");
    let lemnosd = dir.join("lemnosd.sock");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd(&lemnosd);
    // The counter is off before the service starts, so lemnosd cannot describe it.
    let client = lemnos_client::Client::connect(&lemnosd, "test").expect("lemnosd");
    let counter = client
        .list()
        .expect("list")
        .into_iter()
        .find(|device| device.name == "thing")
        .expect("the counter");
    client.set_power(counter.index, false).expect("off");
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    let off = wait_for("the counter, off", || resource(&dashboard, "lemnos.thing"));
    assert_eq!(off.state, ResourceState::Pending);
    assert_eq!(
        off.topics,
        ["lemnos.thing.status", "lemnos.thing.description"],
        "nothing described, no values"
    );

    // Switched on, it is described, and its values and controls appear.
    client.set_power(counter.index, true).expect("on");
    let on = wait_for("the counter described", || {
        resource(&dashboard, "lemnos.thing").filter(|found| found.topics.len() == 3)
    });
    assert_eq!(on.state, ResourceState::Ready);
    assert!(on.actions.contains(&"lemnos.thing.set".to_owned()));
    let mut count = dashboard
        .subscribe("lemnos.thing.values")
        .expect("subscribe");
    wait_for("a counter value", || {
        latest(&mut count, 0).and_then(|b| float(&b))
    });
}

/// The board's identity device, with its files in `dir`: the cpuinfo its serial is read from,
/// and the data partition its running slot is read from.
struct IdentityFiles {
    dir: PathBuf,
}

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The contract ids the three devices of [`identity_board`] offer, sorted.
const OFFERS: &str = "accelerometer/1 example.counter/1 system.identity/1";

impl IdentityFiles {
    fn new(dir: &Path, cpuinfo: &str) -> Self {
        let files = Self {
            dir: dir.to_path_buf(),
        };
        std::fs::write(files.dir.join("cpuinfo"), cpuinfo).expect("cpuinfo");
        files
    }

    /// Boots `slot`: the marker names it and its manifest is on the data partition, as the
    /// initramfs leaves them.
    /// Removes the boot marker: no slot is running now, so its facts go.
    fn unboot(&self) {
        let _ = std::fs::remove_file(self.dir.join("data/board/slots/booted"));
    }

    fn boot(&self, slot: &str, product: &str, version: &str) {
        let slots = self.dir.join("data/slots");
        std::fs::create_dir_all(&slots).expect("slots");
        let marker = self.dir.join("data/board/slots");
        std::fs::create_dir_all(&marker).expect("marker");
        std::fs::write(
            slots.join(format!("{slot}.json")),
            manifest(slot, product, version),
        )
        .expect("manifest");
        std::fs::write(marker.join("booted"), format!("{slot}\n")).expect("booted");
    }

    /// The board file: the two devices of [`BOARD`], and the identity device.
    fn board(&self) -> String {
        format!(
            "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"reference\"\n\
             [[devices]]\nid = \"imu\"\ndriver = \"fake-accelerometer\"\n\
             [devices.config]\nx = 1.5\ny = 0\nz = 2\n\
             [[devices]]\nid = \"thing\"\ndriver = \"fake-counter\"\n[devices.config]\nstart = 41\n\
             [[devices]]\nid = \"identity\"\ndriver = \"system-identity\"\n[devices.config]\n\
             serial = \"cpuinfo\"\nserial_path = \"{cpuinfo}\"\nname = \"Alpha\"\nrevision = \"r2\"\n\
             data = \"{data}\"\n",
            cpuinfo = self.dir.join("cpuinfo").display(),
            data = self.dir.join("data").display(),
        )
    }
}

/// A slot manifest, schema 3, as the slots driver writes it.
fn manifest(slot: &str, product: &str, version: &str) -> String {
    format!(
        "{{\"schema\":3,\"id\":\"{slot}\",\"product\":\"{product}\",\"version\":\"{version}\",\"board\":\"reference\",\
         \"image\":{{\"size\":4096,\"sha256\":\"{HASH}\"}},\
         \"verity\":{{\"root_hash\":\"{HASH}\",\"salt\":\"{HASH}\",\"data_blocks\":1,\"hash_offset\":4096}},\
         \"boot\":{{\"files\":{{\"cmdline.txt\":\"{HASH}\"}}}}}}"
    )
}

/// lemnosd over a board file's text, with the identity driver as well as the fake devices.
fn start_lemnosd_board(socket: &Path, board: &str) -> Service {
    Service::start(LemnosdConfig {
        socket: socket.to_path_buf(),
        name: "t".to_string(),
        board: parse(board).expect("board"),
        drivers: vec![
            entry::<FakeAccelerometer>(),
            entry::<FakeCounter>(),
            entry::<Identity>(),
            entry::<SystemSlots>(),
        ],
        backend: Box::new(SimBoard::new()),
        options: HostOptions::default(),
    })
    .expect("lemnosd")
}

/// The facts the node's hello document carries now, by name. `None` until a session has them.
fn hello(dashboard: &Orion) -> Option<BTreeMap<String, String>> {
    let node = dashboard.node().ok()?;
    Some(
        node.facts
            .into_iter()
            .map(|fact| (fact.name, String::from_utf8_lossy(&fact.value).into_owned()))
            .collect(),
    )
}

/// Waits until the hello document has `name`, and returns the facts.
fn hello_with(dashboard: &Orion, name: &str) -> BTreeMap<String, String> {
    wait_for(&format!("the fact {name}"), || {
        hello(dashboard).filter(|facts| facts.contains_key(name))
    })
}

#[test]
fn the_board_publishes_its_identity_facts_and_what_it_offers() {
    let dir = dir("facts");
    let lemnosd = dir.join("lemnosd.sock");
    let files = IdentityFiles::new(&dir, "Serial\t\t: 0000000000ABCDEF\n");
    files.boot("os-2", "an-os", "2.0");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd_board(&lemnosd, &files.board());
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());

    let facts = hello_with(&dashboard, "offers");
    let expected: BTreeMap<String, String> = [
        ("family", "reference"),
        ("lease.seconds", "60"),
        ("leases.ended", "0"),
        ("model", "Alpha r2"),
        ("offers", OFFERS),
        ("serial", "0000000000abcdef"),
        ("version.lemnos", env!("CARGO_PKG_VERSION")),
        ("version.os", "2.0"),
        ("version.product", "an-os"),
        ("version.slot", "os-2"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect();
    assert_eq!(facts, expected);
}

#[test]
fn a_fact_that_cannot_be_read_is_absent() {
    let dir = dir("absent");
    let lemnosd = dir.join("lemnosd.sock");
    // No Serial line, and no slot is booted: those two facts cannot be known.
    let files = IdentityFiles::new(&dir, "model name : no serial here\n");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd_board(&lemnosd, &files.board());
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());

    let facts = hello_with(&dashboard, "offers");
    let names: Vec<&str> = facts.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "family",
            "lease.seconds",
            "leases.ended",
            "model",
            "offers",
            "version.lemnos"
        ],
        "no serial and no running slot: absent, not guessed"
    );
}

#[test]
fn the_running_slot_is_followed_without_a_rebind() {
    let dir = dir("slot");
    let lemnosd = dir.join("lemnosd.sock");
    let files = IdentityFiles::new(&dir, "Serial : 0000000000abcdef\n");
    files.boot("os-2", "an-os", "2.0");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd_board(&lemnosd, &files.board());
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    hello_with(&dashboard, "version.slot");

    // The boot marker moves to another slot while lemnosd keeps running: the fact follows it.
    files.boot("os-3", "other-os", "3.0");
    wait_for("the running slot is os-3", || {
        hello(&dashboard).filter(|facts| {
            facts.get("version.slot").map(String::as_str) == Some("os-3")
                && facts.get("version.product").map(String::as_str) == Some("other-os")
                && facts.get("version.os").map(String::as_str) == Some("3.0")
        })
    });
}

#[test]
fn a_slot_that_becomes_known_after_the_start_is_published() {
    let dir = dir("late-slot");
    let lemnosd = dir.join("lemnosd.sock");
    let files = IdentityFiles::new(&dir, "Serial : 0000000000abcdef\n");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd_board(&lemnosd, &files.board());
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    let before = hello_with(&dashboard, "serial");
    assert!(!before.contains_key("version.slot"), "{before:?}");

    // A new fact is a new topic: the session is published again, and the fact is there.
    files.boot("os-2", "an-os", "2.0");
    let after = hello_with(&dashboard, "version.slot");
    assert_eq!(after.get("version.slot").map(String::as_str), Some("os-2"));
    assert_eq!(
        after.get("serial").map(String::as_str),
        Some("0000000000abcdef")
    );
}

#[test]
fn offers_follow_a_device_going_and_coming() {
    let dir = dir("offers");
    let lemnosd = dir.join("lemnosd.sock");
    let files = IdentityFiles::new(&dir, "Serial : 0000000000abcdef\n");
    files.boot("os-2", "an-os", "2.0");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let service = start_lemnosd_board(&lemnosd, &files.board());
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    hello_with(&dashboard, "offers");
    let offers =
        |dashboard: &Orion| hello(dashboard).and_then(|facts| facts.get("offers").cloned());
    wait_for("every device offered", || {
        (offers(&dashboard).as_deref() == Some(OFFERS)).then_some(())
    });

    // The counter is switched off: its contract is no longer offered; on again, it is.
    let client = lemnos_client::Client::connect(&lemnosd, "test").expect("lemnosd");
    let counter = client
        .list()
        .expect("list")
        .into_iter()
        .find(|device| device.name == "thing")
        .expect("the counter");
    client.set_power(counter.index, false).expect("off");
    wait_for("the counter's contract not offered", || {
        (offers(&dashboard).as_deref() == Some("accelerometer/1 system.identity/1")).then_some(())
    });
    client.set_power(counter.index, true).expect("on");
    wait_for("the counter's contract offered again", || {
        (offers(&dashboard).as_deref() == Some(OFFERS)).then_some(())
    });
    drop(service);
}

/// The board of [`IdentityFiles::board`], with the slot driver over the same data and boot
/// directories as the identity device reads.
fn outbuf_board(files: &IdentityFiles) -> String {
    format!(
        "{}[[devices]]\nid = \"slots\"\ndriver = \"system-slots\"\n\
         config = {{ data = \"{}\", boot = \"{}\", record = \"file\", partuuid = \"abcd-02\", \
         boot_sources = [\"rpi-boot-mode-1=/soc/mmc@7e202000\"], boot_args = \"os\", reserve_mb = 0, running = \"\" }}\n",
        files.board(),
        files.dir.join("data").display(),
        files.dir.join("boot").display()
    )
}

#[test]
fn the_call_action_carries_an_out_buffer_reply_whole_or_names_why_not() {
    let dir = dir("outbuf");
    let lemnosd = dir.join("lemnosd.sock");
    let files = IdentityFiles::new(&dir, "Serial\t\t: 0000000000ABCDEF\n");
    files.boot("os-2", "an-os", "2.0");
    // A slot is listed once its image is there, as the manifest's 4096 bytes.
    image(&files.dir, "os-2");
    std::fs::create_dir_all(dir.join("boot/slots")).expect("boot slots");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd_board(&lemnosd, &outbuf_board(&files));
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    wait_for("the slots resource", || {
        resource(&dashboard, "lemnos.slots")
    });
    wait_for("the identity resource", || {
        resource(&dashboard, "lemnos.identity")
    });

    // `facts` takes an out buffer and no arguments: the bridge sends the size itself.
    let facts = run_once_said(
        &dashboard,
        "lemnos.identity.call",
        "lemnos.identity",
        "facts",
    );
    assert_eq!(facts.outcome, Outcome::Succeeded, "{facts:?}");
    let text = String::from_utf8(facts.output).expect("text");
    assert!(text.contains("serial=0000000000abcdef"), "{text}");

    // `list` on the slot device: JSON, as text.
    let list = run_once_said(&dashboard, "lemnos.slots.call", "lemnos.slots", "list");
    assert_eq!(list.outcome, Outcome::Succeeded, "{list:?}");
    let json = String::from_utf8(list.output).expect("text");
    assert!(json.starts_with("{\"slot_active\":"), "{json}");
    assert!(json.contains("\"id\":\"os-2\""), "{json}");

    // More slots than one Orion frame carries (64 KiB): the reply is refused by name, never cut
    // short, and the call returns rather than waiting.
    for i in 0..600 {
        let id = format!("bulk-{i:03}");
        std::fs::write(
            files.dir.join("data/slots").join(format!("{id}.json")),
            manifest(&id, "bulk", "1.0"),
        )
        .expect("manifest");
        image(&files.dir, &id);
    }
    let big = run_once_said(&dashboard, "lemnos.slots.call", "lemnos.slots", "list");
    assert_ne!(big.outcome, Outcome::Succeeded, "{big:?}");
    assert!(big.output.is_empty(), "no partial reply: {big:?}");
    assert!(big.detail.contains("no paged form"), "{big:?}");
    assert!(big.detail.contains("61440"), "{big:?}");
}

/// The slot ids in a `list_page` reply, in order.
fn page_ids(json: &str) -> Vec<String> {
    json.split("\"id\":\"")
        .skip(1)
        .map(|rest| rest.split('"').next().unwrap_or_default().to_string())
        .collect()
}

/// The text of a string field of a reply, or of a bare word field such as `more`.
fn page_field<'a>(json: &'a str, key: &str) -> &'a str {
    let start = json
        .find(&format!("\"{key}\":"))
        .unwrap_or_else(|| panic!("no {key} in {json}"))
        + key.len()
        + 3;
    let rest = &json[start..];
    match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next().unwrap_or_default(),
        None => rest.split([',', '}']).next().unwrap_or_default(),
    }
}

#[test]
fn the_slot_list_is_walked_page_by_page_through_the_bridge_with_nothing_lost_or_repeated() {
    let dir = dir("paged");
    let lemnosd = dir.join("lemnosd.sock");
    let files = IdentityFiles::new(&dir, "Serial\t\t: 0000000000ABCDEF\n");
    files.boot("os-2", "an-os", "2.0");
    image(&files.dir, "os-2");
    std::fs::create_dir_all(dir.join("boot/slots")).expect("boot slots");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd_board(&lemnosd, &outbuf_board(&files));
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    wait_for("the slots resource", || {
        resource(&dashboard, "lemnos.slots")
    });

    // 600 more slots: about 210 KB of list, so `list` is refused and the pages carry it.
    let mut expected: Vec<String> = vec!["os-2".to_string()];
    for i in 0..600 {
        let id = format!("bulk-{i:03}");
        std::fs::write(
            files.dir.join("data/slots").join(format!("{id}.json")),
            manifest(&id, "bulk", "1.0"),
        )
        .expect("manifest");
        image(&files.dir, &id);
        expected.push(id);
    }
    let whole = run_once_said(&dashboard, "lemnos.slots.call", "lemnos.slots", "list");
    assert_ne!(whole.outcome, Outcome::Succeeded, "{whole:?}");

    // Page by page, the cursor the last page named. After the first page a slot is installed:
    // its install number is larger than every other slot's, as a real install's is.
    let mut seen: Vec<String> = Vec::new();
    let mut after = "-".to_string();
    let mut pages = 0;
    loop {
        let reply = run_once_said(
            &dashboard,
            "lemnos.slots.call",
            "lemnos.slots",
            &format!("list_page {after}"),
        );
        assert_eq!(reply.outcome, Outcome::Succeeded, "{reply:?}");
        assert!(reply.output.len() <= 61440, "{} bytes", reply.output.len());
        let json = String::from_utf8(reply.output).expect("text");
        pages += 1;
        seen.extend(page_ids(&json));
        if pages == 1 {
            let id = "late";
            std::fs::write(
                files.dir.join("data/slots").join(format!("{id}.json")),
                manifest(id, "late", "1.0"),
            )
            .expect("manifest");
            image(&files.dir, id);
            let meta = files
                .dir
                .join("data/board/slots")
                .join(format!("{id}.meta"));
            std::fs::write(meta, "SEQ='100000'\nINSTALLED='2026-11-13T00:00:00Z'\n").expect("meta");
        }
        if page_field(&json, "more") != "true" {
            break;
        }
        after = page_field(&json, "next").to_string();
        assert_ne!(after, "", "{json}");
        assert!(pages < 100, "the walk does not end");
    }
    assert!(pages >= 3, "{pages} pages");
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), seen.len(), "a slot came on two pages");
    expected.sort();
    assert_eq!(
        sorted, expected,
        "every slot that was there at the start, once, and no later one"
    );
}

/// The image file of slot `id` under `dir`, 4096 bytes as its manifest says.
fn image(dir: &Path, id: &str) {
    let file =
        std::fs::File::create(dir.join("data/slots").join(format!("{id}.erofs"))).expect("image");
    file.set_len(4096).expect("image size");
}

#[path = "support/tracked.rs"]
mod tracked;
use tracked::TrackedDevice;

/// lemnosd over a board text, with the given drivers.
fn start_lemnosd_with(
    socket: &Path,
    board: &str,
    drivers: Vec<lemnos_host::DriverEntry>,
) -> Service {
    Service::start(LemnosdConfig {
        socket: socket.to_path_buf(),
        name: "t".to_string(),
        board: parse(board).expect("board"),
        drivers,
        backend: Box::new(SimBoard::new()),
        options: HostOptions::default(),
    })
    .expect("lemnosd")
}

/// The newest value of a field, with the time it belongs to.
fn latest_sample(subscriber: &mut Subscriber, field: u16) -> Option<(Vec<u8>, SampleTime)> {
    let _ = subscriber.wait(Duration::from_millis(50));
    subscriber
        .latest(FieldId(field))
        .map(|sample| (sample.payload.to_vec(), sample.time))
}

#[test]
fn a_fact_that_comes_and_goes_moves_nothing_else() {
    let dir = dir("fact-moves");
    let lemnosd = dir.join("lemnosd.sock");
    let files = IdentityFiles::new(&dir, "Serial : 0000000000abcdef\n");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd_board(&lemnosd, &files.board());
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    hello_with(&dashboard, "serial");
    wait_for("the counter's resource", || {
        resource(&dashboard, "lemnos.thing")
    });

    // The counter is an unrelated device: it counts on every read, so a reset would show.
    let mut count = dashboard
        .subscribe("lemnos.thing.values")
        .expect("subscribe");
    let mut status = dashboard
        .subscribe("lemnos.thing.status")
        .expect("subscribe");
    let before = wait_for("the counter counting", || {
        latest(&mut count, 0).and_then(|b| float(&b))
    });
    wait_for("the counter's status live", || {
        let _ = status.wait(Duration::from_millis(50));
        (status.status() == Status::Live).then_some(())
    });

    // A fact comes, then a fact goes: each while the counter keeps counting.
    let mut lowest = before;
    files.boot("os-2", "an-os", "2.0");
    hello_with(&dashboard, "version.slot");
    for _ in 0..10 {
        let _ = status.wait(Duration::from_millis(20));
        assert_ne!(
            status.status(),
            Status::PublisherGone,
            "a fact moved the counter's topic"
        );
        if let Some(value) = latest(&mut count, 0).and_then(|b| float(&b)) {
            lowest = lowest.min(value);
        }
    }
    files.unboot();
    wait_for("the running slot's fact gone", || {
        hello(&dashboard).filter(|facts| !facts.contains_key("version.slot"))
    });
    for _ in 0..10 {
        let _ = status.wait(Duration::from_millis(20));
        assert_ne!(
            status.status(),
            Status::PublisherGone,
            "a fact going moved the counter's topic"
        );
        if let Some(value) = latest(&mut count, 0).and_then(|b| float(&b)) {
            lowest = lowest.min(value);
        }
    }
    assert!(
        lowest >= before,
        "the counter went back: {lowest} < {before}"
    );
    let after = wait_for("the counter counting after the changes", || {
        latest(&mut count, 0)
            .and_then(|b| float(&b))
            .filter(|value| *value > before)
    });
    assert!(after > before);
    assert_eq!(status.status(), Status::Live);
}

/// What a second client sees of the counter while the board changes, read as fast as it can.
#[derive(Debug, Default)]
struct Seen {
    /// Reads in which the counter's values topic was not live: withdrawn, or declared again.
    not_live: u32,
    /// Reads in which the counter was not listed, or not ready.
    not_listed: u32,
    /// Values that went back: a sequence that started again.
    back: u32,
    /// Values that changed as read.
    moved: u32,
    /// Samples lost to this client for reading too slowly.
    missed: u64,
}

/// Reads the counter's topic and resource in a loop, in a thread of its own, until `stop` is set.
fn watch_counter(orion: PathBuf, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<Seen> {
    std::thread::spawn(move || {
        let watcher = dashboard(&orion);
        let mut values = watcher.subscribe("lemnos.thing.values").expect("subscribe");
        let mut status = watcher.subscribe("lemnos.thing.status").expect("subscribe");
        let mut seen = Seen::default();
        let mut last: Option<f64> = None;
        // Counted from the moment both topics are live: before that there is nothing to lose.
        while values.status() != Status::Live || status.status() != Status::Live {
            let _ = values.wait(Duration::from_millis(1));
            let _ = status.wait(Duration::from_millis(1));
        }
        while !stop.load(Ordering::Relaxed) {
            let _ = status.wait(Duration::from_millis(1));
            let _ = values.wait(Duration::from_millis(1));
            if status.status() != Status::Live {
                seen.not_live += 1;
            }
            if values.status() != Status::Live {
                seen.not_live += 1;
            }
            if resource(&watcher, "lemnos.thing")
                .is_none_or(|found| found.state != ResourceState::Ready)
            {
                seen.not_listed += 1;
            }
            let now = values
                .latest(FieldId(0))
                .and_then(|sample| float(sample.payload));
            if let Some(value) = now {
                if last.is_some_and(|before| value < before) {
                    seen.back += 1;
                }
                if last != Some(value) {
                    seen.moved += 1;
                }
                last = Some(value);
            }
        }
        seen.missed = values.missed();
        seen
    })
}

#[test]
fn a_fact_and_a_device_coming_and_going_move_an_unrelated_device_not_at_all() {
    // A second client reads the counter (an unrelated device) the whole time. Each change below
    // used to withdraw and declare the whole session again, which the counter's topic and resource
    // show at once. Now only the fact, or the imu, is said: the counter must not move at all.
    let dir = dir("unrelated");
    let lemnosd = dir.join("lemnosd.sock");
    let files = IdentityFiles::new(&dir, "Serial : 0000000000abcdef\n");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd_board(&lemnosd, &files.board());
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    hello_with(&dashboard, "serial");
    wait_for("the counter's resource, ready", || {
        resource(&dashboard, "lemnos.thing").filter(|found| found.state == ResourceState::Ready)
    });
    let imu = lemnos_client::Client::connect(&lemnosd, "test")
        .expect("lemnosd")
        .list()
        .expect("list")
        .into_iter()
        .find(|device| device.name == "imu")
        .expect("the imu")
        .index;
    let client = lemnos_client::Client::connect(&lemnosd, "test-power").expect("lemnosd");

    let stop = Arc::new(AtomicBool::new(false));
    let watch = watch_counter(board.socket().to_path_buf(), Arc::clone(&stop));
    std::thread::sleep(Duration::from_millis(200));

    // A fact comes, the imu goes and comes back, then the fact goes. Each one is given time.
    files.boot("os-2", "an-os", "2.0");
    hello_with(&dashboard, "version.slot");
    std::thread::sleep(Duration::from_millis(200));
    client.set_power(imu, false).expect("the imu off");
    std::thread::sleep(Duration::from_millis(200));
    client.set_power(imu, true).expect("the imu on");
    std::thread::sleep(Duration::from_millis(200));
    files.unboot();
    wait_for("the running slot's fact gone", || {
        hello(&dashboard).filter(|facts| !facts.contains_key("version.slot"))
    });
    std::thread::sleep(Duration::from_millis(200));

    stop.store(true, Ordering::Relaxed);
    let seen = watch.join().expect("the watcher");
    assert!(seen.moved > 0, "the counter did not count: {seen:?}");
    assert_eq!(
        seen.not_live, 0,
        "the counter's topic was withdrawn: {seen:?}"
    );
    assert_eq!(seen.not_listed, 0, "the counter was not listed: {seen:?}");
    assert_eq!(
        seen.back, 0,
        "the counter's sequence started again: {seen:?}"
    );
    assert_eq!(seen.missed, 0, "samples were lost: {seen:?}");
}

/// The tracked device, with a dashboard session and a second Orion session to run long jobs from.
/// Orion names every local session by the same caller (`local:<uid>`), so the bridge keeps one
/// lemnosd connection for them, and the driver sees one caller: what the holds below count.
fn start_tracked(dir: &Path) -> (Board, Logic, Service, Bridge, Orion) {
    start_tracked_leased(dir, lemnos_orion::config::LEASE)
}

fn start_tracked_leased(dir: &Path, lease: Duration) -> (Board, Logic, Service, Bridge, Orion) {
    start_tracked_opt(dir, Some(lease))
}

fn start_tracked_opt(
    dir: &Path,
    lease: Option<Duration>,
) -> (Board, Logic, Service, Bridge, Orion) {
    let lemnosd = dir.join("lemnosd.sock");
    let board_text = format!("{BOARD}[[devices]]\nid = \"tracked\"\ndriver = \"tracked\"\n");
    let board = Board::start_for_node(dir);
    let logic = Logic::start(dir);
    let service = start_lemnosd_with(
        &lemnosd,
        &board_text,
        vec![
            entry::<FakeAccelerometer>(),
            entry::<FakeCounter>(),
            entry::<TrackedDevice>(),
        ],
    );
    let bridge = start_bridge_opt(&lemnosd, board.socket(), None, lease);
    let dashboard = dashboard(board.socket());
    wait_for("the tracked device's resource", || {
        resource(&dashboard, "lemnos.tracked").filter(|found| found.state == ResourceState::Ready)
    });
    (board, logic, service, bridge, dashboard)
}

/// Runs `args` on the tracked device, and its reply as text.
fn tracked_call(orion: &Orion, args: &str) -> String {
    let finished = run_once_said(orion, "lemnos.tracked.call", "lemnos.tracked", args);
    assert_eq!(finished.outcome, Outcome::Succeeded, "{finished:?}");
    String::from_utf8(finished.output).expect("text")
}

/// `call describe` answers the device's description as version 1 text: the version line, then the
/// device's record (with its description generation), its contracts, its methods and its items.
/// It takes no arguments. The text is read back with the module's reader.
#[test]
fn call_describe_answers_the_device_description_as_text() {
    let dir = dir("describe");
    let (_board, _logic, _service, _bridge, dashboard) = start_tracked(&dir);
    let text = tracked_call(&dashboard, "describe");
    assert!(text.starts_with("describe 1\n"), "the version line: {text}");
    let records = lemnos_orion::description::parse(&text).expect("the reader reads it");
    let device = records.iter().find(|r| r.word == "device").expect("device");
    assert_eq!(device.get("label"), Some("tracked"), "{text}");
    assert_eq!(device.get("state"), Some("running"), "{text}");
    assert_eq!(device.get("generation"), Some("1"), "{text}");
    let contract = records
        .iter()
        .find(|r| r.word == "contract")
        .expect("contract");
    assert_eq!(contract.get("index"), Some("0"));
    assert_eq!(contract.get("tag"), Some("tracked/1"));
    let method = |id: &str| {
        records
            .iter()
            .find(|r| r.word == "method" && r.get("id") == Some(id))
            .unwrap_or_else(|| panic!("method {id}: {text}"))
    };
    assert_eq!(
        device.get("lease_seconds"),
        Some("60"),
        "the lease a client renews within"
    );
    assert_eq!(method("1").get("name"), Some("take"));
    assert_eq!(method("4").get("name"), Some("hold_until_cancelled"));
    assert_eq!(
        method("4").get("job"),
        Some("true"),
        "the long job is marked"
    );
    // A description with arguments is refused, and nothing is sent.
    let finished = run(
        &dashboard,
        "lemnos.tracked.call",
        "lemnos.tracked",
        "describe x",
    );
    assert_eq!(finished.outcome, Outcome::Rejected, "{finished:?}");
    assert!(finished.output.is_empty(), "{finished:?}");
    assert_eq!(finished.detail, "describe takes no arguments");
}

/// The lease the hold tests use: a second, so a test waits seconds rather than the default minute.
const TEST_LEASE: Duration = Duration::from_secs(1);

/// A second Orion session of this user: it runs long jobs, and it is the one a test ends.
fn second_session(board: &Board) -> Orion {
    wait_for("a second Orion session", || {
        Orion::connect(&Address::Local(board.socket().to_path_buf()), "caller").ok()
    })
}

/// A caller that goes away (its session ends) and whose holds are idle: the holds are released
/// within its lease. Every local session is one caller, so the dashboard is the caller's lemnosd
/// connection too; it stays silent so that nothing renews the lease. Mutation: with the lease's
/// `expire` made a no-op the holds stay (`held` is 1) and this test fails.
#[test]
fn a_dead_callers_idle_holds_are_released_within_its_lease() {
    let dir = dir("lease-dead-idle");
    let (board, _logic, _service, _bridge, dashboard) = start_tracked_leased(&dir, TEST_LEASE);
    let caller = second_session(&board);
    let finished = run(&caller, "lemnos.tracked.call", "lemnos.tracked", "take");
    assert_eq!(finished.outcome, Outcome::Succeeded, "{finished:?}");
    // The caller's session ends.
    drop(caller);
    // Its lease (and one more tick) runs out with nothing sent on the connection.
    std::thread::sleep(TEST_LEASE * 3);
    assert_eq!(
        tracked_call(&dashboard, "held"),
        "0",
        "the dead caller's hold outlived its lease"
    );
    // The bridge says so in its hello document: the lease, and how many leases have ended.
    let facts = hello(&dashboard).expect("the hello document");
    assert_eq!(facts.get("lease.seconds").map(String::as_str), Some("1"));
    assert_eq!(
        facts.get("leases.ended").map(String::as_str),
        Some("1"),
        "{facts:?}"
    );
}

/// A dropped caller is told once: its first reply after the lease ended is served normally, on a fresh
/// connection, with its holds gone, and its `detail` begins with the note `lease-ended:`. The reply
/// after it carries no note. The hello document's `leases.ended` counts the drop for everyone.
#[test]
fn a_dropped_caller_is_told_once_by_the_note_on_its_next_reply() {
    let dir = dir("lease-dropped-reply");
    let (board, _logic, _service, _bridge, dashboard) = start_tracked_leased(&dir, TEST_LEASE);
    let caller = second_session(&board);
    let finished = run(&caller, "lemnos.tracked.call", "lemnos.tracked", "take");
    assert_eq!(finished.outcome, Outcome::Succeeded, "{finished:?}");
    drop(caller);
    std::thread::sleep(TEST_LEASE * 3);
    let first = run(&dashboard, "lemnos.tracked.call", "lemnos.tracked", "held");
    assert_eq!(first.outcome, Outcome::Succeeded, "{first:?}");
    assert_eq!(String::from_utf8(first.output).expect("text"), "0");
    assert_eq!(
        first.detail,
        lemnos_orion::calls::LEASE_ENDED_NOTE,
        "the first reply after the drop carries the note"
    );
    let second = run(&dashboard, "lemnos.tracked.call", "lemnos.tracked", "held");
    assert_eq!(second.outcome, Outcome::Succeeded, "{second:?}");
    assert_eq!(second.detail, "", "the note is given once");
    let facts = hello(&dashboard).expect("the hello document");
    assert_eq!(facts.get("leases.ended").map(String::as_str), Some("1"));
}

/// A caller that is alive and renews its lease keeps its holds for as long as it renews them.
/// `describe` is the renewal with nothing to do. Mutation: a lease that expires at once releases
/// the hold and this test fails.
#[test]
fn an_alive_caller_that_renews_its_lease_keeps_its_holds() {
    let dir = dir("lease-alive");
    let (_board, _logic, _service, _bridge, dashboard) = start_tracked_leased(&dir, TEST_LEASE);
    tracked_call(&dashboard, "take");
    assert_eq!(tracked_call(&dashboard, "held"), "1");
    let until = Instant::now() + TEST_LEASE * 3;
    while Instant::now() < until {
        std::thread::sleep(TEST_LEASE / 3);
        tracked_call(&dashboard, "describe");
    }
    assert_eq!(
        tracked_call(&dashboard, "held"),
        "1",
        "a caller that renewed its lease lost its hold"
    );
}

/// A caller whose session ends while its long job runs: the job is cancelled at once (the Orion
/// Cancel), and the holds it took are released within its lease, after its last request. Mutation:
/// with `expire` a no-op the hold stays and this test fails.
#[test]
fn a_dead_callers_long_job_ends_and_its_holds_are_released_within_its_lease() {
    let dir = dir("lease-dead-job");
    let (board, _logic, _service, _bridge, dashboard) = start_tracked_leased(&dir, TEST_LEASE);
    let caller = second_session(&board);
    let finished = run(&caller, "lemnos.tracked.call", "lemnos.tracked", "take");
    assert_eq!(finished.outcome, Outcome::Succeeded, "{finished:?}");
    let _long = caller
        .run(&ActionCall {
            name: "lemnos.tracked.call",
            target: Target::Resource("lemnos.tracked".parse().expect("name")),
            args: b"hold_until_cancelled",
            deadline_ms: 0,
            detached: false,
        })
        .expect("run");
    std::thread::sleep(Duration::from_millis(300));
    drop(_long);
    drop(caller);
    std::thread::sleep(TEST_LEASE * 3);
    assert_eq!(
        tracked_call(&dashboard, "held"),
        "0",
        "the dead caller's hold outlived its lease after its long job was cancelled"
    );
}

/// A silent long job keeps its caller alive: a caller with any run in flight is alive, whether or not
/// the run reports progress, so its other hold stays for three leases while the job runs with the
/// caller sending nothing. Cancelling the run ends its shield: the caller, still silent, loses its
/// hold one lease after the cancel. Mutation: with the in-flight shield removed the hold is released at
/// the lease and the first check fails.
#[test]
fn a_silent_long_job_keeps_its_callers_other_hold_until_it_is_cancelled() {
    let dir = dir("lease-silent-run");
    let (board, _logic, _service, _bridge, dashboard) = start_tracked_leased(&dir, TEST_LEASE);
    let caller = second_session(&board);
    let finished = run(&caller, "lemnos.tracked.call", "lemnos.tracked", "take");
    assert_eq!(finished.outcome, Outcome::Succeeded, "{finished:?}");
    let mut long = caller
        .run(&ActionCall {
            name: "lemnos.tracked.call",
            target: Target::Resource("lemnos.tracked".parse().expect("name")),
            args: b"hold_until_cancelled",
            deadline_ms: 0,
            detached: false,
        })
        .expect("run");
    // The caller stays silent for three leases while its silent job runs.
    std::thread::sleep(TEST_LEASE * 3);
    let held = run(&dashboard, "lemnos.tracked.call", "lemnos.tracked", "held");
    assert_eq!(
        String::from_utf8(held.output).expect("text"),
        "1",
        "a silent long job lost its caller's hold at the lease"
    );
    long.cancel().expect("cancel");
    let _ = long.finish(WAIT);
    // The run is over, and the caller is silent: its lease counts from the cancel.
    std::thread::sleep(TEST_LEASE * 2);
    assert_eq!(
        tracked_call(&dashboard, "held"),
        "0",
        "the caller's hold outlived its lease after its run was cancelled"
    );
}

/// A long job that reports progress renews its caller's lease: the caller, silent otherwise, keeps
/// its hold for three leases while the job reports. Mutation: with the progress not renewing the
/// lease the hold is released at the lease and this test fails.
#[test]
fn a_long_job_reporting_progress_keeps_its_callers_other_hold() {
    let dir = dir("lease-progress");
    let (_board, _logic, _service, _bridge, dashboard) = start_tracked_leased(&dir, TEST_LEASE);
    tracked_call(&dashboard, "take");
    assert_eq!(tracked_call(&dashboard, "held"), "1");
    let mut long = dashboard
        .run(&ActionCall {
            name: "lemnos.tracked.call",
            target: Target::Resource("lemnos.tracked".parse().expect("name")),
            args: b"count_until_cancelled",
            deadline_ms: 0,
            detached: false,
        })
        .expect("run");
    // The caller sends nothing for three leases: only the job's progress is news.
    std::thread::sleep(TEST_LEASE * 3);
    let held = tracked_call(&dashboard, "held");
    long.cancel().expect("cancel");
    let _ = long.finish(WAIT);
    assert_eq!(
        held, "1",
        "a long job reporting progress lost its caller's hold"
    );
}

/// `LEMNOS_ORION_LEASE=0` turns the lease off: holds stay however long the caller is silent, the
/// hello document has neither lease fact, and the describe reply has no `lease_seconds`.
#[test]
fn a_lease_of_zero_is_off_and_the_bridge_says_nothing_of_one() {
    let dir = dir("lease-off");
    let (_board, _logic, _service, _bridge, dashboard) = start_tracked_opt(&dir, None);
    tracked_call(&dashboard, "take");
    std::thread::sleep(TEST_LEASE * 3);
    assert_eq!(
        tracked_call(&dashboard, "held"),
        "1",
        "a caller with the lease off lost its hold"
    );
    let facts = hello(&dashboard).expect("the hello document");
    assert!(!facts.contains_key("lease.seconds"), "{facts:?}");
    assert!(!facts.contains_key("leases.ended"), "{facts:?}");
    let text = tracked_call(&dashboard, "describe");
    assert!(!text.contains("lease_seconds"), "{text}");
}

#[test]
fn a_plain_cancel_ends_the_run_and_keeps_the_callers_holds() {
    let dir = dir("cancel");
    let (board, _logic, _service, _bridge, dashboard) = start_tracked(&dir);
    // Its holds are taken first, so the cancel below has something to lose.
    tracked_call(&dashboard, "take");
    assert_eq!(tracked_call(&dashboard, "held"), "1");

    // The caller is still there: it cancels its own long job. Only the run ends: the caller's
    // driver holds stay, and the driver's own connection is not told the caller is gone.
    let caller = wait_for("a second Orion session", || {
        Orion::connect(&Address::Local(board.socket().to_path_buf()), "caller").ok()
    });
    let mut long = caller
        .run(&ActionCall {
            name: "lemnos.tracked.call",
            target: Target::Resource("lemnos.tracked".parse().expect("name")),
            args: b"hold_until_cancelled",
            deadline_ms: 0,
            detached: false,
        })
        .expect("run");
    std::thread::sleep(Duration::from_millis(300));
    long.cancel().expect("cancel");
    let finished = long.finish(WAIT).expect("the cancelled run ends");
    assert_eq!(finished.outcome, Outcome::Cancelled, "{finished:?}");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        tracked_call(&dashboard, "held"),
        "1",
        "a plain cancel told the driver its caller is gone"
    );
}

#[test]
fn a_caller_that_goes_away_ends_its_run_and_its_holds_wait_for_orion() {
    let dir = dir("callers");
    let (board, _logic, _service, _bridge, dashboard) = start_tracked(&dir);
    tracked_call(&dashboard, "take");
    let who = tracked_call(&dashboard, "who");
    assert_ne!(who, "0", "the caller is not the local caller");
    assert_eq!(
        tracked_call(&dashboard, "who"),
        who,
        "one caller, one number"
    );
    assert_eq!(
        tracked_call(&dashboard, "held"),
        "1",
        "the driver holds for the caller"
    );

    // The caller starts a long job, then goes away while it runs. Orion cancels its run with the
    // same cancel a caller's explicit cancel is sent with, so the bridge cannot tell them apart.
    let caller = wait_for("a second Orion session", || {
        Orion::connect(&Address::Local(board.socket().to_path_buf()), "caller").ok()
    });
    let long = caller
        .run(&ActionCall {
            name: "lemnos.tracked.call",
            target: Target::Resource("lemnos.tracked".parse().expect("name")),
            args: b"hold_until_cancelled",
            deadline_ms: 0,
            detached: false,
        })
        .expect("run");
    // The bridge takes the run and starts the job on lemnosd; a moment for that to happen.
    std::thread::sleep(Duration::from_millis(300));
    drop(long);
    drop(caller);
    // The run's job is over on the driver: a new long job starts (it is refused while one runs).
    wait_for("the caller's job is over on the driver", || {
        let mut run = dashboard
            .run(&ActionCall {
                name: "lemnos.tracked.call",
                target: Target::Resource("lemnos.tracked".parse().expect("name")),
                args: b"hold_until_cancelled",
                deadline_ms: 0,
                detached: false,
            })
            .ok()?;
        match run.finish(Duration::from_millis(200)) {
            // Refused at once: the driver is still busy with the caller's job.
            Ok(_) => None,
            Err(ClientError::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut => {
                // Started: it is the one that is wanted; cancel it again so the device is free.
                let _ = run.cancel();
                Some(())
            }
            Err(_) => None,
        }
    });
    // ORION-R2: at R2 the bridge reads a session end as `caller_gone` (reason 2), and the driver
    // drops the caller's holds: `held` is then "0". At this revision the same cancel says no more
    // than a plain one, so the holds stay.
    assert_eq!(
        tracked_call(&dashboard, "held"),
        "1",
        "the holds stay until Orion says why a run was cancelled"
    );
}

#[test]
fn values_keep_the_time_the_driver_read_them_on_the_monotonic_clock() {
    let dir = dir("stamps");
    let lemnosd = dir.join("lemnosd.sock");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd(&lemnosd);
    let before = SampleTime::now();
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    wait_for("the counter's resource", || {
        resource(&dashboard, "lemnos.thing")
    });
    let mut count = dashboard
        .subscribe("lemnos.thing.values")
        .expect("subscribe");
    let (_, time) = wait_for("a counter value", || latest_sample(&mut count, 0));
    // A monotonic time since the start of the test, not the wall clock (which is far larger).
    assert!(time >= before, "{time:?} is before the start of the test");
    assert!(
        time <= SampleTime::now(),
        "{time:?} is in the future of the monotonic clock: a wall-clock stamp"
    );
}

#[test]
fn an_idle_session_is_kept_while_nothing_changes() {
    let dir = dir("idle");
    let lemnosd = dir.join("lemnosd.sock");
    // The comm core gives a quiet session 2 s, with a 50 ms heartbeat. The bridge pings, so it is
    // never quiet for that long, and nothing is said to Orion meanwhile. The idle time is long so
    // that only a bridge thread not run for 2 s could be dropped: a shorter one let a loaded machine
    // starve the bridge past the idle time (the comm core then closes the session, correctly).
    const IDLE_MS: u32 = 2000;
    let board = Board::start_with(
        &dir,
        BoardOptions {
            for_node: true,
            heartbeat_ms: 50,
            idle_ms: IDLE_MS,
            ..BoardOptions::default()
        },
    );
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd(&lemnosd);
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    wait_for("the imu's resource", || {
        resource(&dashboard, "lemnos.imu").filter(|found| found.state == ResourceState::Ready)
    });
    // The dashboard's own session must keep pinging too, or the comm core closes it. Two and a half
    // idle times: the comm core would have closed a quiet session several times over.
    let until = Instant::now() + Duration::from_millis(u64::from(IDLE_MS) * 5 / 2);
    while Instant::now() < until {
        let _ = dashboard.pump(Duration::from_millis(20));
    }
    let imu = resource(&dashboard, "lemnos.imu").expect("still published");
    assert_eq!(imu.state, ResourceState::Ready);
    let peek = run(&dashboard, "lemnos.thing.call", "lemnos.thing", "peek");
    assert_eq!(peek.outcome, Outcome::Succeeded, "{peek:?}");
}

#[test]
fn a_comm_core_with_a_short_idle_time_is_connected_to_again() {
    let dir = dir("short-idle");
    let lemnosd = dir.join("lemnosd.sock");
    let options = || BoardOptions {
        for_node: true,
        heartbeat_ms: 50,
        idle_ms: 400,
        ..BoardOptions::default()
    };
    let board = Board::start_with(&dir, options());
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd(&lemnosd);
    let socket = board.socket().to_path_buf();
    let _bridge = start_bridge(&lemnosd, &socket, None);
    {
        let dashboard = dashboard(&socket);
        wait_for("the imu's resource", || resource(&dashboard, "lemnos.imu"));
    }
    // The comm core goes away: the bridge notices within the idle time and connects again when it
    // is back.
    drop(board);
    let board = Board::start_with(&dir, options());
    let dashboard = dashboard(board.socket());
    wait_for("the imu's resource after the comm core came back", || {
        resource(&dashboard, "lemnos.imu").filter(|found| found.state == ResourceState::Ready)
    });
    let peek = run_once_said(&dashboard, "lemnos.thing.call", "lemnos.thing", "peek");
    assert_eq!(peek.outcome, Outcome::Succeeded, "{peek:?}");
}

/// A reply the bridge sends as the run's output: its bytes when they are not all hex digits (a
/// record has zero bytes, so it is always the bytes), else the hex text the bridge wrote.
fn reply(output: &[u8]) -> Vec<u8> {
    if output.iter().all(u8::is_ascii_hexdigit) && !output.is_empty() {
        let text = std::str::from_utf8(output).expect("ascii");
        return (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
            .collect();
    }
    output.to_vec()
}

/// A run started through the bridge, with the progress it reports on the way: each update's
/// `done` and `total`, as the run's progress data carries them (little-endian), then the end.
fn run_watched(dashboard: &Orion, device: &str, args: &str) -> (Vec<(u64, u64)>, Finished) {
    let action = format!("lemnos.{device}.call");
    let mut run = dashboard
        .run(&ActionCall {
            name: &action,
            target: Target::Resource(format!("lemnos.{device}").parse().expect("name")),
            args: args.as_bytes(),
            deadline_ms: 0,
            detached: false,
        })
        .expect("run");
    let mut seen = Vec::new();
    loop {
        match run.wait(WAIT).expect("news of the run") {
            Some(Update::Doing(doing)) => {
                let done = u64::from_le_bytes(doing.data[..8].try_into().expect("done"));
                let total = u64::from_le_bytes(doing.data[8..16].try_into().expect("total"));
                assert_eq!(doing.message, format!("{done} of {total}"));
                seen.push((done, total));
            }
            Some(Update::Finished(finished)) => return (seen, finished),
            None => panic!("the run went quiet"),
        }
    }
}

/// lemnosd over a self-test member (the probe) and the board's own self-test over it.
fn start_lemnosd_self_test(socket: &Path) -> Service {
    let board = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
        [[devices]]\nid = \"a\"\ndriver = \"fake-probe\"\n\
        [[devices]]\nid = \"selftest\"\ndriver = \"board-self-test\"\n\
        config = { members = [\"a\"] }\n";
    start_lemnosd_with(
        socket,
        board,
        vec![entry::<FakeProbe>(), entry::<BoardSelfTest>()],
    )
}

#[test]
fn the_board_self_test_is_listed_run_and_read_through_the_bridge() {
    let dir = dir("self-test");
    let lemnosd = dir.join("lemnosd.sock");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd_self_test(&lemnosd);
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    wait_for("the self-test resource", || {
        resource(&dashboard, "lemnos.selftest")
    });

    // `tests` is an out-buffer method: the bridge sends the size, and the reply is the list.
    let listed = run_once_said(
        &dashboard,
        "lemnos.selftest.call",
        "lemnos.selftest",
        "tests",
    );
    assert_eq!(listed.outcome, Outcome::Succeeded, "{listed:?}");
    let names: Vec<String> = read_tests(&reply(&listed.output))
        .expect("the list")
        .map(|test| test.name.as_str().to_string())
        .collect();
    assert_eq!(names.first().map(String::as_str), Some("a:ok"), "{names:?}");
    assert_eq!(names.last().map(String::as_str), Some("all"), "{names:?}");

    // `run` is a long job: its end is the run's end, and its result is the record.
    let run = run_once_said(
        &dashboard,
        "lemnos.selftest.call",
        "lemnos.selftest",
        "run a:bad false",
    );
    assert_eq!(run.outcome, Outcome::Succeeded, "{run:?}");
    let answer = Record::read(&mut Reader::new(&reply(&run.output))).expect("a record");
    assert_eq!(answer.outcome, SelfOutcome::Fail);
    assert_eq!(
        answer.reason,
        lemnos_contract_self_test::reason::OUT_OF_RANGE
    );
    assert_eq!(reply(&run.output).len(), RECORD_BYTES);

    // `last` gives the result the run left.
    let last = run_once_said(
        &dashboard,
        "lemnos.selftest.call",
        "lemnos.selftest",
        "last",
    );
    assert_eq!(last.outcome, Outcome::Succeeded, "{last:?}");
    let kept: Vec<String> = read_last(&reply(&last.output))
        .expect("the last results")
        .map(|entry| entry.name.as_str().to_string())
        .collect();
    assert!(kept.contains(&"a:bad".to_string()), "{kept:?}");

    // A slow test is a long job whose progress reaches the bridge's client. The board's own run
    // reports steps, so this goes to the member: `slow` waits a second, and each update is the
    // milliseconds done of its bound (1000).
    wait_for("the member", || resource(&dashboard, "lemnos.a"));
    let (seen, slow) = run_watched(&dashboard, "a", "run slow false");
    assert_eq!(slow.outcome, Outcome::Succeeded, "{slow:?}");
    let record = Record::read(&mut Reader::new(&reply(&slow.output))).expect("a record");
    assert_eq!(record.outcome, SelfOutcome::Pass);
    assert!(
        !seen.is_empty(),
        "the run's progress came back through the bridge"
    );
    assert!(seen.iter().all(|&(_, total)| total == 1000), "{seen:?}");
    assert!(seen.windows(2).all(|w| w[0].0 <= w[1].0), "{seen:?}");
    assert!(seen.iter().all(|&(done, _)| done <= 1000), "{seen:?}");
}

/// A device that cannot come up is described through the bridge all the same: its device record
/// says its state and why, it is not live, and it has no items. Its contracts and methods come from
/// the manifest, which lemnosd has for every device on the board.
#[test]
fn a_device_that_cannot_come_up_is_described_with_its_state_and_reason() {
    let dir = dir("refused");
    let lemnosd = dir.join("lemnosd.sock");
    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd_refused(&lemnosd);
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    wait_for("the refused device's resource", || {
        resource(&dashboard, "lemnos.bad")
    });
    let done = wait_for("the refused device's describe", || {
        let done = run_once_said(&dashboard, "lemnos.bad.call", "lemnos.bad", "describe");
        (done.outcome == Outcome::Succeeded).then_some(done)
    });
    let text = String::from_utf8(done.output).expect("text");
    let records = lemnos_orion::description::parse(&text).expect("the reader reads it");
    let device = records.iter().find(|r| r.word == "device").expect("device");
    assert_eq!(device.get("live"), Some("false"), "{text}");
    assert_eq!(device.get("state"), Some("retrying"), "{text}");
    let reason = device.get("reason").expect("a reason");
    assert!(!reason.is_empty(), "the reason is said: {text}");
    assert!(
        records.iter().all(|r| r.word != "item"),
        "a device that does not run has no items: {text}"
    );
    assert!(
        records.iter().any(|r| r.word == "method"),
        "its methods come from the manifest: {text}"
    );
}
