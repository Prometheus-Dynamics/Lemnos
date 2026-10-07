//! The traits drivers implement.

use crate::DeviceInfo;
use core::fmt;
use embedded_hal::delay::DelayNs;
use lemnos_hal::{ErrorKind, HalError};

/// The error of a device-model operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceError<E> {
    /// The driver failed.
    Driver(E),
    /// No such control, or the device does not support the operation.
    Unsupported,
    /// A control value outside its `min..=max`.
    OutOfRange,
    /// `read` was given fewer slots than the device has channels.
    BufferTooSmall,
}

impl<E> DeviceError<E> {
    /// Maps the driver error.
    pub fn map<F>(self, f: impl FnOnce(E) -> F) -> DeviceError<F> {
        match self {
            Self::Driver(error) => DeviceError::Driver(f(error)),
            Self::Unsupported => DeviceError::Unsupported,
            Self::OutOfRange => DeviceError::OutOfRange,
            Self::BufferTooSmall => DeviceError::BufferTooSmall,
        }
    }
}

impl<E: HalError> HalError for DeviceError<E> {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Driver(error) => error.kind(),
            Self::Unsupported => ErrorKind::Unsupported,
            Self::OutOfRange | Self::BufferTooSmall => ErrorKind::InvalidInput,
        }
    }
}

impl<E: fmt::Debug> fmt::Display for DeviceError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Driver(error) => write!(f, "device driver: {error:?}"),
            Self::Unsupported => f.write_str("unsupported device operation"),
            Self::OutOfRange => f.write_str("control value out of range"),
            Self::BufferTooSmall => f.write_str("buffer smaller than the channel count"),
        }
    }
}

impl<E: fmt::Debug> core::error::Error for DeviceError<E> {}

/// A device described by a `'static` [`DeviceInfo`].
///
/// Implement [`Sensor`] for devices with channels and [`Control`] for devices
/// with controls; many devices are both (a fan reads its speed and accepts a
/// duty cycle).
pub trait Device {
    type Error: HalError;

    /// What the device is, reads and accepts.
    fn info(&self) -> &'static DeviceInfo;

    /// Brings the device up (identify, reset, configure). Calling it again
    /// re-initializes. Defaults to doing nothing.
    fn init(&mut self, delay: &mut dyn DelayNs) -> Result<(), DeviceError<Self::Error>> {
        let _ = delay;
        Ok(())
    }
}

/// A device that measures.
pub trait Sensor: Device {
    /// Writes one value per channel into `out`, in `info().channels` order;
    /// [`NO_VALUE`](crate::NO_VALUE) marks a channel without a valid reading.
    /// Slots past the channel count are left alone.
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Self::Error>>;
}

/// A device that accepts settings.
pub trait Control: Device {
    /// Sets control `index` (into `info().controls`) to `value` and returns
    /// the value applied, which may be rounded or clamped by the hardware.
    fn set(&mut self, index: usize, value: i32) -> Result<i32, DeviceError<Self::Error>>;

    /// The current value of control `index`, read back from the device or
    /// the last value written.
    fn get(&mut self, index: usize) -> Result<i32, DeviceError<Self::Error>>;
}

/// Checks a control write against `info`: the index exists and the value is
/// in range.
pub fn check_control<E>(info: &DeviceInfo, index: usize, value: i32) -> Result<(), DeviceError<E>> {
    let control = info.controls.get(index).ok_or(DeviceError::Unsupported)?;
    if control.accepts(value) {
        Ok(())
    } else {
        Err(DeviceError::OutOfRange)
    }
}

/// Checks that `out` has a slot for every channel of `info`.
pub fn check_buffer<E>(info: &DeviceInfo, out: &[i32]) -> Result<(), DeviceError<E>> {
    if out.len() >= info.channels.len() {
        Ok(())
    } else {
        Err(DeviceError::BufferTooSmall)
    }
}
