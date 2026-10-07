//! The async BMI088 driver, over embedded-hal-async I2C.

use crate::{
    ACC_CHIP_ID, ACC_DATA, ACC_POWER_ON_US, ACC_PWR_CONF, ACC_PWR_CTRL, ACC_RESET_US,
    ACC_SOFTRESET, ACC_SUSPEND_WRITE_US, ACC_TEMP, ACCEL_ADDRESS, ACCEL_CHIP_ID, Config, Error,
    GYR_CHIP_ID, GYR_DATA, GYR_RESET_US, GYR_SOFTRESET, GYRO_ADDRESS, GYRO_CHIP_ID, ImuFixed,
    SOFTRESET, decode_axes, decode_temperature_mc,
};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::i2c::I2c;
use lemnos_hal::asynch::RegisterBus;
use lemnos_hal::register::{AddressWidth, I2cRegisters};

/// A BMI088 over an async I2C bus; see [`crate::Bmi088`].
#[derive(Debug)]
pub struct Bmi088<I2C> {
    i2c: I2C,
    accel_address: u8,
    gyro_address: u8,
    pub(crate) config: Option<Config>,
    pub(crate) settings: Config,
}

impl<I2C: I2c> Bmi088<I2C> {
    pub fn new(i2c: I2C) -> Self {
        Self::with_addresses(i2c, ACCEL_ADDRESS, GYRO_ADDRESS)
    }

    pub fn with_addresses(i2c: I2C, accel_address: u8, gyro_address: u8) -> Self {
        Self {
            i2c,
            accel_address,
            gyro_address,
            config: None,
            settings: Config::default(),
        }
    }

    /// Sets the configuration `lemnos_device::asynch::Device::init` applies.
    pub fn with_config(mut self, config: Config) -> Self {
        self.settings = config;
        self
    }

    /// A BMI088 that an earlier `init` already configured with
    /// `config`, for example after `release` or when a
    /// runtime adapter wraps a borrowed bus per operation. Touches no
    /// registers.
    pub fn resume(i2c: I2C, accel_address: u8, gyro_address: u8, config: Config) -> Self {
        Self {
            config: Some(config),
            settings: config,
            ..Self::with_addresses(i2c, accel_address, gyro_address)
        }
    }

    pub fn config(&self) -> Option<Config> {
        self.config
    }

    fn accel(&mut self) -> I2cRegisters<&mut I2C> {
        I2cRegisters::new(&mut self.i2c, self.accel_address, AddressWidth::Bits8)
    }

    fn gyro(&mut self) -> I2cRegisters<&mut I2C> {
        I2cRegisters::new(&mut self.i2c, self.gyro_address, AddressWidth::Bits8)
    }

    pub async fn chip_ids(&mut self) -> Result<(u8, u8), Error<I2C::Error>> {
        let accel = self.accel().read8(ACC_CHIP_ID).await?;
        let gyro = self.gyro().read8(GYR_CHIP_ID).await?;
        Ok((accel, gyro))
    }

    /// See [`crate::Bmi088::init`].
    pub async fn init(
        &mut self,
        delay: &mut impl DelayNs,
        config: Config,
    ) -> Result<(), Error<I2C::Error>> {
        let (accel, gyro) = self.chip_ids().await?;
        if accel != ACCEL_CHIP_ID || gyro != GYRO_CHIP_ID {
            return Err(Error::WrongChip { accel, gyro });
        }
        self.accel().write8(ACC_SOFTRESET, SOFTRESET).await?;
        delay.delay_us(ACC_RESET_US).await;
        self.gyro().write8(GYR_SOFTRESET, SOFTRESET).await?;
        delay.delay_us(GYR_RESET_US).await;

        self.accel().write8(ACC_PWR_CONF, 0x00).await?;
        delay.delay_us(ACC_SUSPEND_WRITE_US).await;
        self.accel().write8(ACC_PWR_CTRL, 0x04).await?;
        delay.delay_us(ACC_POWER_ON_US).await;

        for w in config.accel_writes() {
            self.accel().write(w.address, w.bytes, w.value).await?;
        }
        for w in config.gyro_writes() {
            self.gyro().write(w.address, w.bytes, w.value).await?;
        }
        self.config = Some(config);
        self.settings = config;
        Ok(())
    }

    pub async fn read_accel_raw(&mut self) -> Result<[i16; 3], Error<I2C::Error>> {
        let mut bytes = [0u8; 6];
        self.accel().read_burst(ACC_DATA, &mut bytes).await?;
        Ok(decode_axes(bytes))
    }

    pub async fn read_gyro_raw(&mut self) -> Result<[i16; 3], Error<I2C::Error>> {
        let mut bytes = [0u8; 6];
        self.gyro().read_burst(GYR_DATA, &mut bytes).await?;
        Ok(decode_axes(bytes))
    }

    pub async fn read_fixed(&mut self) -> Result<ImuFixed, Error<I2C::Error>> {
        let config = self.config.ok_or(Error::NotInitialized)?;
        let accel = self.read_accel_raw().await?;
        let gyro = self.read_gyro_raw().await?;
        Ok(config.fixed(accel, gyro))
    }

    #[cfg(feature = "float")]
    pub async fn read(&mut self) -> Result<crate::ImuSample, Error<I2C::Error>> {
        let config = self.config.ok_or(Error::NotInitialized)?;
        let accel = self.read_accel_raw().await?;
        let gyro = self.read_gyro_raw().await?;
        Ok(config.sample(accel, gyro))
    }

    pub async fn temperature_mc(&mut self) -> Result<i32, Error<I2C::Error>> {
        let mut bytes = [0u8; 2];
        self.accel().read_burst(ACC_TEMP, &mut bytes).await?;
        Ok(decode_temperature_mc(bytes[0], bytes[1]))
    }

    #[cfg(feature = "float")]
    pub async fn temperature_c(&mut self) -> Result<f32, Error<I2C::Error>> {
        Ok(self.temperature_mc().await? as f32 / 1000.0)
    }

    pub fn release(self) -> I2C {
        self.i2c
    }
}
