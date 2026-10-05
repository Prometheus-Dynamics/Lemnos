//! The async BMM150 driver, over embedded-hal-async I2C.

use crate::{
    CHIP_ID, Config, Error, MagneticFieldFixed, POWER_ON_US, REG_CHIP_ID, REG_DATA, REG_OP_MODE,
    REG_POWER, REG_REP_XY, REG_REP_Z, REG_TRIM_X1, REG_TRIM_Z2, REG_TRIM_Z4, RawSample, Trim,
};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::i2c::I2c;
use lemnos_hal::asynch::RegisterBus;
use lemnos_hal::register::{AddressWidth, I2cRegisters};

/// A BMM150 over an async I2C bus; see [`crate::Bmm150`].
#[derive(Debug)]
pub struct Bmm150<I2C> {
    i2c: I2C,
    address: u8,
    trim: Option<Trim>,
}

impl<I2C: I2c> Bmm150<I2C> {
    pub fn new(i2c: I2C, address: u8) -> Self {
        Self {
            i2c,
            address,
            trim: None,
        }
    }

    /// A BMM150 that an earlier `init` already powered up and configured,
    /// using the [`Trim`] it read. Touches no registers.
    pub fn resume(i2c: I2C, address: u8, trim: Trim) -> Self {
        Self {
            i2c,
            address,
            trim: Some(trim),
        }
    }

    pub fn trim(&self) -> Option<&Trim> {
        self.trim.as_ref()
    }

    fn registers(&mut self) -> I2cRegisters<&mut I2C> {
        I2cRegisters::new(&mut self.i2c, self.address, AddressWidth::Bits8)
    }

    /// See [`crate::Bmm150::init`].
    pub async fn init(
        &mut self,
        delay: &mut impl DelayNs,
        config: Config,
    ) -> Result<(), Error<I2C::Error>> {
        self.registers().write8(REG_POWER, 0x01).await?;
        delay.delay_us(POWER_ON_US).await;
        let mut regs = self.registers();
        let found = regs.read8(REG_CHIP_ID).await?;
        if found != CHIP_ID {
            return Err(Error::WrongChip { found });
        }
        let (mut x1y1, mut z4x2y2, mut rest) = ([0u8; 2], [0u8; 4], [0u8; 10]);
        regs.read_burst(REG_TRIM_X1, &mut x1y1).await?;
        regs.read_burst(REG_TRIM_Z4, &mut z4x2y2).await?;
        regs.read_burst(REG_TRIM_Z2, &mut rest).await?;
        let (rep_xy, rep_z) = config.preset.registers();
        regs.write8(REG_REP_XY, rep_xy).await?;
        regs.write8(REG_REP_Z, rep_z).await?;
        regs.write8(REG_OP_MODE, config.op_mode()).await?;
        self.trim = Some(Trim::from_registers(x1y1, z4x2y2, rest));
        Ok(())
    }

    pub async fn read_raw(&mut self) -> Result<RawSample, Error<I2C::Error>> {
        let mut bytes = [0u8; 8];
        self.registers().read_burst(REG_DATA, &mut bytes).await?;
        Ok(RawSample::from_registers(bytes))
    }

    pub async fn read_fixed(&mut self) -> Result<MagneticFieldFixed, Error<I2C::Error>> {
        let trim = self.trim.ok_or(Error::NotInitialized)?;
        Ok(trim.compensate_fixed(self.read_raw().await?))
    }

    #[cfg(feature = "float")]
    pub async fn read(&mut self) -> Result<crate::MagneticField, Error<I2C::Error>> {
        let trim = self.trim.ok_or(Error::NotInitialized)?;
        Ok(trim.compensate(self.read_raw().await?))
    }

    pub async fn power_down(&mut self) -> Result<(), Error<I2C::Error>> {
        self.registers().write8(REG_POWER, 0x00).await?;
        self.trim = None;
        Ok(())
    }

    pub fn release(self) -> I2C {
        self.i2c
    }
}
