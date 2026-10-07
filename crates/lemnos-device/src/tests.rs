use super::*;
use embedded_hal::delay::DelayNs;
use fixed::rescale;
use lemnos_hal::{ErrorKind, HalError};

/// A thermometer with one channel and one control, failing on demand.
struct Probe {
    temperature_mc: i32,
    offset_mc: i32,
    fail: bool,
    inits: u32,
}

static PROBE: DeviceInfo = DeviceInfo::new(
    DeviceClass::Temperature,
    "TEST",
    &[Channel::new("temperature", Quantity::Temperature, -3)],
    &[ControlInfo::new(
        "offset",
        Quantity::Temperature,
        -3,
        -5_000,
        5_000,
    )],
);

impl Device for Probe {
    type Error = ErrorKind;

    fn info(&self) -> &'static DeviceInfo {
        &PROBE
    }

    fn init(&mut self, _delay: &mut dyn DelayNs) -> Result<(), DeviceError<ErrorKind>> {
        self.inits += 1;
        Ok(())
    }
}

impl Sensor for Probe {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<ErrorKind>> {
        check_buffer(&PROBE, out)?;
        if self.fail {
            return Err(DeviceError::Driver(ErrorKind::Timeout));
        }
        out[0] = self.temperature_mc + self.offset_mc;
        Ok(())
    }
}

impl Control for Probe {
    fn set(&mut self, index: usize, value: i32) -> Result<i32, DeviceError<ErrorKind>> {
        check_control(&PROBE, index, value)?;
        self.offset_mc = value;
        Ok(value)
    }

    fn get(&mut self, index: usize) -> Result<i32, DeviceError<ErrorKind>> {
        check_control::<ErrorKind>(&PROBE, index, 0)?;
        Ok(self.offset_mc)
    }
}

struct NoDelay;

impl DelayNs for NoDelay {
    fn delay_ns(&mut self, _ns: u32) {}
}

fn probe() -> Probe {
    Probe {
        temperature_mc: 41_500,
        offset_mc: 0,
        fail: false,
        inits: 0,
    }
}

#[test]
fn device_ref_dispatches_by_kind() {
    let mut device = probe();
    let mut both = DeviceRef::both(&mut device);
    assert!(both.is_sensor() && both.is_control());
    both.init(&mut NoDelay).unwrap();
    assert_eq!(both.set(0, -500), Ok(-500));
    let mut out = [0; 2];
    both.read(&mut out).unwrap();
    assert_eq!(out[0], 41_000);
    assert_eq!(both.get(0), Ok(-500));
    assert_eq!(both.set(0, 9_000), Err(ErrorKind::InvalidInput));
    assert_eq!(both.set(1, 0), Err(ErrorKind::Unsupported));
    assert_eq!(device.inits, 1);

    let mut sensor_only = DeviceRef::sensor(&mut device);
    assert_eq!(sensor_only.set(0, 0), Err(ErrorKind::Unsupported));
    assert_eq!(sensor_only.info().model, "TEST");
    device.fail = true;
    let mut sensor_only = DeviceRef::sensor(&mut device);
    assert_eq!(sensor_only.read(&mut out), Err(ErrorKind::Timeout));
    assert_eq!(sensor_only.read(&mut []), Err(ErrorKind::InvalidInput));
}

#[test]
fn info_finds_names() {
    assert_eq!(PROBE.channel_index("temperature"), Some(0));
    assert_eq!(PROBE.control_index("offset"), Some(0));
    assert_eq!(PROBE.control_index("gain"), None);
    assert_eq!(PROBE.channels[0].unit().symbol(), "°C");
    assert_eq!(Quantity::Acceleration.unit(), Unit::MetrePerSecondSquared);
    assert_eq!(DeviceClass::PowerMonitor.name(), "power-monitor");
}

#[test]
fn rescale_rounds_and_saturates() {
    // nA to µA.
    assert_eq!(rescale(1_499, -9, -6), 1);
    assert_eq!(rescale(1_500, -9, -6), 2);
    assert_eq!(rescale(-1_500, -9, -6), -2);
    // mV to µV.
    assert_eq!(rescale(3_300, -3, -6), 3_300_000);
    assert_eq!(rescale(i64::MAX, 0, -6), i32::MAX);
    assert_eq!(rescale(i64::MIN, 0, 0), NO_VALUE + 1);
    assert_eq!(rescale(5, 0, -30), i32::MAX);
    assert_eq!(rescale(5, -30, 0), 0);
}

#[test]
fn status_follows_error_kind() {
    assert_eq!(
        DeviceStatus::after_error(ErrorKind::NotFound),
        DeviceStatus::Missing
    );
    assert_eq!(
        DeviceStatus::after_error(ErrorKind::Timeout),
        DeviceStatus::Degraded
    );
    assert_eq!(
        DeviceStatus::after_error(ErrorKind::Unsupported),
        DeviceStatus::Faulted
    );
    assert!(DeviceStatus::Missing > DeviceStatus::Available);
}

#[test]
fn device_error_kinds() {
    let error: DeviceError<ErrorKind> = DeviceError::Driver(ErrorKind::Busy);
    assert_eq!(error.kind(), ErrorKind::Busy);
    assert_eq!(error.map(|_| ErrorKind::Failed).kind(), ErrorKind::Failed);
    assert_eq!(
        DeviceError::<ErrorKind>::OutOfRange.kind(),
        ErrorKind::InvalidInput
    );
}

#[cfg(feature = "float")]
#[test]
fn float_conversions() {
    let channel = Channel::new("v", Quantity::Voltage, -6);
    assert_eq!(channel.to_f32(3_300_000), Some(3.3));
    assert_eq!(channel.to_f32(NO_VALUE), None);
    let control = ControlInfo::new("duty", Quantity::Ratio, -3, 0, 1_000);
    assert_eq!(control.from_f32(0.5), Some(500));
    assert_eq!(control.from_f32(1.5), None);
    assert_eq!(control.from_f32(f32::NAN), None);
}

#[cfg(feature = "alloc")]
#[test]
fn boxed_device_owns() {
    let mut boxed = BoxedDevice::both(probe());
    assert_eq!(boxed.set(0, 100), Ok(100));
    let mut out = [0; 1];
    boxed.as_ref().read(&mut out).unwrap();
    assert_eq!(out[0], 41_600);
}
