//! embedded-hal 1.0 on Linux, in pure Rust: no C libraries, no `i2cdev`,
//! `spidev` or `gpio-cdev` crates.
//!
//! - `I2cBus` (feature `i2c`): an i2c-dev adapter as an embedded-hal
//!   [`I2c`](embedded_hal::i2c::I2c) bus. Each target address is claimed with
//!   `I2C_SLAVE` (never `I2C_SLAVE_FORCE`) before it is used, so addresses a
//!   kernel driver owns fail with [`ErrorKind::Busy`].
//! - `Spidev` (feature `spi`): a spidev node as an embedded-hal
//!   [`SpiDevice`](embedded_hal::spi::SpiDevice); a transaction is one
//!   `SPI_IOC_MESSAGE`, so chip select stays asserted throughout.
//! - `GpioChip`, `GpioLines`, `GpioLine` (feature `gpio-cdev`): the GPIO
//!   character device, uAPI v2: inputs, outputs, bias, drive, debounce, edge
//!   events, reconfiguration without releasing; `GpioLine` is an
//!   embedded-hal input/output pin.
//! - The [`lemnos_hal::raw`] traits: `RawLine` for `GpioLine`, `RawSpi` for
//!   `Spidev` (one SPI mode per transaction; speed and word size per
//!   segment).
//! - [`StdDelay`]: [`DelayNs`](embedded_hal::delay::DelayNs) over
//!   `std::thread::sleep`.
//!
//! The same code backs the `lemnos-bus` sessions of [`LinuxBackend`], so
//! drivers written against embedded-hal and the runtime see one
//! implementation. Errors are [`IoError`]: the OS error, classified by
//! [`ErrorKind`].
//!
//! [`LinuxBackend`]: crate::LinuxBackend

#[cfg(feature = "gpio-cdev")]
mod gpio;
#[cfg(all(feature = "gpio-cdev", feature = "tokio"))]
mod gpio_async;
#[cfg(feature = "i2c")]
mod i2c;
#[cfg(any(feature = "gpio-cdev", feature = "spi"))]
mod raw;
#[cfg(feature = "spi")]
mod spi;

#[cfg(feature = "gpio-cdev")]
pub use gpio::{
    EdgeEvent, EdgeKind, GpioChip, GpioChipInfo, GpioLine, GpioLineInfo, GpioLines, LineBias,
    LineDirection, LineDrive, LineEdge, LineSettings,
};
#[cfg(all(feature = "gpio-cdev", feature = "tokio"))]
pub use gpio_async::AsyncGpioLine;
#[cfg(feature = "i2c")]
pub use i2c::{I2cBus, I2cMessage, PioI2cBus};
#[cfg(feature = "gpio-cdev")]
pub use raw::line_settings;
#[cfg(feature = "spi")]
pub use spi::{SpiMode, SpiTransfer, Spidev};

use lemnos_hal::{ErrorKind, HalError};
use std::fmt;
use std::io;
use std::time::Duration;

/// An OS error from a Linux HAL device, classified by [`ErrorKind`] (from the
/// errno when there is one).
#[derive(Debug)]
pub struct IoError(io::Error);

impl IoError {
    /// The coarse classification.
    pub fn kind(&self) -> ErrorKind {
        match self.0.raw_os_error() {
            Some(errno) => ErrorKind::from_errno(errno),
            None => ErrorKind::from_io(self.0.kind()),
        }
    }

    /// The underlying I/O error.
    pub fn io(&self) -> &io::Error {
        &self.0
    }

    /// Unwraps the I/O error.
    pub fn into_io(self) -> io::Error {
        self.0
    }
}

impl From<io::Error> for IoError {
    fn from(error: io::Error) -> Self {
        Self(error)
    }
}

impl From<IoError> for io::Error {
    fn from(error: IoError) -> Self {
        error.0
    }
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for IoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

impl HalError for IoError {
    fn kind(&self) -> ErrorKind {
        IoError::kind(self)
    }
}

impl embedded_hal::i2c::Error for IoError {
    fn kind(&self) -> embedded_hal::i2c::ErrorKind {
        IoError::kind(self).to_i2c()
    }
}

impl embedded_hal::spi::Error for IoError {
    fn kind(&self) -> embedded_hal::spi::ErrorKind {
        IoError::kind(self).to_spi()
    }
}

impl embedded_hal::digital::Error for IoError {
    fn kind(&self) -> embedded_hal::digital::ErrorKind {
        embedded_hal::digital::ErrorKind::Other
    }
}

impl embedded_hal::pwm::Error for IoError {
    fn kind(&self) -> embedded_hal::pwm::ErrorKind {
        embedded_hal::pwm::ErrorKind::Other
    }
}

/// [`DelayNs`](embedded_hal::delay::DelayNs) over `std::thread::sleep`
/// (at least the time asked for; the scheduler may add more).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StdDelay;

impl embedded_hal::delay::DelayNs for StdDelay {
    fn delay_ns(&mut self, ns: u32) {
        std::thread::sleep(Duration::from_nanos(u64::from(ns)));
    }

    fn delay_us(&mut self, us: u32) {
        std::thread::sleep(Duration::from_micros(u64::from(us)));
    }

    fn delay_ms(&mut self, ms: u32) {
        std::thread::sleep(Duration::from_millis(u64::from(ms)));
    }
}

#[cfg(any(feature = "i2c", feature = "gpio-cdev"))]
pub(crate) fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(any(test, feature = "gpio-cdev"))]
pub(crate) fn c_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal::delay::DelayNs;

    #[test]
    fn classifies_errnos() {
        assert_eq!(
            IoError::from(io::Error::from_raw_os_error(16)).kind(),
            ErrorKind::Busy
        );
        assert_eq!(
            IoError::from(io::Error::from_raw_os_error(121)).kind(),
            ErrorKind::Nack
        );
        assert_eq!(
            IoError::from(io::Error::from_raw_os_error(13)).kind(),
            ErrorKind::PermissionDenied
        );
        let e = IoError::from(io::Error::from_raw_os_error(6));
        assert_eq!(
            embedded_hal::i2c::Error::kind(&e),
            embedded_hal::i2c::ErrorKind::NoAcknowledge(
                embedded_hal::i2c::NoAcknowledgeSource::Unknown
            )
        );
        assert_eq!(c_string(b"gpiochip0\0\0junk"), "gpiochip0");
    }

    #[test]
    fn std_delay_waits() {
        let start = std::time::Instant::now();
        StdDelay.delay_us(1500);
        assert!(start.elapsed() >= Duration::from_micros(1500));
    }
}
