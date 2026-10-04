//! The async twin of [`RegisterBus`](super::RegisterBus), for
//! embedded-hal-async buses (Embassy, RTIC, async Linux wrappers).

#![allow(async_fn_in_trait)]

use super::{
    Endian, RegWrite, RegisterError, RegisterResult, check_width, decode_value, encode_value,
};
use core::fmt;

/// Async register access to one device; see [`super::RegisterBus`] for the
/// semantics. Only [`read_burst`](Self::read_burst) and
/// [`write_burst`](Self::write_burst) are required.
pub trait RegisterBus {
    /// The underlying bus's error.
    type BusError: fmt::Debug;

    /// Reads `buf.len()` consecutive registers starting at `address`.
    async fn read_burst(
        &mut self,
        address: u16,
        buf: &mut [u8],
    ) -> RegisterResult<(), Self::BusError>;

    /// Writes `data` to consecutive registers starting at `address`, in one
    /// transfer.
    async fn write_burst(
        &mut self,
        address: u16,
        data: &[u8],
    ) -> RegisterResult<(), Self::BusError>;

    /// Byte order of multi-byte values.
    fn endian(&self) -> Endian {
        Endian::Big
    }

    /// Reads `bytes` (1 to 4) registers starting at `address` as one value.
    async fn read(&mut self, address: u16, bytes: u8) -> RegisterResult<u32, Self::BusError> {
        let n = check_width(bytes)?;
        let mut buf = [0u8; 4];
        self.read_burst(address, &mut buf[..n]).await?;
        Ok(decode_value(&buf[..n], self.endian()))
    }

    /// Writes the low `bytes` (1 to 4) bytes of `value` starting at `address`.
    async fn write(
        &mut self,
        address: u16,
        bytes: u8,
        value: u32,
    ) -> RegisterResult<(), Self::BusError> {
        let mut buf = [0u8; 4];
        let n = encode_value(value, bytes, self.endian(), &mut buf)?;
        self.write_burst(address, &buf[..n]).await
    }

    /// Writes several registers in order. Implementations may batch them.
    async fn write_sequence(&mut self, writes: &[RegWrite]) -> RegisterResult<(), Self::BusError> {
        for w in writes {
            self.write(w.address, w.bytes, w.value).await?;
        }
        Ok(())
    }

    /// Read-modify-write: replaces the bits in `mask` with those of `value`.
    /// Returns the value written.
    async fn modify(
        &mut self,
        address: u16,
        bytes: u8,
        mask: u32,
        value: u32,
    ) -> RegisterResult<u32, Self::BusError> {
        let old = self.read(address, bytes).await?;
        let new = (old & !mask) | (value & mask);
        if new != old {
            self.write(address, bytes, new).await?;
        }
        Ok(new)
    }

    /// Writes, reads back and compares.
    async fn write_verified(
        &mut self,
        address: u16,
        bytes: u8,
        value: u32,
    ) -> RegisterResult<(), Self::BusError> {
        self.write(address, bytes, value).await?;
        let read = self.read(address, bytes).await?;
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
    async fn read8(&mut self, address: u16) -> RegisterResult<u8, Self::BusError> {
        self.read(address, 1).await.map(|v| v as u8)
    }

    /// Reads a two-register value.
    async fn read16(&mut self, address: u16) -> RegisterResult<u16, Self::BusError> {
        self.read(address, 2).await.map(|v| v as u16)
    }

    /// Writes one register.
    async fn write8(&mut self, address: u16, value: u8) -> RegisterResult<(), Self::BusError> {
        self.write(address, 1, value.into()).await
    }

    /// Writes a two-register value.
    async fn write16(&mut self, address: u16, value: u16) -> RegisterResult<(), Self::BusError> {
        self.write(address, 2, value.into()).await
    }
}

impl<B: RegisterBus + ?Sized> RegisterBus for &mut B {
    type BusError = B::BusError;

    async fn read_burst(
        &mut self,
        address: u16,
        buf: &mut [u8],
    ) -> RegisterResult<(), Self::BusError> {
        (**self).read_burst(address, buf).await
    }
    async fn write_burst(
        &mut self,
        address: u16,
        data: &[u8],
    ) -> RegisterResult<(), Self::BusError> {
        (**self).write_burst(address, data).await
    }
    fn endian(&self) -> Endian {
        (**self).endian()
    }
    async fn write_sequence(&mut self, writes: &[RegWrite]) -> RegisterResult<(), Self::BusError> {
        (**self).write_sequence(writes).await
    }
}
