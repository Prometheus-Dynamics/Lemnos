//! The compact Lemnos device model: what a device is, what it measures and
//! what it controls, without `std`, allocation or `String`s.
//!
//! A driver describes itself with a `'static` [`DeviceInfo`]: a
//! [`DeviceClass`], the [`Channel`]s it reads and the [`ControlInfo`]s it
//! accepts. It implements [`Device`] plus [`Sensor`] and/or [`Control`].
//! Values are fixed-point `i32`s: a channel reads `raw × 10^exponent` in its
//! quantity's canonical [`Unit`] (m/s², rad/s, T, V, A, W, °C, rpm, ...), so a
//! reading means the same thing whichever driver or backend produced it.
//! Describing a device costs read-only data, not code.
//!
//! The [`erased`] module has object-safe forms ([`DynSensor`],
//! [`DynControl`], [`DeviceRef`], and `BoxedDevice` with `alloc`) so a table
//! or a service can hold many drivers behind one pointer type. [`asynch`]
//! mirrors the traits for async drivers. [`kernel`] describes how a chip's
//! channels map onto its mainline Linux driver, so a generic IIO/hwmon
//! binding can serve the same device without per-chip code.
//!
//! The layers that consume this crate are described in
//! `docs/compact-model.md`: `lemnos-lite` (a static table for firmware and
//! small Linux), the full runtime (`lemnos`), and `lemnosd`.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod asynch;
pub mod calibration;
pub mod erased;
pub mod fixed;
pub mod gpio;
pub mod kernel;
mod model;
pub mod power_switch;
mod traits;

#[cfg(test)]
mod tests;

pub use calibration::{
    CalibrationCommand, CalibrationPart, CalibrationRoutine, CalibrationStatus,
    MAX_CALIBRATION_WORDS, PART_ACCEL, PART_GYRO, PART_MAG,
};
#[cfg(feature = "alloc")]
pub use erased::BoxedDevice;
pub use erased::{
    DeviceRef, DynControl, DynDevice, DynLight, DynPixels, DynSensor, DynSensorControl,
};
pub use model::{
    Axis, Channel, ControlInfo, DeviceClass, DeviceInfo, DeviceStatus, NO_VALUE, Quantity, Unit,
};
pub use traits::{Control, Device, DeviceError, Pixels, Rgbw, Sensor, check_buffer, check_control};

/// The largest channel count of any device in this workspace; a buffer of
/// this many `i32`s fits every built-in driver's reading.
pub const MAX_CHANNELS: usize = 16;
