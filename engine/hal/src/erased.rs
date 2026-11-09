//! Buses with their errors reduced to [`ErrorKind`], so buses of different
//! types fit one `dyn I2c<Error = ErrorKind>` (or `SpiDevice`, `OutputPin`).
//! Hosts that pick a bus at run time (from a board definition, from a
//! runtime session) hand drivers one of these.

use crate::{ErrorKind, HalError};
use embedded_hal::digital::{self, OutputPin};
use embedded_hal::i2c::{self, I2c, Operation};
use embedded_hal::spi::{self, SpiDevice};

/// A bus or pin whose errors become their [`ErrorKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Kind<B>(pub B);

impl<B> Kind<B> {
    pub fn into_inner(self) -> B {
        self.0
    }
}

impl<B: i2c::ErrorType> i2c::ErrorType for Kind<B>
where
    B::Error: HalError,
{
    type Error = ErrorKind;
}

impl<B: I2c> I2c for Kind<B>
where
    B::Error: HalError,
{
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), ErrorKind> {
        self.0
            .transaction(address, operations)
            .map_err(|error| error.kind())
    }
}

impl<B: spi::ErrorType> spi::ErrorType for Kind<B>
where
    B::Error: HalError,
{
    type Error = ErrorKind;
}

impl<B: SpiDevice> SpiDevice for Kind<B>
where
    B::Error: HalError,
{
    fn transaction(&mut self, operations: &mut [spi::Operation<'_, u8>]) -> Result<(), ErrorKind> {
        self.0.transaction(operations).map_err(|error| error.kind())
    }
}

impl<B: digital::ErrorType> digital::ErrorType for Kind<B>
where
    B::Error: HalError,
{
    type Error = ErrorKind;
}

impl<B: OutputPin> OutputPin for Kind<B>
where
    B::Error: HalError,
{
    fn set_low(&mut self) -> Result<(), ErrorKind> {
        self.0.set_low().map_err(|error| error.kind())
    }

    fn set_high(&mut self) -> Result<(), ErrorKind> {
        self.0.set_high().map_err(|error| error.kind())
    }
}
