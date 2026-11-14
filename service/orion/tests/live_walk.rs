//! The out-buffer methods the simulated board hosts, called through the bridge's `.call` action
//! against a real Orion core and real lemnosd, each reply checked for its content. The devices:
//! the fake accelerometer and gyroscope, the identity and slots drivers, a raw bus on the simulated
//! I2C, SPI and UART, a USB gadget on a mock function with a data stream, and an indicator on a
//! WS2812 strip on a second simulated SPI. The methods none of these can host are named in the
//! bridge's docs as covered by the text path only.
#![allow(clippy::print_stderr)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use lemnos_contract_indicator::STATES;
use lemnos_driver_indicator::Indicator;
use lemnos_driver_raw_bus::RawBus;
use lemnos_driver_system_identity::Identity;
use lemnos_driver_system_slots::SystemSlots;
use lemnos_driver_usb_gadget::UsbGadget;
use lemnos_driver_ws2812::Ws2812;
use lemnos_fake_devices::{FakeAccelerometer, FakeGyroscope};
use lemnos_host::board::parse;
use lemnos_host::{Options as HostOptions, entry};
use lemnos_orion::config::Config;
use lemnos_orion::{Shared, calls, publish, source};
use lemnos_sim::{Layout, SimBoard};
use lemnosd::{Config as LemnosdConfig, Service};
use orion::actions::{ActionCall, Finished, Outcome, Target};
use orion::provider::{Found, ResourceState};
use orion::topics::{FieldId, Next, Subscriber};
use orion::{Address, Orion};
use orion_node_app::{Node, NodeSettings};
use orion_stubs::Board;
use tokio::sync::oneshot;

const WAIT: Duration = Duration::from_secs(20);
/// How long a power-on's changes must stay quiet, and the device must hold its state, before the
/// test reads the generation: longer than a missed bind's first retry (one second).
const SETTLE: Duration = Duration::from_millis(2500);

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A fresh directory for one test. Unix socket paths are short, so the name is short.
fn dir(test: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("lo-{test}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test directory");
    dir
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

/// A run of an action, asked once. Orion restores topics, then actions, then records after a comm core
/// restart, so a resource is never listed before its actions are back (C23).
fn run(dashboard: &Orion, action: &str, resource: &str, args: &str) -> Finished {
    let mut run = dashboard
        .run(&ActionCall {
            name: action,
            target: Target::Resource(resource.parse().expect("name")),
            args: args.as_bytes(),
            deadline_ms: lemnos_orion::calls::RUN_DEADLINE_MS,
            detached: false,
        })
        .expect("run");
    run.finish(WAIT)
        .unwrap_or_else(|error| panic!("{action} {args:?} on {resource}: {error:?}"))
}

const CPUINFO: &str = "Serial\t\t: 0000000000ABCDEF\n";
const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const WAIT_NAMES: [&str; 8] = [
    "imu", "gyro", "identity", "slots", "bus", "gadget", "strip", "lights",
];

fn manifest(slot: &str, product: &str, version: &str) -> String {
    format!(
        "{{\"schema\":3,\"id\":\"{slot}\",\"product\":\"{product}\",\"version\":\"{version}\",\"board\":\"walk\",\
         \"image\":{{\"size\":4096,\"sha256\":\"{HASH}\"}},\
         \"verity\":{{\"root_hash\":\"{HASH}\",\"salt\":\"{HASH}\",\"data_blocks\":1,\"hash_offset\":4096}},\
         \"boot\":{{\"files\":{{\"cmdline.txt\":\"{HASH}\"}}}}}}"
    )
}

/// The image of slot `id` under `dir`, 4096 bytes as its manifest says.
fn image(dir: &Path, id: &str) {
    let file =
        std::fs::File::create(dir.join("data/slots").join(format!("{id}.erofs"))).expect("image");
    file.set_len(4096).expect("image size");
}

/// The board: the fakes, the identity and slots drivers on their files, a raw bus, a USB gadget,
/// and an indicator on a strip.
fn board_text(dir: &Path) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"walk\"\n\
         [[devices]]\nid = \"imu\"\ndriver = \"fake-accelerometer\"\nconfig = {{ x = 0.0, y = 0.0, z = 9.81 }}\n\
         [[devices]]\nid = \"gyro\"\ndriver = \"fake-gyroscope\"\nconfig = {{ x = 0.0, y = 0.0, z = 0.0 }}\n\
         [[devices]]\nid = \"identity\"\ndriver = \"system-identity\"\n\
         config = {{ serial = \"cpuinfo\", serial_path = \"{cpuinfo}\", name = \"Alpha\", revision = \"r2\", data = \"{data}\" }}\n\
         [[devices]]\nid = \"slots\"\ndriver = \"system-slots\"\n\
         config = {{ data = \"{data}\", boot = \"{boot}\", record = \"file\", partuuid = \"abcd-02\", boot_args = \"os\", reserve_mb = 0, running = \"\" }}\n\
         [[devices]]\nid = \"bus\"\ndriver = \"raw-bus\"\nuses = {{ i2c = \"i2c\", spi = \"spi\", uart = \"uart\" }}\n\
         config = {{ writes = true, allow_i2c = 80 }}\n\
         [[devices]]\nid = \"gadget\"\ndriver = \"usb-gadget\"\nuses = {{ function = \"usb\", data = \"data\" }}\n\
         config = {{ vendor = 4617, product = 1, serial = \"S0\", interfaces = \"interface 0xff 0 0 pipe\\nendpoint in bulk\\nendpoint out bulk\\nendpoint in interrupt 16 10\\n\" }}\n\
         [[devices]]\nid = \"strip\"\ndriver = \"ws2812\"\nbus = \"strip-spi\"\nconfig = {{ count = 4 }}\n\
         [[devices]]\nid = \"lights\"\ndriver = \"indicator\"\nuses = {{ pixels = \"strip\" }}\n",
        cpuinfo = dir.join("cpuinfo").display(),
        data = dir.join("data").display(),
        boot = dir.join("boot").display(),
    )
}

/// The walk board, started: lemnosd on it, the bridge, and Orion's dashboard once every device
/// is a resource. `sim` is the simulated board lemnosd runs on, kept by the caller to change it.
struct Walk {
    _board: Board,
    _logic: Logic,
    _service: Service,
    _bridge: Bridge,
    dashboard: Orion,
}

fn start_walk(dir: &Path, sim: SimBoard) -> Walk {
    let lemnosd = dir.join("lemnosd.sock");
    let board = Board::start_for_node(dir);
    let logic = Logic::start(dir);
    let service = Service::start(LemnosdConfig {
        socket: lemnosd.clone(),
        name: "t".to_string(),
        board: parse(&board_text(dir)).expect("board"),
        drivers: vec![
            entry::<FakeAccelerometer>(),
            entry::<FakeGyroscope>(),
            entry::<Identity>(),
            entry::<SystemSlots>(),
            entry::<RawBus>(),
            entry::<UsbGadget>(),
            entry::<Ws2812>(),
            entry::<Indicator>(),
        ],
        backend: Box::new(sim),
        options: HostOptions::default(),
    })
    .expect("lemnosd");
    let bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    // Each device is listed. A device whose bind misses its deadline under load is still retrying
    // for a moment, and a call to it is asked once, by `run`.
    for device in WAIT_NAMES {
        wait_for(device, || resource(&dashboard, &format!("lemnos.{device}")));
    }
    Walk {
        _board: board,
        _logic: logic,
        _service: service,
        _bridge: bridge,
        dashboard,
    }
}

/// The text of a run that succeeded.
fn said(finished: Finished) -> String {
    assert_eq!(finished.outcome, Outcome::Succeeded, "{finished:?}");
    String::from_utf8(finished.output).expect("text")
}

/// A run that was refused: no output, and the detail names it.
fn refused(finished: Finished) -> String {
    assert_ne!(finished.outcome, Outcome::Succeeded, "{finished:?}");
    assert!(
        finished.output.is_empty(),
        "a refusal carries no output: {finished:?}"
    );
    finished.detail
}

#[test]
fn the_out_buffer_methods_the_simulator_hosts_answer_through_the_bridge() {
    let dir = dir("walk");
    std::fs::write(dir.join("cpuinfo"), CPUINFO).expect("cpuinfo");
    std::fs::create_dir_all(dir.join("data/slots")).expect("slots");
    std::fs::create_dir_all(dir.join("data/board/slots")).expect("marker");
    std::fs::create_dir_all(dir.join("boot/slots")).expect("boot slots");
    std::fs::write(
        dir.join("data/slots/os-2.json"),
        manifest("os-2", "an-os", "2.0"),
    )
    .expect("manifest");
    image(&dir, "os-2");
    std::fs::write(dir.join("data/board/slots/booted"), "os-2\n").expect("booted");

    let sim = SimBoard::new();
    let i2c = sim.add_i2c("i2c");
    i2c.attach(0x50, Layout::Bytes);
    i2c.set_bytes(0x50, 0x10, &[0xaa, 0xbb]);
    let spi = sim.add_spi("spi");
    spi.respond(&[0xef, 0x40, 0x18]);
    sim.add_spi("strip-spi");
    let uart = sim.add_uart("uart");
    uart.receive(b"OK\r\nmore");
    sim.add_usb_function("usb");
    sim.add_stream("data", 8);

    let walk = start_walk(&dir, sim);
    let dashboard = &walk.dashboard;
    let call = |device: &str, words: &str| {
        run(
            dashboard,
            &format!("lemnos.{device}.call"),
            &format!("lemnos.{device}"),
            words,
        )
    };

    // system-identity: the three facts and the list of them.
    assert_eq!(said(call("identity", "serial")), "0000000000abcdef");
    assert_eq!(said(call("identity", "family")), "walk");
    assert_eq!(said(call("identity", "model")), "Alpha r2");
    let facts = said(call("identity", "facts"));
    for line in ["serial=0000000000abcdef", "family=walk", "model=Alpha r2"] {
        assert!(facts.contains(line), "{facts}");
    }

    // system-slots: the list, with the slot on disk.
    let list = said(call("slots", "list"));
    assert!(list.starts_with("{\"slot_active\":"), "{list}");
    assert!(list.contains("\"id\":\"os-2\""), "{list}");

    // raw-bus: an I2C read of two bytes from register 0x10 of 0x50, an SPI read of three bytes
    // after a command, and a UART read of four bytes.
    assert_eq!(said(call("bus", "i2c_transfer 80 10 2")), "aabb");
    assert_eq!(said(call("bus", "spi_transfer 9f000000 4")), "ef401800");
    assert_eq!(said(call("bus", "uart_read 4 10")), "OK\r\n");

    // usb-function: the data stream's name. control_out has no request pending, so it is refused.
    assert_eq!(said(call("gadget", "data_stream")), "data");
    // No request is pending, so the call is INVALID_ARGUMENT (-22).
    let refusal = refused(call("gadget", "control_out"));
    assert!(refusal.contains("(-22)"), "{refusal}");

    // accelerometer and gyroscope: the fakes have no FIFO, so read_batch is NOT_SUPPORTED.
    for device in ["imu", "gyro"] {
        let refusal = refused(call(device, "read_batch"));
        assert!(refusal.contains("(-95)"), "{refusal}");
        assert!(refusal.contains("read_batch"), "{refusal}");
    }

    // indicator: its states, as the contract lists them.
    assert_eq!(said(call("lights", "states")), STATES.join(","));
}

/// The payload of a topic's newest sample of `field`, if it has one.
fn latest(subscriber: &mut Subscriber, field: u16) -> Option<Vec<u8>> {
    let _ = subscriber.wait(Duration::from_millis(50));
    subscriber
        .latest(FieldId(field))
        .map(|sample| sample.payload.to_vec())
}

/// A generation counter's latest value, once the topic has one.
fn generation(subscriber: &mut Subscriber) -> Option<i64> {
    let bytes = latest(subscriber, 0)?;
    Some(i64::from_le_bytes(bytes.try_into().ok()?))
}

/// The generations the description topic has carried since the last read, in order. Each sample is
/// one change the bridge published. A sample lost before the test read it fails the test: the count
/// would not be exact.
fn published(subscriber: &mut Subscriber) -> Vec<i64> {
    let mut said = Vec::new();
    loop {
        match subscriber.next() {
            Next::Sample(sample) => {
                let bytes: [u8; 8] = sample
                    .payload
                    .try_into()
                    .expect("a generation is eight bytes");
                said.push(i64::from_le_bytes(bytes));
            }
            Next::Empty => return said,
            Next::Skipped(lost) => {
                panic!("{lost} samples of the description were lost before the test read them")
            }
        }
    }
}

/// The description generation of the fake accelerometer moves once for each change the bridge
/// publishes, and holds still while nothing changes. The changes are counted, not assumed: a power-on
/// whose bind misses its deadline is retried, and the retry is a change of its own (`retrying`, then
/// `running`), so one power-on can publish two. The generation must move by exactly the number of
/// samples published, each one step on from the last, and the device must be in the state the
/// power-on asked for once the changes have settled.
#[test]
fn a_description_generation_moves_once_per_change_and_holds_while_nothing_changes() {
    let dir = dir("generation");
    std::fs::write(dir.join("cpuinfo"), CPUINFO).expect("cpuinfo");
    std::fs::create_dir_all(dir.join("data/slots")).expect("slots");
    std::fs::create_dir_all(dir.join("data/board/slots")).expect("marker");
    std::fs::create_dir_all(dir.join("boot/slots")).expect("boot slots");
    let walk = start_walk(&dir, SimBoard::new());
    let dashboard = &walk.dashboard;
    let mut described = dashboard
        .subscribe("lemnos.imu.description")
        .expect("subscribe");
    let mut set = dashboard.subscribe("lemnos.devices").expect("subscribe");
    let first = wait_for("the description generation", || generation(&mut described));
    let devices = wait_for("the device-set generation", || generation(&mut set));
    assert_eq!(first, 1, "a device's first generation");
    // What the topic has carried so far is the first generation, which is said; from here every
    // sample is a change after it.
    let _ = published(&mut described);

    // Steady: nothing changes, so nothing is published and nothing moves.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        published(&mut described),
        Vec::<i64>::new(),
        "published while steady"
    );
    assert_eq!(
        generation(&mut described),
        Some(first),
        "moved while steady"
    );
    assert_eq!(
        generation(&mut set),
        Some(devices),
        "the device set moved while steady"
    );

    let mut current = first;
    for (word, state) in [
        ("off", ResourceState::Pending),
        ("on", ResourceState::Ready),
    ] {
        let done = run(dashboard, "lemnos.imu.power", "lemnos.imu", word);
        assert_eq!(done.outcome, Outcome::Succeeded, "{done:?}");
        // Settled: no sample for SETTLE, and the resource in the state the power-on asked for. A
        // missed bind is retried after its first backoff, which is shorter than SETTLE.
        let mut said = Vec::new();
        let mut quiet_since = Instant::now();
        let deadline = Instant::now() + WAIT;
        loop {
            let more = published(&mut described);
            if !more.is_empty() {
                said.extend(more);
                quiet_since = Instant::now();
            }
            let settled = resource(dashboard, "lemnos.imu").map(|found| found.state) == Some(state);
            if settled && quiet_since.elapsed() >= SETTLE {
                break;
            }
            assert!(Instant::now() < deadline, "{word} did not settle");
            std::thread::sleep(Duration::from_millis(20));
        }
        for (step, generation) in said.iter().enumerate() {
            assert_eq!(
                *generation,
                current + step as i64 + 1,
                "a published change moved the generation by other than one"
            );
        }
        let changes = said.len() as i64;
        assert!(changes >= 1, "{word} published no change");
        if word == "off" {
            assert_eq!(changes, 1, "switching off is one change");
        }
        current += changes;
        eprintln!("live_walk: {word}: {changes} change(s) published; generation {current}");
        assert_eq!(
            generation(&mut described),
            Some(current),
            "the generation after {word} is not the changes published"
        );
    }
    assert_eq!(
        generation(&mut set),
        Some(devices),
        "the device set did not change"
    );
}

/// The generation `describe` gives a device is the number its description topic publishes: the two
/// come from one field, so they agree, and a change of the device's state moves both together.
#[test]
fn the_describe_generation_is_the_topic_generation_and_moves_with_it() {
    let dir = dir("describe-generation");
    std::fs::write(dir.join("cpuinfo"), CPUINFO).expect("cpuinfo");
    std::fs::create_dir_all(dir.join("data/slots")).expect("slots");
    std::fs::create_dir_all(dir.join("data/board/slots")).expect("marker");
    std::fs::create_dir_all(dir.join("boot/slots")).expect("boot slots");
    let walk = start_walk(&dir, SimBoard::new());
    let dashboard = &walk.dashboard;
    let mut described = dashboard
        .subscribe("lemnos.imu.description")
        .expect("subscribe");
    // `describe` answers only while the device runs, so each reading is taken while it does.
    let described_generation = |dashboard: &Orion| -> Option<i64> {
        let done = run(dashboard, "lemnos.imu.call", "lemnos.imu", "describe");
        if done.outcome != Outcome::Succeeded {
            return None;
        }
        let text = String::from_utf8(done.output).expect("text");
        let records = lemnos_orion::description::parse(&text).expect("the reader reads it");
        let device = records.iter().find(|r| r.word == "device").expect("device");
        Some(
            device
                .get("generation")
                .and_then(|g| g.parse().ok())
                .expect("a generation"),
        )
    };
    // Running: the reading is the topic's, and both are the same number.
    let done = run(dashboard, "lemnos.imu.power", "lemnos.imu", "on");
    assert_eq!(done.outcome, Outcome::Succeeded, "{done:?}");
    let running = wait_for("the imu described", || {
        let reading = described_generation(dashboard)?;
        (generation(&mut described) == Some(reading)).then_some(reading)
    });
    // Off: the topic moves by one, and the device is not described while it is off.
    let done = run(dashboard, "lemnos.imu.power", "lemnos.imu", "off");
    assert_eq!(done.outcome, Outcome::Succeeded, "{done:?}");
    wait_for("the topic after off", || {
        (generation(&mut described) == Some(running + 1)).then_some(())
    });
    // Off is described, not refused: the device record says so, at the generation the topic shows.
    let done = run(dashboard, "lemnos.imu.call", "lemnos.imu", "describe");
    assert_eq!(done.outcome, Outcome::Succeeded, "{done:?}");
    let text = String::from_utf8(done.output).expect("text");
    let records = lemnos_orion::description::parse(&text).expect("the reader reads it");
    let device = records.iter().find(|r| r.word == "device").expect("device");
    assert_eq!(device.get("state"), Some("off"), "{text}");
    assert_eq!(device.get("live"), Some("false"), "{text}");
    assert_eq!(
        device.get("generation").and_then(|g| g.parse::<i64>().ok()),
        Some(running + 1),
        "described at the topic's generation"
    );
    // On again: the topic moves by one more, and the description carries the same number.
    let done = run(dashboard, "lemnos.imu.power", "lemnos.imu", "on");
    assert_eq!(done.outcome, Outcome::Succeeded, "{done:?}");
    wait_for("the topic after on", || {
        (generation(&mut described) == Some(running + 2)).then_some(())
    });
    let again = wait_for("the imu described again", || {
        described_generation(dashboard)
    });
    assert_eq!(
        again,
        running + 2,
        "describe and topic disagree after the change"
    );
}

/// A clock for a host the test runs itself: nanoseconds since it was made.
struct Elapsed(Instant);

impl lemnos_core::HostClock for Elapsed {
    fn now_ns(&self) -> u64 {
        self.0.elapsed().as_nanos() as u64
    }
}

/// The size of each device's description on the walk board, as lemnosd frames it: the same
/// `encode_description` of its manifest, description and identity, measured here with the same
/// drivers. A device that is not running has no description to measure (`describe` refuses it),
/// and is printed as such. Every measured description fits the bridge's reply bound.
#[test]
fn the_description_of_each_device_on_the_walk_board_is_measured_against_the_bound() {
    let dir = dir("size");
    std::fs::write(dir.join("cpuinfo"), CPUINFO).expect("cpuinfo");
    std::fs::create_dir_all(dir.join("data/slots")).expect("slots");
    std::fs::create_dir_all(dir.join("data/board/slots")).expect("marker");
    std::fs::create_dir_all(dir.join("boot/slots")).expect("boot slots");
    let sim = SimBoard::new();
    let i2c = sim.add_i2c("i2c");
    i2c.attach(0x50, Layout::Bytes);
    sim.add_spi("spi");
    sim.add_spi("strip-spi");
    sim.add_uart("uart");
    sim.add_usb_function("usb");
    sim.add_stream("data", 8);
    let board = parse(&board_text(&dir)).expect("board");
    let drivers = [
        entry::<FakeAccelerometer>(),
        entry::<FakeGyroscope>(),
        entry::<Identity>(),
        entry::<SystemSlots>(),
        entry::<RawBus>(),
        entry::<UsbGadget>(),
        entry::<Ws2812>(),
        entry::<Indicator>(),
    ];
    let host = lemnos_host::Host::start(
        &board,
        &drivers,
        Box::new(sim),
        Arc::new(Elapsed(Instant::now())),
        HostOptions::default(),
    )
    .expect("host");
    let mut largest = (0usize, String::new());
    for info in host.devices() {
        let Ok(description) = host.describe(info.index) else {
            let _ = writeln!(
                std::io::stderr(),
                "description {}: not running, not measured",
                info.id
            );
            continue;
        };
        let manifest = host.manifest(info.index).expect("manifest");
        let identity = lemnos_wire::Identity {
            label: &info.label,
            bus: info.bus.as_deref().unwrap_or(""),
            state: 0,
            reason: "",
            not_live: false,
        };
        let mut bytes = vec![0; 1 << 20];
        let len = lemnos_wire::encode_description(&manifest, &description, &identity, &mut bytes)
            .expect("encode");
        let _ = writeln!(std::io::stderr(), "description {}: {len} bytes", info.id);
        assert!(len <= calls::ORION_REPLY_MAX as usize, "{}: {len}", info.id);
        if len > largest.0 {
            largest = (len, info.id.clone());
        }
    }
    let _ = writeln!(
        std::io::stderr(),
        "largest description: {} at {} bytes",
        largest.1,
        largest.0
    );
    assert!(largest.0 > 0, "no device on the walk board is running");
}
