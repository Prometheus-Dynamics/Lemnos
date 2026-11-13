//! The out-buffer methods the simulated board hosts, called through the bridge's `.call` action
//! against a real Orion core and real lemnosd, each reply checked for its content. The devices:
//! the fake accelerometer and gyroscope, the identity and slots drivers, a raw bus on the simulated
//! I2C, SPI and UART, a USB gadget on a mock function with a data stream, and an indicator on a
//! WS2812 strip on a second simulated SPI. The methods none of these can host are named in the
//! bridge's docs as covered by the text path only.

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
use orion::provider::Found;
use orion::types::Error as Refusal;
use orion::{Address, ClientError, Orion};
use orion_node_app::{Node, NodeSettings};
use orion_stubs::Board;
use tokio::sync::oneshot;

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
            other => {
                return other
                    .unwrap_or_else(|error| panic!("{action} {args:?} on {resource}: {error:?}"));
            }
        }
    }
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
    let lemnosd = dir.join("lemnosd.sock");
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

    let board = Board::start_for_node(&dir);
    let _logic = Logic::start(&dir);
    let _service = Service::start(LemnosdConfig {
        socket: lemnosd.clone(),
        name: "t".to_string(),
        board: parse(&board_text(&dir)).expect("board"),
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
    let _bridge = start_bridge(&lemnosd, board.socket(), None);
    let dashboard = dashboard(board.socket());
    for device in WAIT_NAMES {
        wait_for(device, || resource(&dashboard, &format!("lemnos.{device}")));
    }
    let call = |device: &str, words: &str| {
        run_once_said(
            &dashboard,
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
