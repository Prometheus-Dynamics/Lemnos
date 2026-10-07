//! `lemnosd` end to end: a board definition with a BMI088 on mock I2C
//! hardware (`lemnos-mock`, through the runtime's session adapter), a fan, a
//! thermal zone and an LED ring in a fake sysfs tree, served to clients over
//! a real socket.

use lemnos_board::{BoardDefinition, BoardError, Buses, DynI2c};
use lemnos_bus::I2cBusBackend;
use lemnos_bus::hal::OwnedI2cBus;
use lemnos_core::{DeviceDescriptor, InterfaceKind};
use lemnos_drivers_linux::SysRoot;
use lemnos_ipc::{
    ClientError, ClientEvent, ClientOptions, LedStatus, Refusal, SystemState, Update,
};
use lemnos_mock::{MockHardware, MockI2cDevice};
use lemnosd::{Service, ServiceConfig};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

struct MockBuses {
    hardware: MockHardware,
    owner: DeviceDescriptor,
    sys: PathBuf,
}

impl Buses for MockBuses {
    fn i2c(&mut self, bus: u32) -> Result<DynI2c, BoardError> {
        self.hardware
            .open_i2c_controller(&self.owner, bus, lemnos_bus::SessionAccess::Shared)
            .map(|session| DynI2c::new(OwnedI2cBus::new(session)))
            .map_err(|e| BoardError::Device {
                device: format!("i2c-{bus}"),
                kind: e.kind(),
                reason: e.to_string(),
            })
    }

    fn sys(&self) -> SysRoot {
        SysRoot::new(&self.sys)
    }
}

fn tree(root: &Path) {
    let _ = fs::remove_dir_all(root);
    for (path, contents) in [
        ("class/hwmon/hwmon2/name", "pwm-fan"),
        ("class/hwmon/hwmon2/pwm1", "200"),
        ("class/hwmon/hwmon2/pwm1_enable", "2"),
        ("class/thermal/thermal_zone0/type", "cpu-thermal"),
        ("class/thermal/thermal_zone0/temp", "45000"),
        ("dev/leds0", ""),
    ] {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}

fn board(root: &Path) -> BoardDefinition {
    BoardDefinition::from_toml_str(&format!(
        r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "raze"

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-1"
address = 0x18
backend = "userspace"
poll_ms = 20

[[devices]]
id = "magnetometer"
driver = "bmm150"
bus = "i2c-1"
address = 0x10
backend = "userspace"

[[devices]]
id = "fan"
driver = "hwmon-fan"
path = "{root}/class/hwmon/hwmon2"
writers = ["helios"]
config = {{ restore_mode = 2 }}

[[devices]]
id = "cpu-thermal"
driver = "thermal-zone"
match = {{ type = "cpu-thermal" }}

[[devices]]
id = "ring"
driver = "ws2812"
path = "{root}/dev/leds0"
config = {{ count = 16, offset = 5, fade_ms = 0, status_effect = "solid", ok = 0x00ff00 }}
"#,
        root = root.display()
    ))
    .unwrap()
}

fn axes(values: [i16; 3]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Starts the service on its own thread; returns its socket and a stopper.
fn start(root: &Path) -> (PathBuf, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    tree(root);
    let socket = root.join("lemnosd.sock");
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (thread_stop, thread_root, thread_socket) =
        (Arc::clone(&stop), root.to_path_buf(), socket.clone());
    let handle = std::thread::spawn(move || {
        let hardware = MockHardware::builder()
            .with_i2c_device(
                MockI2cDevice::new(1, 0x18)
                    .with_bytes(0x00, [0x1e])
                    .with_bytes(0x12, axes([16384, 0, -16384])),
            )
            .with_i2c_device(MockI2cDevice::new(1, 0x68).with_bytes(0x00, [0x0f]))
            .build();
        let owner = DeviceDescriptor::builder("lemnosd.test", InterfaceKind::I2c)
            .unwrap()
            .build()
            .unwrap();
        let buses = MockBuses {
            hardware,
            owner,
            sys: thread_root.clone(),
        };
        let config = ServiceConfig::new(board(&thread_root), thread_socket);
        let mut service = Service::new(config, Box::new(buses)).unwrap();
        ready_tx.send(()).unwrap();
        service.run(&thread_stop).unwrap();
        service.shutdown(false);
    });
    ready_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    (socket, stop, handle)
}

fn ring(root: &Path) -> Vec<u8> {
    fs::read(root.join("dev/leds0")).unwrap()
}

/// Waits until `check` passes (the service renders asynchronously).
fn eventually(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn serves_readings_controls_and_led_intents() {
    let root = std::env::temp_dir().join(format!("lemnosd-test-{}", std::process::id()));
    let (socket, stop, handle) = start(&root);

    let mut helios = ClientOptions::new(&socket, "helios").devices().unwrap();
    assert_eq!(helios.board(), "raze");
    let devices = helios.list().unwrap();
    let names: Vec<&str> = devices.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(names, ["imu", "magnetometer", "fan", "cpu-thermal", "ring"]);
    let status = |id: &str| devices.iter().find(|d| d.id == id).unwrap().status;
    assert_eq!(status("imu"), lemnos_device::DeviceStatus::Available);
    // Nothing answers at 0x10: not available (retried later), the rest
    // unaffected.
    assert_ne!(
        status("magnetometer"),
        lemnos_device::DeviceStatus::Available
    );
    assert_eq!(devices[4].pixels, 16);

    // Typed reads.
    let imu = helios.read("imu").unwrap();
    let x = imu.value("acceleration.x").unwrap();
    assert!((x - 29.419).abs() < 0.01, "{x}");
    assert!(imu.value("acceleration.z").unwrap() < -29.0);
    let zone = helios.read("cpu-thermal").unwrap();
    assert_eq!(zone.value("temperature"), Some(45.0));
    assert!(matches!(
        helios.read("nope"),
        Err(ClientError::Refused(Refusal::UnknownDevice))
    ));

    // Subscriptions.
    helios.subscribe("imu", 20).unwrap();
    let mut readings = 0;
    while readings < 3 {
        match helios.next_event_timeout(Duration::from_secs(5)).unwrap() {
            Some(ClientEvent::Data(Update::Reading(r))) if r.device == "imu" => readings += 1,
            Some(_) => {}
            None => panic!("no readings"),
        }
    }

    // Controls under the write policy.
    assert_eq!(helios.set("fan", "pwm_mode", 1.0).unwrap(), 1.0);
    assert_eq!(helios.set("fan", "duty", 0.7).unwrap(), 0.702);
    assert_eq!(
        fs::read_to_string(root.join("class/hwmon/hwmon2/pwm1")).unwrap(),
        "179"
    );
    assert!(matches!(
        helios.set("fan", "duty", 1.5),
        Err(ClientError::Refused(Refusal::OutOfRange))
    ));
    let mut other = ClientOptions::new(&socket, "photonvision")
        .devices()
        .unwrap();
    assert!(matches!(
        other.set("fan", "duty", 0.5),
        Err(ClientError::Refused(Refusal::NotAllowed))
    ));
    assert_eq!(other.get("fan", "duty").unwrap(), 0.702);

    // LED intents: status over app frames, geometry applied, cleared on exit.
    let mut leds = ClientOptions::new(&socket, "vision-app").leds().unwrap();
    leds.set_leds(&[(0, 0xff0000)]).unwrap();
    leds.sync().unwrap();
    // Logical LED 0 is physical LED 5 (the ring's offset).
    eventually(|| ring(&root).get(20..24) == Some(&[255, 0, 0, 0][..]));
    assert_eq!(&ring(&root)[0..4], &[0, 0, 0, 0]);
    leds.status(LedStatus::Ok).unwrap();
    leds.sync().unwrap();
    eventually(|| ring(&root).chunks(4).all(|p| p == [0, 255, 0, 0]));
    leds.system(SystemState::Updating {
        progress: Some(500),
        phase: lemnos_ipc::Phase::Writing,
    })
    .unwrap();
    leds.sync().unwrap();
    // Half the ring filled (from logical 0 = physical 5), the rest dim.
    eventually(|| {
        let bytes = ring(&root);
        bytes[20..24] != [0, 255, 0, 0] && bytes[20..24] == bytes[24..28]
    });
    leds.clear().unwrap();
    leds.sync().unwrap();
    eventually(|| ring(&root).iter().all(|b| *b == 0));

    // A client's intents end when it leaves (unless it asked to keep them).
    leds.status(LedStatus::Error).unwrap();
    leds.sync().unwrap();
    eventually(|| ring(&root).iter().any(|b| *b != 0));
    drop(leds);
    eventually(|| ring(&root).iter().all(|b| *b == 0));

    // Stopping hands the fan back to the kernel.
    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
    assert_eq!(
        fs::read_to_string(root.join("class/hwmon/hwmon2/pwm1_enable")).unwrap(),
        "2"
    );
    assert!(!socket.exists());
    let _ = fs::remove_dir_all(&root);
}
