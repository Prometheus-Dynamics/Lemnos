//! Voice-coil motor (camera focus lens) drivers over embedded-hal I2C,
//! without `std` or allocation.
//!
//! [`Vcm`] drives a chip through its [`VcmFormat`]: the built-in
//! [`VcmChip`]s (DW9714, DW9807, DW9817, AK7375) or a custom format
//! ([`VcmChip::Custom`]). With `alloc`, `OwnedVcmFormat` holds a format
//! loaded at run time; with `serde`, [`VcmChip`] (and with both, the owned
//! format) (de)serialize, so descriptions can name the chip or spell out its
//! format.
//! [`asynch::Vcm`] is the same over embedded-hal-async. Ported from Styx
//! (`styx-sensor/src/lens.rs` command formats, `styx-native`'s `I2cVcm`).
//!
//! VCMs report no position: the driver remembers the last position written.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod asynch;
mod device;
mod format;
#[cfg(feature = "alloc")]
mod owned;

#[cfg(test)]
mod tests;

pub use device::info_for_bits;
pub use format::{FormatError, MAX_VCM_WRITES, VcmChip, VcmFormat};
#[cfg(feature = "alloc")]
pub use owned::{OwnedVcmFormat, VcmFormatRefs};

use core::fmt;
use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;
use lemnos_hal::{ErrorKind, HalError};

/// The usual 7-bit address of these VCMs.
pub const DEFAULT_ADDRESS: u8 = 0x0c;

/// The error of a VCM operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VcmError<E> {
    /// The I2C transfer failed.
    I2c(E),
    /// The command format is unusable.
    Format(FormatError),
}

impl<E: embedded_hal::i2c::Error> HalError for VcmError<E> {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::I2c(e) => ErrorKind::from_i2c(e.kind()),
            Self::Format(_) => ErrorKind::InvalidInput,
        }
    }
}

impl<E: fmt::Debug> fmt::Display for VcmError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::I2c(e) => write!(f, "VCM I2C transfer failed: {e:?}"),
            Self::Format(e) => write!(f, "VCM format: {e}"),
        }
    }
}

impl<E: fmt::Debug> core::error::Error for VcmError<E> {}

/// State shared by the blocking and async drivers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct State<'a> {
    pub(crate) address: u8,
    pub(crate) format: VcmFormat<'a>,
    pub(crate) position: Option<i32>,
    pub(crate) powered: bool,
}

impl<'a> State<'a> {
    pub(crate) fn new(address: u8, format: VcmFormat<'a>) -> Result<Self, FormatError> {
        format.check()?;
        Ok(Self {
            address,
            format,
            position: None,
            powered: false,
        })
    }
}

/// A VCM over a blocking I2C bus.
#[derive(Debug)]
pub struct Vcm<'a, I2C> {
    i2c: I2C,
    state: State<'a>,
}

impl<I2C: I2c> Vcm<'static, I2C> {
    /// A built-in chip at [`DEFAULT_ADDRESS`].
    ///
    /// # Panics
    ///
    /// For [`VcmChip::Custom`], which has no built-in format: use
    /// [`Vcm::new`] with its format.
    #[track_caller]
    pub fn chip(i2c: I2C, chip: VcmChip) -> Self {
        Self {
            i2c,
            state: State::new(DEFAULT_ADDRESS, chip.format()).expect("built-in formats are valid"),
        }
    }

    /// A DW9714 at 0x0c.
    pub fn dw9714(i2c: I2C) -> Self {
        Self::chip(i2c, VcmChip::Dw9714)
    }

    /// A DW9807 at 0x0c.
    pub fn dw9807(i2c: I2C) -> Self {
        Self::chip(i2c, VcmChip::Dw9807)
    }

    /// A DW9817 (Raspberry Pi Camera Module 3) at 0x0c.
    pub fn dw9817(i2c: I2C) -> Self {
        Self::chip(i2c, VcmChip::Dw9817)
    }

    /// An AK7375 at 0x0c.
    pub fn ak7375(i2c: I2C) -> Self {
        Self::chip(i2c, VcmChip::Ak7375)
    }
}

impl<'a, I2C: I2c> Vcm<'a, I2C> {
    /// A VCM at 7-bit `address` driven with `format`.
    pub fn new(i2c: I2C, address: u8, format: VcmFormat<'a>) -> Result<Self, FormatError> {
        Ok(Self {
            i2c,
            state: State::new(address, format)?,
        })
    }

    /// The command format.
    pub fn format(&self) -> &VcmFormat<'a> {
        &self.state.format
    }

    /// The 7-bit address.
    pub fn address(&self) -> u8 {
        self.state.address
    }

    /// The last position written (clamped), if any.
    pub fn position(&self) -> Option<i32> {
        self.state.position
    }

    /// Whether [`power_up`](Self::power_up) ran last.
    pub fn is_powered(&self) -> bool {
        self.state.powered
    }

    /// Sends the power-up writes and waits the chip's settle time.
    pub fn power_up(&mut self, delay: &mut impl DelayNs) -> Result<(), VcmError<I2C::Error>> {
        for message in self.state.format.power_up.iter().filter(|m| !m.is_empty()) {
            self.i2c
                .write(self.state.address, message)
                .map_err(VcmError::I2c)?;
        }
        if self.state.format.power_up_us > 0 {
            delay.delay_us(self.state.format.power_up_us);
        }
        self.state.powered = true;
        Ok(())
    }

    /// Sends the standby writes.
    pub fn power_down(&mut self) -> Result<(), VcmError<I2C::Error>> {
        for message in self
            .state
            .format
            .power_down
            .iter()
            .filter(|m| !m.is_empty())
        {
            self.i2c
                .write(self.state.address, message)
                .map_err(VcmError::I2c)?;
        }
        self.state.powered = false;
        Ok(())
    }

    /// Moves to `position` (clamped to the chip's range); returns the
    /// position written.
    pub fn move_to(&mut self, position: i32) -> Result<i32, VcmError<I2C::Error>> {
        let (message, len, clamped) = self.state.format.encode(position);
        self.i2c
            .write(self.state.address, &message[..len])
            .map_err(VcmError::I2c)?;
        self.state.position = Some(clamped);
        Ok(clamped)
    }

    /// Gives the bus back.
    pub fn release(self) -> I2C {
        self.i2c
    }
}
