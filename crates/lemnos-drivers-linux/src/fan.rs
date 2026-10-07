//! A hwmon fan (`pwm1`, `pwm1_enable`, `fan1_input`) as a device-model fan.

use crate::{SysfsError, sysfs};
use lemnos_device::{
    Channel, Control, ControlInfo, Device, DeviceClass, DeviceError, DeviceInfo, NO_VALUE,
    Quantity, Sensor, check_buffer, check_control,
};
use std::path::{Path, PathBuf};

/// Largest raw value the hwmon `pwm1` attribute accepts.
pub const PWM_MAX: u32 = 255;
/// `pwm1_enable` values. On most fan-controller chips: 0 full speed, 1
/// manual, 2 and up the chip's automatic control. On `pwm-fan`: 0 PWM off
/// (full speed), 1 enabled (the boot default), 2 enabled with the supply kept
/// on; the thermal governor drives it through its cooling device instead
/// (see [`FanRestore`](crate::FanRestore)).
pub const MODE_FULL_SPEED: i32 = 0;
pub const MODE_MANUAL: i32 = 1;
pub const MODE_AUTOMATIC: i32 = 2;
/// The largest `pwm1_enable` value accepted.
pub const MODE_MAX: i32 = 5;

/// Channel and control indices of [`FAN_INFO`].
pub const SPEED: usize = 0;
pub const DUTY: usize = 1;
pub const MODE: usize = 2;
pub const CONTROL_DUTY: usize = 0;
pub const CONTROL_MODE: usize = 1;

/// A fan: `speed` (rpm; `NO_VALUE` without a tachometer), `duty` (‰ of
/// full PWM) and `pwm_mode` (`pwm1_enable`); controls `duty` (0..=1000 ‰)
/// and `pwm_mode` (0..=5). The kernel applies `duty` only in mode 1
/// (manual).
pub static FAN_INFO: DeviceInfo = DeviceInfo::new(
    DeviceClass::Fan,
    "hwmon-fan",
    &[
        Channel::new("speed", Quantity::RotationalSpeed, 0),
        Channel::new("duty", Quantity::Ratio, -3),
        Channel::new("pwm_mode", Quantity::Mode, 0),
    ],
    &[
        ControlInfo::new("duty", Quantity::Ratio, -3, 0, 1000),
        ControlInfo::new("pwm_mode", Quantity::Mode, 0, 0, MODE_MAX),
    ],
);

/// A Linux hwmon fan, such as `pwm-fan`, by its class directory
/// (`/sys/class/hwmon/hwmonN`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwmonFan {
    root: PathBuf,
}

/// A raw `pwm1` value as per mille, rounded.
pub fn pwm_to_duty(pwm: u32) -> i32 {
    ((pwm.min(PWM_MAX) * 1000 + PWM_MAX / 2) / PWM_MAX) as i32
}

/// Per mille as a raw `pwm1` value, rounded.
pub fn duty_to_pwm(duty: i32) -> u32 {
    (duty.clamp(0, 1000) as u32 * PWM_MAX + 500) / 1000
}

impl HwmonFan {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The first fan under `hwmon_root` (`/sys/class/hwmon`) whose `name`
    /// attribute is `name` (`pwm-fan`), or any fan when `name` is `None`.
    pub fn find(hwmon_root: &Path, name: Option<&str>) -> Result<Option<Self>, SysfsError> {
        for entry in sysfs::entries(hwmon_root)? {
            if !entry.join("pwm1").exists() {
                continue;
            }
            let found = sysfs::read_optional(&entry.join("name"))?;
            if name.is_none_or(|name| found.as_deref() == Some(name)) {
                return Ok(Some(Self::new(entry)));
            }
        }
        Ok(None)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The `name` attribute.
    pub fn name(&self) -> Result<Option<String>, SysfsError> {
        sysfs::read_optional(&self.root.join("name"))
    }

    pub fn pwm(&self) -> Result<u32, SysfsError> {
        sysfs::read_parsed(&self.root.join("pwm1"))
    }

    pub fn mode(&self) -> Result<i32, SysfsError> {
        sysfs::read_parsed(&self.root.join("pwm1_enable"))
    }

    /// The tachometer, if the fan has one.
    pub fn rpm(&self) -> Result<Option<u32>, SysfsError> {
        sysfs::read_parsed_optional(&self.root.join("fan1_input"))
    }

    pub fn set_pwm(&self, pwm: u32) -> Result<(), SysfsError> {
        sysfs::write(&self.root.join("pwm1"), pwm.min(PWM_MAX))
    }

    pub fn set_mode(&self, mode: i32) -> Result<(), SysfsError> {
        sysfs::write(&self.root.join("pwm1_enable"), mode.clamp(0, MODE_MAX))
    }

    /// Writes `pwm1_enable = 2`, the automatic control of most fan-controller
    /// chips. Not for `pwm-fan`, which has no automatic mode: hand fans back
    /// with [`HwmonFan::restore_plan`] and [`FanRestore::apply`](crate::FanRestore::apply).
    pub fn restore_automatic(&self) -> Result<(), SysfsError> {
        self.set_mode(MODE_AUTOMATIC)
    }
}

impl Device for HwmonFan {
    type Error = SysfsError;

    fn info(&self) -> &'static DeviceInfo {
        &FAN_INFO
    }

    /// Checks that the fan's attributes are readable.
    fn init(
        &mut self,
        _delay: &mut dyn embedded_hal::delay::DelayNs,
    ) -> Result<(), DeviceError<SysfsError>> {
        self.pwm().map_err(DeviceError::Driver)?;
        Ok(())
    }
}

impl Sensor for HwmonFan {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<SysfsError>> {
        check_buffer(&FAN_INFO, out)?;
        out[SPEED] = match self.rpm().map_err(DeviceError::Driver)? {
            Some(rpm) => i32::try_from(rpm).unwrap_or(i32::MAX),
            None => NO_VALUE,
        };
        out[DUTY] = pwm_to_duty(self.pwm().map_err(DeviceError::Driver)?);
        out[MODE] = self.mode().map_err(DeviceError::Driver)?;
        Ok(())
    }
}

impl Control for HwmonFan {
    fn set(&mut self, index: usize, value: i32) -> Result<i32, DeviceError<SysfsError>> {
        check_control(&FAN_INFO, index, value)?;
        if index == CONTROL_DUTY {
            self.set_pwm(duty_to_pwm(value))
                .map_err(DeviceError::Driver)?;
        } else {
            self.set_mode(value).map_err(DeviceError::Driver)?;
        }
        self.get(index)
    }

    fn get(&mut self, index: usize) -> Result<i32, DeviceError<SysfsError>> {
        check_control::<SysfsError>(&FAN_INFO, index, 0)?;
        if index == CONTROL_DUTY {
            Ok(pwm_to_duty(self.pwm().map_err(DeviceError::Driver)?))
        } else {
            self.mode().map_err(DeviceError::Driver)
        }
    }
}
