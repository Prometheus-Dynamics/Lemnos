//! Register maps over I2C and SPI, blocking ([`RegisterBus`]) and async
//! ([`asynch::RegisterBus`]), without allocation.
//!
//! Most sensors, PMICs, motor drivers and camera modules expose a flat map of
//! 8-bit registers behind an 8- or 16-bit register address. A value wider than
//! one byte occupies consecutive registers (big-endian for camera sensors,
//! little-endian for many IMUs), and the device auto-increments the register
//! address after every byte, so consecutive registers can be written in one
//! burst.
//!
//! [`I2cRegisters`] and [`SpiRegisters`] implement both traits over any
//! embedded-hal 1.0 bus. Ported from Styx (`styx-sensor`'s `RegisterBus` and
//! `styx-native`'s `I2cRegisterBus`).

pub mod asynch;
mod i2c;
mod spi;

#[cfg(test)]
mod tests;

use crate::{ErrorKind, HalError};
use core::fmt;

pub use i2c::I2cRegisters;
pub use spi::SpiRegisters;

/// Longest burst [`I2cRegisters::with_bursts`] and [`SpiRegisters::with_bursts`]
/// pack into one transfer, in data bytes (the stack buffer size).
pub const MAX_BURST: usize = 64;

/// Width of a register address on the wire (always sent most significant byte
/// first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AddressWidth {
    /// One byte.
    #[default]
    Bits8,
    /// Two bytes, big-endian (most camera sensors).
    Bits16,
}

impl AddressWidth {
    /// Bytes on the wire.
    pub const fn bytes(self) -> usize {
        match self {
            Self::Bits8 => 1,
            Self::Bits16 => 2,
        }
    }

    /// The width for `bits` (8 or 16).
    pub const fn from_bits(bits: u8) -> Option<Self> {
        match bits {
            8 => Some(Self::Bits8),
            16 => Some(Self::Bits16),
            _ => None,
        }
    }
}

/// Byte order of a multi-byte value across consecutive registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Endian {
    /// Most significant byte at the lowest address (camera sensors, most PMICs).
    #[default]
    Big,
    /// Least significant byte at the lowest address (many IMUs and ADCs).
    Little,
}

/// One register write: `bytes` (1 to 4) bytes of `value` starting at `address`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegWrite {
    /// First register.
    pub address: u16,
    /// The value (its low `bytes` bytes are written).
    pub value: u32,
    /// Number of registers (1 to 4).
    pub bytes: u8,
}

impl RegWrite {
    /// A write of `bytes` bytes.
    pub const fn new(address: u16, bytes: u8, value: u32) -> Self {
        Self {
            address,
            value,
            bytes,
        }
    }

    /// A one-byte write.
    pub const fn byte(address: u16, value: u8) -> Self {
        Self::new(address, 1, value as u32)
    }

    /// A two-byte write.
    pub const fn word(address: u16, value: u16) -> Self {
        Self::new(address, 2, value as u32)
    }
}

/// The error of a register operation.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError<E> {
    /// The bus transfer failed.
    Bus {
        /// Its classification, from the bus error's embedded-hal kind (the
        /// bus's own error keeps the details, e.g. the errno on Linux).
        kind: ErrorKind,
        /// The bus's own error.
        error: E,
    },
    /// A value width outside 1..=4 bytes.
    InvalidWidth(u8),
    /// The value does not fit the given number of bytes.
    ValueTooWide {
        /// The value.
        value: u32,
        /// The width it was written with.
        bytes: u8,
    },
    /// The register address does not fit the address width.
    AddressTooWide(u16),
    /// A transfer longer than the bus or buffer allows.
    TooLong(usize),
    /// A verified write read back a different value.
    Mismatch {
        /// The register.
        address: u16,
        /// What was written.
        wrote: u32,
        /// What was read back.
        read: u32,
    },
}

impl<E> RegisterError<E> {
    /// A bus error of the given kind.
    pub fn bus(kind: ErrorKind, error: E) -> Self {
        Self::Bus { kind, error }
    }

    /// The bus error, if this is one.
    pub fn bus_error(&self) -> Option<&E> {
        match self {
            Self::Bus { error, .. } => Some(error),
            _ => None,
        }
    }

    /// Maps the bus error type.
    pub fn map_bus<F>(self, f: impl FnOnce(E) -> F) -> RegisterError<F> {
        match self {
            Self::Bus { kind, error } => RegisterError::Bus {
                kind,
                error: f(error),
            },
            Self::InvalidWidth(b) => RegisterError::InvalidWidth(b),
            Self::ValueTooWide { value, bytes } => RegisterError::ValueTooWide { value, bytes },
            Self::AddressTooWide(a) => RegisterError::AddressTooWide(a),
            Self::TooLong(n) => RegisterError::TooLong(n),
            Self::Mismatch {
                address,
                wrote,
                read,
            } => RegisterError::Mismatch {
                address,
                wrote,
                read,
            },
        }
    }

    pub(crate) fn i2c(error: E) -> Self
    where
        E: embedded_hal::i2c::Error,
    {
        Self::Bus {
            kind: ErrorKind::from_i2c(error.kind()),
            error,
        }
    }

    pub(crate) fn spi(error: E) -> Self
    where
        E: embedded_hal::spi::Error,
    {
        Self::Bus {
            kind: ErrorKind::from_spi(error.kind()),
            error,
        }
    }
}

impl<E: fmt::Debug> HalError for RegisterError<E> {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Bus { kind, .. } => *kind,
            Self::Mismatch { .. } => ErrorKind::Failed,
            Self::InvalidWidth(_)
            | Self::ValueTooWide { .. }
            | Self::AddressTooWide(_)
            | Self::TooLong(_) => ErrorKind::InvalidInput,
        }
    }
}

impl<E: fmt::Debug> fmt::Display for RegisterError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bus { kind, error } => write!(f, "register bus {kind}: {error:?}"),
            Self::InvalidWidth(b) => write!(f, "{b}-byte register access (1..=4)"),
            Self::ValueTooWide { value, bytes } => {
                write!(f, "value {value:#x} does not fit {bytes} bytes")
            }
            Self::AddressTooWide(a) => write!(f, "register {a:#x} does not fit the address width"),
            Self::TooLong(n) => write!(f, "transfer of {n} bytes is too long"),
            Self::Mismatch {
                address,
                wrote,
                read,
            } => write!(
                f,
                "register {address:#06x} read back {read:#x} after writing {wrote:#x}"
            ),
        }
    }
}

impl<E: fmt::Debug> core::error::Error for RegisterError<E> {}

impl<E: fmt::Debug> embedded_hal::i2c::Error for RegisterError<E> {
    fn kind(&self) -> embedded_hal::i2c::ErrorKind {
        HalError::kind(self).to_i2c()
    }
}

/// Result of a register operation.
pub type RegisterResult<T, E> = Result<T, RegisterError<E>>;

/// Checks a value width (1..=4 bytes).
pub fn check_width<E>(bytes: u8) -> RegisterResult<usize, E> {
    if (1..=4).contains(&bytes) {
        Ok(usize::from(bytes))
    } else {
        Err(RegisterError::InvalidWidth(bytes))
    }
}

/// Encodes a register address into the start of `out`; returns the bytes used.
pub fn encode_address<E>(
    address: u16,
    width: AddressWidth,
    out: &mut [u8],
) -> RegisterResult<usize, E> {
    match width {
        AddressWidth::Bits8 => {
            out[0] = u8::try_from(address).map_err(|_| RegisterError::AddressTooWide(address))?;
            Ok(1)
        }
        AddressWidth::Bits16 => {
            out[..2].copy_from_slice(&address.to_be_bytes());
            Ok(2)
        }
    }
}

/// Encodes the low `bytes` bytes of `value` into the start of `out`.
pub fn encode_value<E>(
    value: u32,
    bytes: u8,
    endian: Endian,
    out: &mut [u8],
) -> RegisterResult<usize, E> {
    let n = check_width(bytes)?;
    if n < 4 && value >> (8 * n) != 0 {
        return Err(RegisterError::ValueTooWide { value, bytes });
    }
    match endian {
        Endian::Big => out[..n].copy_from_slice(&value.to_be_bytes()[4 - n..]),
        Endian::Little => out[..n].copy_from_slice(&value.to_le_bytes()[..n]),
    }
    Ok(n)
}

/// Decodes a value from consecutive register bytes.
pub fn decode_value(bytes: &[u8], endian: Endian) -> u32 {
    match endian {
        Endian::Big => bytes.iter().fold(0u32, |acc, b| (acc << 8) | u32::from(*b)),
        Endian::Little => bytes
            .iter()
            .rev()
            .fold(0u32, |acc, b| (acc << 8) | u32::from(*b)),
    }
}

/// Encodes a register write (address then value) into a fixed buffer; returns
/// the buffer and the length used.
pub fn encode_write<E>(
    address: u16,
    bytes: u8,
    value: u32,
    width: AddressWidth,
    endian: Endian,
) -> RegisterResult<([u8; 6], usize), E> {
    let mut buf = [0u8; 6];
    let a = encode_address(address, width, &mut buf)?;
    let n = encode_value(value, bytes, endian, &mut buf[a..])?;
    Ok((buf, a + n))
}

/// Packs `writes[start..]` into `buf` as one burst: the (flagged) address of
/// the first write, then the data of every following write to the next
/// consecutive register, up to `burst` data bytes. Returns the bytes used and
/// the index of the first write not packed.
pub(crate) fn pack_run<E>(
    writes: &[RegWrite],
    start: usize,
    width: AddressWidth,
    endian: Endian,
    burst: usize,
    address_flag: u16,
    buf: &mut [u8],
) -> RegisterResult<(usize, usize), E> {
    let first = writes[start];
    let header = encode_address(first.address | address_flag, width, buf)?;
    let mut used = header + encode_value(first.value, first.bytes, endian, &mut buf[header..])?;
    let mut next = first.address.checked_add(u16::from(first.bytes));
    let mut i = start + 1;
    while let Some(w) = writes.get(i) {
        let n = check_width(w.bytes)?;
        if next != Some(w.address) || used - header + n > burst || used + n > buf.len() {
            break;
        }
        used += encode_value(w.value, w.bytes, endian, &mut buf[used..])?;
        next = w.address.checked_add(u16::from(w.bytes));
        i += 1;
    }
    Ok((used, i))
}

/// Register access to one device. Addresses are 8 or 16 bits as the
/// implementation is configured; values wider than one byte are consecutive
/// registers in the implementation's [`Endian`] order.
///
/// Only [`read_burst`](Self::read_burst) and [`write_burst`](Self::write_burst)
/// are required; implementations may override the rest to batch transfers.
pub trait RegisterBus {
    /// The underlying bus's error.
    type BusError: fmt::Debug;

    /// Reads `buf.len()` consecutive registers starting at `address`.
    fn read_burst(&mut self, address: u16, buf: &mut [u8]) -> RegisterResult<(), Self::BusError>;

    /// Writes `data` to consecutive registers starting at `address`, in one
    /// transfer.
    fn write_burst(&mut self, address: u16, data: &[u8]) -> RegisterResult<(), Self::BusError>;

    /// Byte order of multi-byte values.
    fn endian(&self) -> Endian {
        Endian::Big
    }

    /// Reads `bytes` (1 to 4) registers starting at `address` as one value.
    fn read(&mut self, address: u16, bytes: u8) -> RegisterResult<u32, Self::BusError> {
        let n = check_width(bytes)?;
        let mut buf = [0u8; 4];
        self.read_burst(address, &mut buf[..n])?;
        Ok(decode_value(&buf[..n], self.endian()))
    }

    /// Writes the low `bytes` (1 to 4) bytes of `value` starting at `address`.
    fn write(&mut self, address: u16, bytes: u8, value: u32) -> RegisterResult<(), Self::BusError> {
        let mut buf = [0u8; 4];
        let n = encode_value(value, bytes, self.endian(), &mut buf)?;
        self.write_burst(address, &buf[..n])
    }

    /// Writes several registers in order. Implementations may batch them.
    fn write_sequence(&mut self, writes: &[RegWrite]) -> RegisterResult<(), Self::BusError> {
        writes
            .iter()
            .try_for_each(|w| self.write(w.address, w.bytes, w.value))
    }

    /// Read-modify-write: replaces the bits in `mask` with those of `value`.
    /// Returns the value written.
    fn modify(
        &mut self,
        address: u16,
        bytes: u8,
        mask: u32,
        value: u32,
    ) -> RegisterResult<u32, Self::BusError> {
        let old = self.read(address, bytes)?;
        let new = (old & !mask) | (value & mask);
        if new != old {
            self.write(address, bytes, new)?;
        }
        Ok(new)
    }

    /// Writes, reads back and compares.
    fn write_verified(
        &mut self,
        address: u16,
        bytes: u8,
        value: u32,
    ) -> RegisterResult<(), Self::BusError> {
        self.write(address, bytes, value)?;
        let read = self.read(address, bytes)?;
        if read == value {
            Ok(())
        } else {
            Err(RegisterError::Mismatch {
                address,
                wrote: value,
                read,
            })
        }
    }

    /// Reads one register.
    fn read8(&mut self, address: u16) -> RegisterResult<u8, Self::BusError> {
        self.read(address, 1).map(|v| v as u8)
    }

    /// Reads a two-register value.
    fn read16(&mut self, address: u16) -> RegisterResult<u16, Self::BusError> {
        self.read(address, 2).map(|v| v as u16)
    }

    /// Reads a four-register value.
    fn read32(&mut self, address: u16) -> RegisterResult<u32, Self::BusError> {
        self.read(address, 4)
    }

    /// Writes one register.
    fn write8(&mut self, address: u16, value: u8) -> RegisterResult<(), Self::BusError> {
        self.write(address, 1, value.into())
    }

    /// Writes a two-register value.
    fn write16(&mut self, address: u16, value: u16) -> RegisterResult<(), Self::BusError> {
        self.write(address, 2, value.into())
    }

    /// Writes a four-register value.
    fn write32(&mut self, address: u16, value: u32) -> RegisterResult<(), Self::BusError> {
        self.write(address, 4, value)
    }
}

impl<B: RegisterBus + ?Sized> RegisterBus for &mut B {
    type BusError = B::BusError;

    fn read_burst(&mut self, address: u16, buf: &mut [u8]) -> RegisterResult<(), Self::BusError> {
        (**self).read_burst(address, buf)
    }
    fn write_burst(&mut self, address: u16, data: &[u8]) -> RegisterResult<(), Self::BusError> {
        (**self).write_burst(address, data)
    }
    fn endian(&self) -> Endian {
        (**self).endian()
    }
    fn read(&mut self, address: u16, bytes: u8) -> RegisterResult<u32, Self::BusError> {
        (**self).read(address, bytes)
    }
    fn write(&mut self, address: u16, bytes: u8, value: u32) -> RegisterResult<(), Self::BusError> {
        (**self).write(address, bytes, value)
    }
    fn write_sequence(&mut self, writes: &[RegWrite]) -> RegisterResult<(), Self::BusError> {
        (**self).write_sequence(writes)
    }
}
