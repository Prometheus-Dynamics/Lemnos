//! The orientation fusion device and calibration persistence, end to end: a
//! real `lemnosd` over fake IMU and magnetometer drivers (registered in the
//! registry, so no bus is needed) and a real socket.
//!
//! The fakes read a static pose (gravity along +Z) and count their reads, so
//! a test can tell an idle sensor from one that is read.

use lemnos_board::{BoardDefinition, BoardError, Buses, DeviceSpec, DriverEntry, Interface};
use lemnos_device::{
    Axis, BoxedDevice, CalibrationCommand, CalibrationStatus, Channel, Device, DeviceClass,
    DeviceError, DeviceInfo, DeviceStatus, Quantity, Sensor,
};
use lemnos_drivers_linux::SysRoot;
use lemnos_hal::{ErrorKind, HalError};
use lemnos_ipc::{ClientEvent, DeviceClient, Update};
use lemnosd::{Service, ServiceConfig};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Reads of each fake sensor, by the index in its build function (one index
/// per test, so tests running in parallel do not see each other's reads).
static IMU_READS: [AtomicU32; 4] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];
static MAG_READS: [AtomicU32; 4] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];

#[derive(Debug)]
struct Fail;

impl HalError for Fail {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Failed
    }
}

const IMU_CHANNELS: &[Channel] = &[
    Channel::new("acceleration.x", Quantity::Acceleration, -3).on(Axis::X),
    Channel::new("acceleration.y", Quantity::Acceleration, -3).on(Axis::Y),
    Channel::new("acceleration.z", Quantity::Acceleration, -3).on(Axis::Z),
    Channel::new("angular_rate.x", Quantity::AngularRate, -6).on(Axis::X),
    Channel::new("angular_rate.y", Quantity::AngularRate, -6).on(Axis::Y),
    Channel::new("angular_rate.z", Quantity::AngularRate, -6).on(Axis::Z),
    Channel::new("acceleration_cal.x", Quantity::Acceleration, -3).on(Axis::X),
    Channel::new("acceleration_cal.y", Quantity::Acceleration, -3).on(Axis::Y),
    Channel::new("acceleration_cal.z", Quantity::Acceleration, -3).on(Axis::Z),
    Channel::new("angular_rate_cal.x", Quantity::AngularRate, -6).on(Axis::X),
    Channel::new("angular_rate_cal.y", Quantity::AngularRate, -6).on(Axis::Y),
    Channel::new("angular_rate_cal.z", Quantity::AngularRate, -6).on(Axis::Z),
];

static IMU_INFO: DeviceInfo = DeviceInfo::new(DeviceClass::Imu, "FAKE-IMU", IMU_CHANNELS, &[]);

const MAG_CHANNELS: &[Channel] = &[
    Channel::new("magnetic_field.x", Quantity::MagneticField, -9).on(Axis::X),
    Channel::new("magnetic_field.y", Quantity::MagneticField, -9).on(Axis::Y),
    Channel::new("magnetic_field.z", Quantity::MagneticField, -9).on(Axis::Z),
    Channel::new("magnetic_field_cal.x", Quantity::MagneticField, -9).on(Axis::X),
    Channel::new("magnetic_field_cal.y", Quantity::MagneticField, -9).on(Axis::Y),
    Channel::new("magnetic_field_cal.z", Quantity::MagneticField, -9).on(Axis::Z),
];

static MAG_INFO: DeviceInfo =
    DeviceInfo::new(DeviceClass::Magnetometer, "FAKE-MAG", MAG_CHANNELS, &[]);

/// An IMU at rest with gravity along +Z (9.806 m/s²). Its calibration is a
/// revision counter: `Apply` bumps it, and it persists as two words, the
/// revision and a marker.
struct FakeImu {
    index: usize,
    revision: u32,
    /// The acceleration at rest (mm/s²): gravity along +Z for a level board.
    accel: [i32; 3],
}

impl Device for FakeImu {
    type Error = Fail;

    fn info(&self) -> &'static DeviceInfo {
        &IMU_INFO
    }
}

impl Sensor for FakeImu {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Fail>> {
        IMU_READS[self.index].fetch_add(1, Ordering::SeqCst);
        // acceleration (mm/s²), angular rate (µrad/s), then their calibrated forms.
        let [ax, ay, az] = self.accel;
        let values = [ax, ay, az, 0, 0, 0, ax, ay, az, 0, 0, 0];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
        Ok(())
    }

    fn calibration_status(&self) -> Option<CalibrationStatus> {
        Some(CalibrationStatus {
            revision: self.revision,
            ..CalibrationStatus::default()
        })
    }

    fn calibration_command(
        &mut self,
        command: CalibrationCommand,
    ) -> Result<(), DeviceError<Fail>> {
        if command == CalibrationCommand::Apply {
            self.revision += 1;
        }
        Ok(())
    }

    fn calibration_words(&self, out: &mut [i32]) -> usize {
        if out.len() < 2 {
            return 0;
        }
        out[0] = self.revision as i32;
        out[1] = 42;
        2
    }

    fn load_calibration(&mut self, words: &[i32]) -> Result<(), DeviceError<Fail>> {
        match words {
            [revision, 42] => {
                self.revision = *revision as u32;
                Ok(())
            }
            _ => Err(DeviceError::OutOfRange),
        }
    }
}

/// A magnetometer with a fixed field (40 µT along +Z).
struct FakeMag {
    index: usize,
}

impl Device for FakeMag {
    type Error = Fail;

    fn info(&self) -> &'static DeviceInfo {
        &MAG_INFO
    }
}

impl Sensor for FakeMag {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Fail>> {
        MAG_READS[self.index].fetch_add(1, Ordering::SeqCst);
        let values = [0, 0, 40_000, 0, 0, 40_000];
        for (slot, value) in out.iter_mut().zip(values) {
            *slot = value;
        }
        Ok(())
    }
}

fn imu_at<const N: usize>(_: &DeviceSpec, _: &mut dyn Buses) -> Result<BoxedDevice, BoardError> {
    Ok(BoxedDevice::sensor(FakeImu {
        index: N,
        revision: 0,
        accel: [0, 0, 9806],
    }))
}

/// A board on its side, as the Raze rests on the bench: gravity along the
/// sensor's X (accelerometer about (9.79, -0.10, -0.44) m/s²).
fn tilted_at<const N: usize>(_: &DeviceSpec, _: &mut dyn Buses) -> Result<BoxedDevice, BoardError> {
    Ok(BoxedDevice::sensor(FakeImu {
        index: N,
        revision: 0,
        accel: [9793, -99, -440],
    }))
}

fn mag_at<const N: usize>(_: &DeviceSpec, _: &mut dyn Buses) -> Result<BoxedDevice, BoardError> {
    Ok(BoxedDevice::sensor(FakeMag { index: N }))
}

fn entry(name: &'static str, class: DeviceClass, build: lemnos_board::Build) -> DriverEntry {
    DriverEntry {
        name,
        summary: "test fake",
        class,
        interface: Interface::Platform,
        default_address: None,
        config_keys: &[],
        match_keys: &[],
        config_choices: &[],
        kernel: false,
        userspace: true,
        build,
    }
}

/// A buses object for devices that need none.
struct NoBuses(PathBuf);

impl Buses for NoBuses {
    fn i2c(&mut self, bus: u32) -> Result<lemnos_board::DynI2c, BoardError> {
        Err(BoardError::Device {
            device: format!("i2c-{bus}"),
            kind: ErrorKind::NotFound,
            reason: "no buses in this test".into(),
        })
    }

    fn sys(&self) -> SysRoot {
        SysRoot::new(&self.0)
    }
}

/// A fresh directory under the temporary directory.
fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lemnosd-fusion-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

struct Running {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Starts a service for `board` (TOML) with the fake drivers, its calibration
/// in `calibration_dir`.
fn start(root: &Path, board: &str, calibration_dir: &Path) -> Running {
    let socket = root.join("lemnosd.sock");
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (thread_stop, thread_root, thread_socket, thread_dir, board) = (
        Arc::clone(&stop),
        root.to_path_buf(),
        socket.clone(),
        calibration_dir.to_path_buf(),
        board.to_string(),
    );
    let handle = std::thread::spawn(move || {
        let board = BoardDefinition::from_toml_str(&board).unwrap();
        let mut config = ServiceConfig::new(board, thread_socket);
        config
            .registry
            .register(entry("fake-imu-0", DeviceClass::Imu, imu_at::<0>));
        config
            .registry
            .register(entry("fake-imu-1", DeviceClass::Imu, imu_at::<1>));
        config
            .registry
            .register(entry("fake-imu-2", DeviceClass::Imu, imu_at::<2>));
        config
            .registry
            .register(entry("fake-imu-tilt-3", DeviceClass::Imu, tilted_at::<3>));
        config
            .registry
            .register(entry("fake-mag-0", DeviceClass::Magnetometer, mag_at::<0>));
        config
            .registry
            .register(entry("fake-mag-1", DeviceClass::Magnetometer, mag_at::<1>));
        config
            .registry
            .register(entry("fake-mag-2", DeviceClass::Magnetometer, mag_at::<2>));
        config
            .registry
            .register(entry("fake-mag-3", DeviceClass::Magnetometer, mag_at::<3>));
        config.calibration_dir = thread_dir;
        let mut service = Service::new(config, Box::new(NoBuses(thread_root))).unwrap();
        ready_tx.send(()).unwrap();
        service.run(&thread_stop).unwrap();
        service.shutdown(false);
    });
    ready_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    Running {
        socket,
        stop,
        handle: Some(handle),
    }
}

/// A calibration status, retried while the device is first being read (a
/// `Busy` refusal, which is retryable).
fn status(client: &mut DeviceClient, device: &str) -> lemnos_device::CalibrationStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match client.calibration_status(device) {
            Ok(status) => return status,
            Err(lemnos_ipc::ClientError::Refused(lemnos_ipc::Refusal::Device(ErrorKind::Busy)))
                if Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{device}: calibration status: {e}"),
        }
    }
}

fn connect(running: &Running) -> DeviceClient {
    DeviceClient::connect(&running.socket, "helios").unwrap()
}

/// The next reading of `device` (from the event stream), within `wait`.
fn next_reading(
    client: &mut DeviceClient,
    device: &str,
    wait: Duration,
) -> Option<lemnos_ipc::Reading> {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Ok(Some(ClientEvent::Data(Update::Reading(reading)))) =
            client.next_event_timeout(Duration::from_millis(100))
            && reading.device == device
        {
            return Some(reading);
        }
    }
    None
}

/// A board: an IMU and a magnetometer (fake drivers `index`), and, when
/// `fusion` is given, the orientation device with that config.
fn board(index: usize, fusion: Option<&str>) -> String {
    board_with(&format!("fake-imu-{index}"), index, fusion)
}

fn board_with(imu_driver: &str, index: usize, fusion: Option<&str>) -> String {
    let mut text = format!(
        r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "test"

[[devices]]
id = "imu"
driver = "{imu_driver}"
poll_ms = 10

[[devices]]
id = "magnetometer"
driver = "fake-mag-{index}"
poll_ms = 10
idle_poll_ms = 0
"#
    );
    if let Some(config) = fusion {
        text.push_str(&format!(
            "\n[[devices]]\nid = \"orientation\"\ndriver = \"fusion\"\npoll_ms = 10\n{config}\n"
        ));
    }
    text
}

#[test]
fn the_imu_and_magnetometer_are_idle_until_fusion_is_subscribed() {
    let root = dir("on-demand");
    let calibration = root.join("calibration");
    let running = start(
        &root,
        &board(
            0,
            Some(r#"config = { imu = "imu", mag = "magnetometer", mode = "9axis" }"#),
        ),
        &calibration,
    );
    let mut client = connect(&running);

    // Idle: nobody subscribes, so neither sensor is read again. (Each gets
    // one read when it is built, as any sensor does; the count must then stop.)
    std::thread::sleep(Duration::from_millis(300));
    let (imu_idle, mag_idle) = (
        IMU_READS[0].load(Ordering::SeqCst),
        MAG_READS[0].load(Ordering::SeqCst),
    );
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        IMU_READS[0].load(Ordering::SeqCst),
        imu_idle,
        "imu read while idle"
    );
    assert_eq!(
        MAG_READS[0].load(Ordering::SeqCst),
        mag_idle,
        "magnetometer read while idle"
    );
    assert!(imu_idle <= 1 && mag_idle <= 1);

    // Subscribing starts both, and the output is gravity along +Z, level.
    client.subscribe("orientation", 20).unwrap();
    let reading = next_reading(&mut client, "orientation", Duration::from_secs(5))
        .expect("the fusion reports once the imu has been read");
    let gravity = reading.value("gravity.z").expect("gravity.z has a value");
    assert!((gravity - 9.806).abs() < 0.1, "gravity.z {gravity}");
    assert!(reading.value("roll").unwrap().abs() < 0.01);
    assert!(reading.value("pitch").unwrap().abs() < 0.01);
    assert!(IMU_READS[0].load(Ordering::SeqCst) > 0);
    assert!(MAG_READS[0].load(Ordering::SeqCst) > 0);

    // The fusion's rate: 50 ms over one second is about 20 readings.
    client.subscribe("orientation", 50).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let before = IMU_READS[0].load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_secs(1));
    let per_second = IMU_READS[0].load(Ordering::SeqCst) - before;
    assert!(
        (5..=60).contains(&per_second),
        "imu reads a second: {per_second}"
    );

    // Unsubscribing ends the internal subscriptions: back to idle.
    client.subscribe("orientation", 0).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let (imu, mag) = (
        IMU_READS[0].load(Ordering::SeqCst),
        MAG_READS[0].load(Ordering::SeqCst),
    );
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        IMU_READS[0].load(Ordering::SeqCst),
        imu,
        "imu read after the last subscriber left"
    );
    assert_eq!(
        MAG_READS[0].load(Ordering::SeqCst),
        mag,
        "magnetometer read after the last subscriber left"
    );
    drop(running);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_first_output_follows_gravity_from_a_tilted_start() {
    // The board on its side, read through lemnosd's fusion path (not the
    // crate alone): the first output must already sit on the tilt, not start
    // level (roll and pitch from the first accelerometer sample, as
    // `Orientation::init` does).
    let root = dir("tilted");
    let calibration = root.join("calibration");
    let running = start(
        &root,
        &board_with(
            "fake-imu-tilt-3",
            3,
            Some(r#"config = { imu = "imu", mag = "magnetometer", mode = "6axis" }"#),
        ),
        &calibration,
    );
    let mut client = connect(&running);
    client.subscribe("orientation", 20).unwrap();
    let reading = next_reading(&mut client, "orientation", Duration::from_secs(5))
        .expect("the fusion reports once the imu has been read");
    // Pitch is atan2(-9.793, 0.44) = -87.4 degrees: within 5 degrees of that.
    let pitch = reading.value("pitch").expect("pitch has a value");
    let gravity_x = reading.value("gravity.x").expect("gravity.x has a value");
    assert!(
        (pitch + 87.4_f64.to_radians()).abs() < 5.0_f64.to_radians(),
        "first pitch {} deg",
        pitch.to_degrees()
    );
    assert!(gravity_x > 9.0, "first gravity.x {gravity_x}");
    drop(running);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn always_keeps_the_imu_read_without_a_subscriber() {
    let root = dir("always");
    let running = start(
        &root,
        &board(
            1,
            Some("config = { imu = \"imu\", mode = \"6axis\", always = true }"),
        ),
        &root.join("calibration"),
    );
    let mut client = connect(&running);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        IMU_READS[1].load(Ordering::SeqCst) > 5,
        "always: the imu is read with no subscriber"
    );
    // A one-shot read of the fusion is served once its output is valid.
    let deadline = Instant::now() + Duration::from_secs(5);
    let reading = loop {
        match client.read("orientation") {
            Ok(reading) => break reading,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("no reading of the fusion: {e}"),
        }
    };
    assert_eq!(reading.status, DeviceStatus::Available);
    assert!((reading.value("gravity.z").unwrap() - 9.806).abs() < 0.1);
    drop(running);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_calibration_survives_a_restart() {
    let root = dir("persist");
    let calibration = root.join("calibration");
    {
        let running = start(&root, &board(2, None), &calibration);
        let mut client = connect(&running);
        assert_eq!(status(&mut client, "imu").revision, 0);
        client
            .calibration("imu", CalibrationCommand::Apply)
            .unwrap();
        assert_eq!(status(&mut client, "imu").revision, 1);
        // Applied, so saved at once.
        let text = fs::read_to_string(calibration.join("imu.toml")).unwrap();
        assert!(text.contains("format = \"lemnos.calibration\""), "{text}");
        assert!(text.contains("revision = 1"), "{text}");
        assert!(text.contains("words = [1, 42]"), "{text}");
    }
    // A new service loads the file.
    let running = start(&root, &board(2, None), &calibration);
    let mut client = connect(&running);
    assert_eq!(status(&mut client, "imu").revision, 1);
    drop(running);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_bad_calibration_file_is_ignored_with_its_reason() {
    let root = dir("bad-file");
    let calibration = root.join("calibration");
    fs::create_dir_all(&calibration).unwrap();
    fs::write(
        calibration.join("imu.toml"),
        "format = \"something-else\"\nschema_version = 1\ndevice = \"imu\"\n",
    )
    .unwrap();
    let running = start(&root, &board(2, None), &calibration);
    let mut client = connect(&running);
    assert_eq!(status(&mut client, "imu").revision, 0);
    let imu = client
        .list()
        .unwrap()
        .into_iter()
        .find(|d| d.id == "imu")
        .unwrap();
    assert_eq!(imu.status, DeviceStatus::Available);
    assert!(imu.reason.contains("format is"), "{}", imu.reason);
    drop(running);
    let _ = fs::remove_dir_all(&root);
}
