//! [`Blocking`]: a blocking bus, register map or delay used through the async
//! traits, for async driver code over blocking buses.

use crate::Endian;
use crate::register::{RegWrite, RegisterBus, RegisterResult, asynch};
use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::{ErrorType as I2cErrorType, I2c, Operation as I2cOperation};
use embedded_hal::spi::{ErrorType as SpiErrorType, Operation as SpiOperation, SpiDevice};

/// Wraps a blocking implementation so it implements the async twin of its
/// trait: [`asynch::RegisterBus`] for a [`RegisterBus`] (such as
/// [`I2cRegisters`](crate::I2cRegisters) over a blocking I2C bus), and the
/// embedded-hal-async `I2c`, `SpiDevice` and `DelayNs` for their blocking
/// counterparts.
///
/// Every operation runs to completion inside the first poll: the futures
/// never return `Pending`, and they block the executor for the transfer's
/// duration. That suits an async driver core run over a blocking bus (a
/// Linux `/dev/i2c-N`, a simple MCU HAL), not a busy shared executor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Blocking<T>(pub T);

impl<T> Blocking<T> {
    pub const fn new(inner: T) -> Self {
        Self(inner)
    }

    pub fn into_inner(self) -> T {
        self.0
    }

    pub const fn get_ref(&self) -> &T {
        &self.0
    }

    pub fn get_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// The result of a register operation on the blocking map `R`.
type Result<T, R> = RegisterResult<T, <R as RegisterBus>::BusError>;

impl<R: RegisterBus> asynch::RegisterBus for Blocking<R> {
    type BusError = R::BusError;

    async fn read_burst(&mut self, address: u16, buf: &mut [u8]) -> Result<(), R> {
        self.0.read_burst(address, buf)
    }

    async fn write_burst(&mut self, address: u16, data: &[u8]) -> Result<(), R> {
        self.0.write_burst(address, data)
    }

    fn endian(&self) -> Endian {
        self.0.endian()
    }

    async fn read(&mut self, address: u16, bytes: u8) -> Result<u32, R> {
        self.0.read(address, bytes)
    }

    async fn write(&mut self, address: u16, bytes: u8, value: u32) -> Result<(), R> {
        self.0.write(address, bytes, value)
    }

    async fn write_sequence(&mut self, writes: &[RegWrite]) -> Result<(), R> {
        self.0.write_sequence(writes)
    }

    async fn modify(&mut self, address: u16, bytes: u8, mask: u32, value: u32) -> Result<u32, R> {
        self.0.modify(address, bytes, mask, value)
    }

    async fn write_verified(&mut self, address: u16, bytes: u8, value: u32) -> Result<(), R> {
        self.0.write_verified(address, bytes, value)
    }
}

impl<T: I2cErrorType> I2cErrorType for Blocking<T> {
    type Error = T::Error;
}

impl<T: I2c> embedded_hal_async::i2c::I2c for Blocking<T> {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [I2cOperation<'_>],
    ) -> core::result::Result<(), Self::Error> {
        self.0.transaction(address, operations)
    }
}

impl<T: SpiErrorType> SpiErrorType for Blocking<T> {
    type Error = T::Error;
}

impl<T: SpiDevice> embedded_hal_async::spi::SpiDevice for Blocking<T> {
    async fn transaction(
        &mut self,
        operations: &mut [SpiOperation<'_, u8>],
    ) -> core::result::Result<(), Self::Error> {
        self.0.transaction(operations)
    }
}

impl<T: DelayNs> embedded_hal_async::delay::DelayNs for Blocking<T> {
    async fn delay_ns(&mut self, ns: u32) {
        self.0.delay_ns(ns);
    }

    async fn delay_us(&mut self, us: u32) {
        self.0.delay_us(us);
    }

    async fn delay_ms(&mut self, ms: u32) {
        self.0.delay_ms(ms);
    }
}
