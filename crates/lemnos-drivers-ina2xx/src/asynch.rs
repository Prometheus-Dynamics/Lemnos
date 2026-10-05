//! The async INA2xx driver, over embedded-hal-async I2C.

use crate::{Config, ConfigError, Error, Model, ReadingFixed, Setup};
use embedded_hal_async::i2c::I2c;
use lemnos_hal::asynch::RegisterBus;
use lemnos_hal::register::{AddressWidth, I2cRegisters};

/// An INA2xx over an async I2C bus; see [`crate::Ina`].
#[derive(Debug)]
pub struct Ina<I2C> {
    i2c: I2C,
    address: u8,
    setup: Setup,
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

    async fn read_registers(&mut self) -> Result<[u32; 5], Error<I2C::Error>> {
        let registers = self.setup.registers();
        let mut raw = [0u32; 5];
        let mut regs = self.registers();
        for (slot, (register, bytes)) in raw.iter_mut().zip(registers) {
            *slot = regs.read(*register, *bytes).await?;
        }
        Ok(raw)
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
