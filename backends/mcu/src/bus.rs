use embedded_hal::i2c::{self, Error as _, I2c, Operation};
use embedded_hal::spi::{self, Error as _, SpiDevice};
use lemnos_hal::ErrorKind;

/// An embedded-hal I2C bus whose errors are [`ErrorKind`].
#[derive(Debug)]
pub struct I2cBus<B>(pub B);

impl<B: I2c> i2c::ErrorType for I2cBus<B> {
    type Error = ErrorKind;
}

impl<B: I2c> I2c for I2cBus<B> {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), ErrorKind> {
        self.0
            .transaction(address, operations)
            .map_err(|error| ErrorKind::from_i2c(error.kind()))
    }
}

/// An embedded-hal SPI device whose errors are [`ErrorKind`].
#[derive(Debug)]
pub struct SpiBus<B>(pub B);

impl<B: SpiDevice> spi::ErrorType for SpiBus<B> {
    type Error = ErrorKind;
}

impl<B: SpiDevice> SpiDevice for SpiBus<B> {
    fn transaction(&mut self, operations: &mut [spi::Operation<'_, u8>]) -> Result<(), ErrorKind> {
        self.0
            .transaction(operations)
            .map_err(|error| ErrorKind::from_spi(error.kind()))
    }
}
