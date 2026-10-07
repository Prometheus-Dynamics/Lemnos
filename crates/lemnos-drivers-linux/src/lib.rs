//! Lemnos device-model drivers over Linux kernel interfaces:
//!
//! - [`HwmonFan`]: a hwmon fan (`pwm1`, `pwm1_enable`, `fan1_input`) as a
//!   [`DeviceClass::Fan`](lemnos_device::DeviceClass::Fan) sensor and
//!   control, with [`HwmonFan::restore_automatic`] as the failsafe.
//! - [`ThermalZone`]: a thermal zone as a temperature sensor.
//! - [`UserspaceRegulator`] and [`DebugfsClock`]: supplies and clocks the
//!   kernel exposes, as `lemnos_hal::Regulator` and `ClockOutput`.
//! - [`Ws2812Pio`]: a WS2812/SK6812 strip on the Raspberry Pi RP1
//!   `ws2812-pio` character device, as a light (`Pixels` plus `brightness`
//!   and `color` controls).
//! - [`KernelDevice`]: the generic binding that serves a chip through its
//!   mainline IIO or hwmon driver from the chip crate's
//!   [`KernelBinding`](lemnos_device::kernel::KernelBinding), producing
//!   exactly the channels the userspace driver would.
//!
//! These are plain `lemnos_device` devices: a `lemnos-lite` table, the full
//! runtime (through `lemnos-driver-sdk`'s adapter) and `lemnosd` host them
//! alike. Everything here is file IO under a configurable root, so tests run
//! against a fake sysfs tree.

#![forbid(unsafe_code)]

mod error;
mod fan;
mod kernel;
mod power;
mod strip;
pub mod sysfs;
mod thermal;

#[cfg(test)]
mod tests;

pub use error::SysfsError;
pub use fan::{
    CONTROL_DUTY, CONTROL_MODE, DUTY, FAN_INFO, HwmonFan, MODE, MODE_AUTOMATIC, MODE_FULL_SPEED,
    MODE_MANUAL, MODE_MAX, PWM_MAX, SPEED, duty_to_pwm, pwm_to_duty,
};
pub use kernel::{I2cLocation, KernelDevice, SysRoot};
pub use lemnos_drivers_ws2812 as ws2812;
pub use power::{DebugfsClock, UserspaceRegulator};
pub use strip::{CONTROL_BRIGHTNESS, CONTROL_COLOR, LIGHT_INFO, Ws2812Pio};
pub use thermal::{THERMAL_INFO, ThermalZone};
