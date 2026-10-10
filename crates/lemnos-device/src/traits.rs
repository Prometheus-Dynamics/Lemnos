//! The traits drivers implement.

use crate::calibration::{CalibrationCommand, CalibrationStatus};
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

    /// Reads the samples taken since the last read into `out`, one row of
    /// channels each, oldest first, and returns how many. A device that buffers
    /// samples (see [`sample_period_us`](Self::sample_period_us)) returns as
    /// many as it has, up to `out.len()`; any other device reads one. Rows past
    /// the count are left alone.
    fn read_batch(
        &mut self,
        out: &mut [[i32; crate::MAX_CHANNELS]],
    ) -> Result<usize, DeviceError<Self::Error>> {
        let Some(row) = out.first_mut() else {
            return Ok(0);
        };
        self.read(&mut row[..])?;
        Ok(1)
    }

    /// The spacing of the samples in a batch, in microseconds: the samples of
    /// one batch are this far apart, the last one taken when the batch was
    /// read. `None` for a device whose batch is one sample.
    fn sample_period_us(&self) -> Option<u32> {
        None
    }

    /// Which channels the next [`read`](Self::read) needs: bit `i` is channel
    /// `i` of `info().channels`. A device that can read a subset of its
    /// channels for less bus time (fewer registers) does so, and writes
    /// [`NO_VALUE`](crate::NO_VALUE) for the channels not selected. The default
    /// ignores the selection and reads every channel. Selecting costs nothing
    /// on the bus by itself.
    fn select_channels(&mut self, mask: u64) {
        let _ = mask;
    }

    /// The calibration this sensor reports (see [`crate::calibration`]), or
    /// `None` for a sensor without one.
    fn calibration_status(&self) -> Option<CalibrationStatus> {
        None
    }

    /// Applies a calibration command. `Unsupported` by default.
    fn calibration_command(
        &mut self,
        command: CalibrationCommand,
    ) -> Result<(), DeviceError<Self::Error>> {
        let _ = command;
        Err(DeviceError::Unsupported)
    }

    /// Writes the applied calibration as words for a host to persist (word 0
    /// is the layout's version); returns how many. 0 by default.
    fn calibration_words(&self, out: &mut [i32]) -> usize {
        let _ = out;
        0
    }

    /// Applies calibration words written by [`calibration_words`](Self::calibration_words).
    /// `Unsupported` by default; a driver rejects words of another version.
    fn load_calibration(&mut self, words: &[i32]) -> Result<(), DeviceError<Self::Error>> {
        let _ = words;
        Err(DeviceError::Unsupported)
    }
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

/// One LED's colour: red, green, blue and (for RGBW parts) white.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgbw {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub w: u8,
}

impl Rgbw {
    pub const OFF: Self = Self::new(0, 0, 0, 0);

    pub const fn new(r: u8, g: u8, b: u8, w: u8) -> Self {
        Self { r, g, b, w }
    }

    /// A colour from `0xRRGGBB` (white 0).
    pub const fn rgb(rgb: u32) -> Self {
        Self::new((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 0)
    }

    /// `0xRRGGBB`.
    pub const fn to_rgb(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }

    /// Every component scaled by `level / 255`.
    pub const fn scaled(self, level: u8) -> Self {
        const fn s(c: u8, level: u8) -> u8 {
            ((c as u16 * level as u16 + 127) / 255) as u8
        }
        Self::new(
            s(self.r, level),
            s(self.g, level),
            s(self.b, level),
            s(self.w, level),
        )
    }
}

/// A device that shows a frame of colours: an LED or an addressable strip.
pub trait Pixels: Device {
    /// How many LEDs.
    fn pixel_count(&self) -> usize;

    /// Shows `pixels` (index 0 is the first logical LED); LEDs past the end
    /// of `pixels` turn off.
    fn show(&mut self, pixels: &[Rgbw]) -> Result<(), DeviceError<Self::Error>>;
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
