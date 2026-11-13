//! The service against a real lemnosd (its service, over the simulated board, with the fake
//! accelerometer and counter), a real Orion comm core and real Orion node logic, in-process.
//!
//! Generic: the service has no code for either device. The devices are named only in the board
//! text and in the names the checks read. What the checks see is what any Orion program sees:
//! resources found by prefix, topics read by subscribing, actions run by name.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
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
use orion::provider::{Found, ResourceState};
use orion::topics::{FieldId, Status, Subscriber};
use orion::types::Error as Refusal;
use orion::{Address, ClientError, Orion};
use orion_node_app::{Node, NodeSettings};
use orion_stubs::Board;
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
    let shared = Arc::new(Shared::new().expect("shared"));
    let config = Config {
        lemnosd_socket: lemnosd.to_path_buf(),
        orion: Address::Local(orion.to_path_buf()),
        node_id: node.map(str::to_owned),
        run_as: None,
        files: Vec::new(),
        links: Vec::new(),
    };
    let (jobs, queue) = mpsc::channel();
    let threads = vec![
        {
            let (socket, shared) = (lemnosd.to_path_buf(), Arc::clone(&shared));
            std::thread::spawn(move || source::run(&socket, &shared))
        },
        {
            let (socket, shared) = (lemnosd.to_path_buf(), Arc::clone(&shared));
            std::thread::spawn(move || calls::run(&socket, &queue, &shared))
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
    assert_eq!(thing.topics, ["lemnos.thing.values", "lemnos.thing.status"]);
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
    wait_for(
        "the counter's resource and its call action after the restart",
        || {
            resource(&dashboard, "lemnos.thing").filter(|found| {
                found.state == ResourceState::Ready
                    && found.actions.contains(&"lemnos.thing.call".to_owned())
            })
        },
    );
    let mut values = dashboard.subscribe("lemnos.imu.values").expect("subscribe");
    wait_for("the imu's x after the restart", || {
        (latest(&mut values, 0).and_then(|b| float(&b)) == Some(1.5)).then_some(())
    });
    let peek = run_once_said(&dashboard, "lemnos.thing.call", "lemnos.thing", "peek");
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
        ["lemnos.thing.status"],
        "nothing described, no values"
    );

    // Switched on, it is described, and its values and controls appear.
    client.set_power(counter.index, true).expect("on");
    let on = wait_for("the counter described", || {
        resource(&dashboard, "lemnos.thing").filter(|found| found.topics.len() == 2)
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
        ["family", "model", "offers", "version.lemnos"],
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
