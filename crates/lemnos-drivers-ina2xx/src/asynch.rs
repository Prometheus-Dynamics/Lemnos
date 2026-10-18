//! The async INA2xx driver, over embedded-hal-async I2C.

use crate::{
    Config, ConfigError, Error, MAX_POLL_REGISTERS, Model, POLL_BYTES, ReadingFixed, Setup,
    poll_decode, poll_operations, poll_pointers,
};
use embedded_hal::i2c::Operation;
use embedded_hal_async::i2c::I2c;
use lemnos_hal::asynch::RegisterBus;
use lemnos_hal::register::{AddressWidth, I2cRegisters};
use lemnos_hal::{ErrorKind, register::RegisterError};

/// An INA2xx over an async I2C bus; see [`crate::Ina`].
#[derive(Debug)]
pub struct Ina<I2C> {
    i2c: I2C,
    address: u8,
    pub(crate) setup: Setup,
}

impl<I2C: I2c> Ina<I2C> {
    /// A chip at 7-bit `address`; see [`crate::Ina::new`].
    pub fn new(i2c: I2C, address: u8, model: Model, config: Config) -> Result<Self, ConfigError> {
        Ok(Self {
            i2c,
            address,
            setup: Setup::new(model, config)?,
        })
    }

    pub fn model(&self) -> Model {
        self.setup.model
    }

    fn registers(&mut self) -> I2cRegisters<&mut I2C> {
        I2cRegisters::new(&mut self.i2c, self.address, AddressWidth::Bits8)
    }

    /// Verifies the chip and starts continuous conversion with the
    /// calibration.
    pub async fn init(&mut self) -> Result<(), Error<I2C::Error>> {
        let setup = self.setup;
        let mut regs = self.registers();
        let manufacturer = regs.read16(setup.model.manufacturer_register()).await?;
        let device = regs.read16(setup.model.device_register()).await?;
        setup.check_ids(manufacturer, device)?;
        for w in setup.writes() {
            regs.write(w.address, w.bytes, w.value).await?;
        }
        Ok(())
    }

    /// One poll: every register of the readings, in one I2C transaction.
    async fn read_registers(&mut self) -> Result<[u32; 5], Error<I2C::Error>> {
        let registers = self.setup.registers();
        let mut pointers = [0u8; MAX_POLL_REGISTERS];
        poll_pointers(registers, &mut pointers);
        let mut data = [0u8; POLL_BYTES];
        let mut ops: [Operation<'_>; 2 * MAX_POLL_REGISTERS] =
            core::array::from_fn(|_| Operation::Write(&[]));
        let len = poll_operations(registers, &pointers, &mut data, &mut ops);
        self.i2c
            .transaction(self.address, &mut ops[..len])
            .await
            .map_err(|error| {
                Error::Register(RegisterError::bus(
                    ErrorKind::from_i2c(embedded_hal::i2c::Error::kind(&error)),
                    error,
                ))
            })?;
        Ok(poll_decode(registers, &data))
    }

    /// Reads the latest conversion in integer units.
    pub async fn read_fixed(&mut self) -> Result<ReadingFixed, Error<I2C::Error>> {
        let raw = self.read_registers().await?;
        Ok(self.setup.reading_fixed(&raw))
    }

    /// Reads the latest conversion in SI units.
    #[cfg(feature = "float")]
    pub async fn read(&mut self) -> Result<crate::Reading, Error<I2C::Error>> {
        let raw = self.read_registers().await?;
        Ok(self.setup.reading(&raw))
    }

    /// Gives the bus back.
    pub fn release(self) -> I2C {
        self.i2c
    }
}
