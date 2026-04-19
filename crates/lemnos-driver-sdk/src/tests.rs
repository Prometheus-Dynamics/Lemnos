use crate::{
    BoundDevice, CustomInteraction, Driver, DriverBindContext, DriverConformanceHarness,
    DriverError, DriverMatchLevel, I2cControllerIo, I2cDeviceIo, LinuxClassDeviceIo,
    MAX_RETAINED_OUTPUT_BYTES, NoopBoundDevice, OUTPUT_BYTES_PREVIEW_KIND, OUTPUT_KIND, OUTPUT_LEN,
    OUTPUT_PREVIEW, OUTPUT_RETAINED_LEN, OUTPUT_TRUNCATED, bind_session_for_kind,
    bind_session_for_kinds, bounded_bytes_output,
};
use lemnos_bus::{
    BusResult, BusSession, I2cControllerSession, I2cSession, SessionAccess, SessionMetadata,
};
use lemnos_core::{
    DeviceControlSurface, DeviceDescriptor, DeviceKind, I2cOperation, InterfaceKind, Value,
};
use lemnos_driver_manifest::{DriverManifest, DriverPriority, MatchCondition, MatchRule};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TEST_ROOT_ID: AtomicU64 = AtomicU64::new(0);

struct TestRoot {
    root: PathBuf,
}

impl TestRoot {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos();
        let id = NEXT_TEST_ROOT_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "lemnos-driver-sdk-tests-{}-{nonce}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create temp root");
        Self { root }
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write test file");
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct FakeDriver;

impl Driver for FakeDriver {
    fn id(&self) -> &str {
        "driver.fake"
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(self.id(), "Fake driver", vec![InterfaceKind::Gpio])
                .with_priority(DriverPriority::Preferred)
                .with_kind(DeviceKind::GpioLine)
                .with_rule(MatchRule::new(25).require(MatchCondition::Vendor("acme".into()))),
        )
    }
}

struct BindableFakeDriver;

impl Driver for BindableFakeDriver {
    fn id(&self) -> &str {
        "driver.bindable"
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(self.id(), "Bindable fake driver", vec![InterfaceKind::Gpio])
                .with_kind(DeviceKind::GpioLine),
        )
    }

    fn bind(
        &self,
        device: &DeviceDescriptor,
        _context: &DriverBindContext<'_>,
    ) -> crate::DriverResult<Box<dyn BoundDevice>> {
        Ok(Box::new(NoopBoundDevice::new(self.id(), device.clone())))
    }
}

struct MismatchedManifestDriver;

impl Driver for MismatchedManifestDriver {
    fn id(&self) -> &str {
        "driver.mismatch"
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(DriverManifest::new(
            "driver.other",
            "Mismatched",
            vec![InterfaceKind::Gpio],
        ))
    }
}

fn gpio_line() -> DeviceDescriptor {
    DeviceDescriptor::builder_for_kind("gpio.line0", DeviceKind::GpioLine)
        .expect("builder")
        .vendor("acme")
        .build()
        .expect("descriptor")
}

#[test]
fn manifest_match_converts_into_driver_match() {
    let device = gpio_line();

    let matched = FakeDriver.matches(&device);
    assert!(matched.is_supported());
    assert_eq!(matched.level, DriverMatchLevel::Preferred);
}

#[test]
fn bind_context_reports_missing_backend() {
    let err = match DriverBindContext::default().gpio("driver.fake") {
        Ok(_) => panic!("gpio backend should be required"),
        Err(err) => err,
    };

    assert!(matches!(
        err,
        DriverError::MissingBackend {
            interface: InterfaceKind::Gpio,
            ..
        }
    ));
}

#[test]
fn noop_bound_device_exposes_state_and_interactions() {
    let device = DeviceDescriptor::new("gpio.line0", InterfaceKind::Gpio).expect("descriptor");
    let interaction = CustomInteraction::new("vendor.calibrate", "Calibrate").expect("interaction");
    let mut bound = NoopBoundDevice::new("driver.fake", device).with_interaction(interaction);

    assert_eq!(bound.driver_id(), "driver.fake");
    assert_eq!(bound.custom_interactions().len(), 1);
    assert_eq!(bound.state().expect("state"), None);
}

#[test]
fn bounded_bytes_output_truncates_large_payloads() {
    let bytes = vec![0xAB; MAX_RETAINED_OUTPUT_BYTES + 8];
    let value = bounded_bytes_output(&bytes);
    let map = match value {
        Value::Map(map) => map,
        other => panic!("expected bounded map output, got {other:?}"),
    };

    assert_eq!(
        map.get(OUTPUT_KIND),
        Some(&Value::from(OUTPUT_BYTES_PREVIEW_KIND))
    );
    assert_eq!(
        map.get(OUTPUT_LEN),
        Some(&Value::from((MAX_RETAINED_OUTPUT_BYTES as u64) + 8))
    );
    assert_eq!(map.get(OUTPUT_TRUNCATED), Some(&Value::from(true)));
    assert_eq!(
        map.get(OUTPUT_RETAINED_LEN),
        Some(&Value::from(MAX_RETAINED_OUTPUT_BYTES as u64))
    );
    assert_eq!(
        map.get(OUTPUT_PREVIEW)
            .and_then(Value::as_bytes)
            .map(<[u8]>::len),
        Some(MAX_RETAINED_OUTPUT_BYTES)
    );
}

#[test]
fn conformance_harness_validates_manifest_and_support() {
    let device = gpio_line();
    let harness = DriverConformanceHarness::new(&FakeDriver);

    let manifest = harness
        .validate_manifest()
        .expect("manifest should conform");
    assert_eq!(manifest.id, "driver.fake");

    let matched = harness
        .expect_supported(&device)
        .expect("gpio line should be supported");
    assert!(matched.is_supported());
}

#[test]
fn conformance_harness_reports_unsupported_devices() {
    let device = DeviceDescriptor::builder_for_kind("usb.dev0", DeviceKind::UsbDevice)
        .expect("builder")
        .build()
        .expect("descriptor");
    let harness = DriverConformanceHarness::new(&FakeDriver);

    let matched = harness
        .expect_unsupported(&device)
        .expect("usb device should be unsupported");
    assert!(!matched.is_supported());
}

#[test]
fn conformance_harness_binds_supported_devices() {
    let device = gpio_line();
    let harness = DriverConformanceHarness::new(&BindableFakeDriver);

    let bound = harness.bind_supported(&device).expect("bind supported");
    assert_eq!(bound.driver_id(), "driver.bindable");
    assert_eq!(bound.device().id, device.id);
}

#[test]
fn conformance_harness_rejects_driver_manifest_id_mismatch() {
    let err = DriverConformanceHarness::new(&MismatchedManifestDriver)
        .validate_manifest()
        .expect_err("manifest mismatch should fail conformance");

    assert!(err.to_string().contains("did not match manifest id"));
}

#[test]
fn bind_session_for_kind_checks_kind_before_building_bound_device() {
    let device = gpio_line();

    let bound = bind_session_for_kind(
        "driver.fake",
        &device,
        DeviceKind::GpioLine,
        "gpio-line device",
        || Ok("session"),
        |driver_id, _session| NoopBoundDevice::new(driver_id, device.clone()),
    )
    .expect("bind session for matching kind");

    assert_eq!(bound.driver_id(), "driver.fake");

    let wrong = DeviceDescriptor::builder_for_kind("spi.dev0", DeviceKind::SpiDevice)
        .expect("builder")
        .build()
        .expect("descriptor");
    let err = match bind_session_for_kind(
        "driver.fake",
        &wrong,
        DeviceKind::GpioLine,
        "gpio-line device",
        || Ok("session"),
        |driver_id, _session| NoopBoundDevice::new(driver_id, wrong.clone()),
    ) {
        Ok(_) => panic!("wrong kind should be rejected"),
        Err(err) => err,
    };

    assert!(matches!(err, DriverError::BindRejected { .. }));
}

#[test]
fn bind_session_for_kinds_accepts_any_expected_kind() {
    let device = DeviceDescriptor::builder_for_kind("usb.if0", DeviceKind::UsbInterface)
        .expect("builder")
        .build()
        .expect("descriptor");

    let bound = bind_session_for_kinds(
        "driver.fake",
        &device,
        &[DeviceKind::UsbDevice, DeviceKind::UsbInterface],
        "usb-device or usb-interface",
        || Ok("session"),
        |driver_id, _session| NoopBoundDevice::new(driver_id, device.clone()),
    )
    .expect("bind session for matching kind set");

    assert_eq!(bound.device().id, device.id);
}

struct FakeI2cSession {
    device: DeviceDescriptor,
    metadata: SessionMetadata,
    registers: BTreeMap<u8, Vec<u8>>,
}

impl FakeI2cSession {
    fn new(registers: impl IntoIterator<Item = (u8, Vec<u8>)>) -> Self {
        Self {
            device: DeviceDescriptor::builder_for_kind("i2c.dev0", DeviceKind::I2cDevice)
                .expect("builder")
                .build()
                .expect("descriptor"),
            metadata: SessionMetadata::new("fake-i2c", SessionAccess::Shared),
            registers: registers.into_iter().collect(),
        }
    }
}

impl BusSession for FakeI2cSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::I2c
    }

    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn close(&mut self) -> BusResult<()> {
        Ok(())
    }
}

impl I2cSession for FakeI2cSession {
    fn read(&mut self, _length: u32) -> BusResult<Vec<u8>> {
        Ok(Vec::new())
    }

    fn write(&mut self, _bytes: &[u8]) -> BusResult<()> {
        Ok(())
    }

    fn write_read(&mut self, write: &[u8], _read_length: u32) -> BusResult<Vec<u8>> {
        let register = *write.first().expect("register");
        Ok(self.registers.get(&register).cloned().unwrap_or_default())
    }

    fn transaction(&mut self, _operations: &[I2cOperation]) -> BusResult<Vec<Vec<u8>>> {
        Ok(Vec::new())
    }
}

struct FakeI2cControllerSession {
    device: DeviceDescriptor,
    metadata: SessionMetadata,
    bus: u32,
    registers: BTreeMap<(u16, u8), Vec<u8>>,
}

impl FakeI2cControllerSession {
    fn new(registers: impl IntoIterator<Item = ((u16, u8), Vec<u8>)>) -> Self {
        Self {
            device: DeviceDescriptor::builder_for_kind("i2c.owner0", DeviceKind::I2cDevice)
                .expect("builder")
                .build()
                .expect("descriptor"),
            metadata: SessionMetadata::new(
                "fake-i2c-controller",
                SessionAccess::ExclusiveController,
            ),
            bus: 4,
            registers: registers.into_iter().collect(),
        }
    }
}

impl BusSession for FakeI2cControllerSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::I2c
    }

    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn close(&mut self) -> BusResult<()> {
        Ok(())
    }
}

impl I2cControllerSession for FakeI2cControllerSession {
    fn bus(&self) -> u32 {
        self.bus
    }

    fn read(&mut self, _address: u16, _length: u32) -> BusResult<Vec<u8>> {
        Ok(Vec::new())
    }

    fn write(&mut self, _address: u16, _bytes: &[u8]) -> BusResult<()> {
        Ok(())
    }

    fn write_read(&mut self, address: u16, write: &[u8], _read_length: u32) -> BusResult<Vec<u8>> {
        let register = *write.first().expect("register");
        Ok(self
            .registers
            .get(&(address, register))
            .cloned()
            .unwrap_or_default())
    }

    fn transaction(
        &mut self,
        _address: u16,
        _operations: &[I2cOperation],
    ) -> BusResult<Vec<Vec<u8>>> {
        Ok(Vec::new())
    }
}

#[test]
fn i2c_device_io_reads_signed_words_and_exact_blocks() {
    let mut session = FakeI2cSession::new([
        (0x01, vec![0x12, 0x34]),
        (0x02, vec![0xff, 0x80]),
        (0x03, vec![1, 2, 3, 4]),
    ]);
    let device = session.device().clone();
    let mut io = I2cDeviceIo::new(&mut session, "driver.fake", &device);

    assert_eq!(io.read_u16_be(0x01).expect("u16"), 0x1234);
    assert_eq!(io.read_i16_be(0x02).expect("i16"), -128);
    assert_eq!(io.read_exact_block::<4>(0x03).expect("block"), [1, 2, 3, 4]);
}

#[test]
fn i2c_controller_io_reads_signed_words_and_exact_blocks() {
    let mut controller = FakeI2cControllerSession::new([
        ((0x18, 0x10), vec![0x44, 0x55]),
        ((0x68, 0x20), vec![0xff, 0x7f]),
        ((0x18, 0x30), vec![9, 8, 7, 6, 5, 4]),
    ]);
    let device = controller.device().clone();
    let mut io = I2cControllerIo::new(&mut controller, "driver.fake", &device);

    assert_eq!(io.target(0x18).read_u16_be(0x10).expect("u16"), 0x4455);
    assert_eq!(io.target(0x68).read_i16_be(0x20).expect("i16"), -129);
    assert_eq!(
        io.target(0x18).read_exact_block::<6>(0x30).expect("block"),
        [9, 8, 7, 6, 5, 4]
    );
}

#[test]
fn linux_class_device_io_reads_and_writes_control_files() {
    let root = TestRoot::new();
    root.write("brightness", "1\n");
    root.write("max_brightness", "255\n");

    let device = DeviceDescriptor::builder_for_kind(
        "linux.gpio.led.ACT",
        DeviceKind::Unspecified(InterfaceKind::Gpio),
    )
    .expect("builder")
    .control_surface(DeviceControlSurface::LinuxClass {
        root: root.root.display().to_string(),
    })
    .build()
    .expect("descriptor");

    let io = LinuxClassDeviceIo::from_device("driver.led", &device).expect("linux class io");
    assert_eq!(io.read_u64("brightness").expect("brightness"), 1);
    assert_eq!(
        io.read_optional_u64("max_brightness").expect("max"),
        Some(255)
    );
    assert_eq!(io.read_optional_trimmed("trigger").expect("missing"), None);

    io.write_u64("brightness", 0).expect("write brightness");
    assert_eq!(
        fs::read_to_string(root.root.join("brightness")).expect("brightness file"),
        "0"
    );
}
