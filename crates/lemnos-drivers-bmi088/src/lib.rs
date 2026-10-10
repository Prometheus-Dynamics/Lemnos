//! Bosch BMI088 6-axis IMU over embedded-hal I2C, without `std` or
//! allocation.
//!
//! The BMI088 is two dies on one bus: an accelerometer (0x18/0x19) and a
//! gyroscope (0x68/0x69). [`Bmi088`] owns the bus, verifies both chip IDs,
//! soft-resets both dies, takes the accelerometer out of its power-on suspend
//! mode, applies a [`Config`], and reads samples in fixed point
//! ([`ImuFixed`]: milli-g and milli-degrees per second) or, with the default
//! `float` feature, in SI units (`ImuSample`: m/s² and rad/s). Leave `float`
//! off on MCUs without an FPU to keep software float routines out of the
//! image.
//! [`asynch::Bmi088`] is the same over embedded-hal-async.

#![no_std]
#![forbid(unsafe_code)]

pub mod asynch;
mod device;
mod fifo;

#[cfg(test)]
mod tests;

pub use device::{INFO, KERNEL};
pub use fifo::MAX_SAMPLES;

#[cfg(feature = "float")]
use core::f32::consts::PI;
use core::fmt;
use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;
use lemnos_hal::register::{AddressWidth, I2cRegisters, RegWrite, RegisterBus, RegisterError};
use lemnos_hal::{ErrorKind, HalError};

/// Accelerometer address with SDO1 low (0x19 with SDO1 high).
pub const ACCEL_ADDRESS: u8 = 0x18;
/// Gyroscope address with SDO2 low (0x69 with SDO2 high).
pub const GYRO_ADDRESS: u8 = 0x68;
pub const ACCEL_CHIP_ID: u8 = 0x1e;
pub const GYRO_CHIP_ID: u8 = 0x0f;
/// Standard gravity, for converting g to m/s².
#[cfg(feature = "float")]
pub const STANDARD_GRAVITY: f32 = 9.806_65;

pub(crate) const ACC_CHIP_ID: u16 = 0x00;
pub(crate) const ACC_DATA: u16 = 0x12;
pub(crate) const ACC_TEMP: u16 = 0x22;
pub(crate) const ACC_CONF: u16 = 0x40;
pub(crate) const ACC_RANGE: u16 = 0x41;
pub(crate) const ACC_PWR_CONF: u16 = 0x7c;
pub(crate) const ACC_PWR_CTRL: u16 = 0x7d;
pub(crate) const ACC_SOFTRESET: u16 = 0x7e;
pub(crate) const GYR_CHIP_ID: u16 = 0x00;
pub(crate) const GYR_DATA: u16 = 0x02;
pub(crate) const GYR_RANGE: u16 = 0x0f;
pub(crate) const GYR_BANDWIDTH: u16 = 0x10;
pub(crate) const GYR_LPM1: u16 = 0x11;
pub(crate) const GYR_SOFTRESET: u16 = 0x14;
pub(crate) const SOFTRESET: u8 = 0xb6;

/// Waits from the datasheet's power-up sequence, in µs.
pub(crate) const ACC_RESET_US: u32 = 1_000;
pub(crate) const GYR_RESET_US: u32 = 30_000;
/// Writes in accelerometer suspend mode need 1 ms between them.
pub(crate) const ACC_SUSPEND_WRITE_US: u32 = 1_000;
pub(crate) const ACC_POWER_ON_US: u32 = 5_000;

/// Accelerometer full scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AccelRange {
    G3,
    /// The reset default.
    #[default]
    G6,
    G12,
    G24,
}

impl AccelRange {
    fn register(self) -> u8 {
        match self {
            Self::G3 => 0x00,
            Self::G6 => 0x01,
            Self::G12 => 0x02,
            Self::G24 => 0x03,
        }
    }

    /// Full scale in milli-g.
    pub fn full_scale_mg(self) -> i32 {
        match self {
            Self::G3 => 3_000,
            Self::G6 => 6_000,
            Self::G12 => 12_000,
            Self::G24 => 24_000,
        }
    }

    /// Full scale in g.
    #[cfg(feature = "float")]
    pub fn full_scale_g(self) -> f32 {
        match self {
            Self::G3 => 3.0,
            Self::G6 => 6.0,
            Self::G12 => 12.0,
            Self::G24 => 24.0,
        }
    }
}

/// Accelerometer output data rate (normal filter bandwidth).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AccelRate {
    Hz12_5,
    Hz25,
    Hz50,
    /// The reset default.
    #[default]
    Hz100,
    Hz200,
    Hz400,
    Hz800,
    Hz1600,
}

impl AccelRate {
    /// `ACC_CONF`: normal bandwidth (0xA) in bits 7-4, rate code in bits 3-0.
    fn register(self) -> u8 {
        0xa0 | match self {
            Self::Hz12_5 => 0x05,
            Self::Hz25 => 0x06,
            Self::Hz50 => 0x07,
            Self::Hz100 => 0x08,
            Self::Hz200 => 0x09,
            Self::Hz400 => 0x0a,
            Self::Hz800 => 0x0b,
            Self::Hz1600 => 0x0c,
        }
    }
}

/// Gyroscope full scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GyroRange {
    /// The reset default.
    #[default]
    Dps2000,
    Dps1000,
    Dps500,
    Dps250,
    Dps125,
}

impl GyroRange {
    fn register(self) -> u8 {
        match self {
            Self::Dps2000 => 0x00,
            Self::Dps1000 => 0x01,
            Self::Dps500 => 0x02,
            Self::Dps250 => 0x03,
            Self::Dps125 => 0x04,
        }
    }

    /// Full scale in milli-degrees per second.
    pub fn full_scale_mdps(self) -> i32 {
        match self {
            Self::Dps2000 => 2_000_000,
            Self::Dps1000 => 1_000_000,
            Self::Dps500 => 500_000,
            Self::Dps250 => 250_000,
            Self::Dps125 => 125_000,
        }
    }

    /// Full scale in degrees per second.
    #[cfg(feature = "float")]
    pub fn full_scale_dps(self) -> f32 {
        match self {
            Self::Dps2000 => 2000.0,
            Self::Dps1000 => 1000.0,
            Self::Dps500 => 500.0,
            Self::Dps250 => 250.0,
            Self::Dps125 => 125.0,
        }
    }
}

/// Gyroscope output data rate and filter bandwidth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GyroRate {
    /// The reset default.
    #[default]
    Hz2000Bw532,
    Hz2000Bw230,
    Hz1000Bw116,
    Hz400Bw47,
    Hz200Bw23,
    Hz100Bw12,
    Hz200Bw64,
    Hz100Bw32,
}

impl GyroRate {
    fn register(self) -> u8 {
        match self {
            Self::Hz2000Bw532 => 0x00,
            Self::Hz2000Bw230 => 0x01,
            Self::Hz1000Bw116 => 0x02,
            Self::Hz400Bw47 => 0x03,
            Self::Hz200Bw23 => 0x04,
            Self::Hz100Bw12 => 0x05,
            Self::Hz200Bw64 => 0x06,
            Self::Hz100Bw32 => 0x07,
        }
    }
}

/// Measurement settings applied by `init`. The default matches the chip's
/// reset values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Config {
    pub accel_range: AccelRange,
    pub accel_rate: AccelRate,
    pub gyro_range: GyroRange,
    pub gyro_rate: GyroRate,
}

impl Config {
    pub(crate) fn accel_writes(self) -> [RegWrite; 2] {
        [
            RegWrite::byte(ACC_RANGE, self.accel_range.register()),
            RegWrite::byte(ACC_CONF, self.accel_rate.register()),
        ]
    }

    pub(crate) fn gyro_writes(self) -> [RegWrite; 3] {
        [
            RegWrite::byte(GYR_LPM1, 0x00),
            RegWrite::byte(GYR_RANGE, self.gyro_range.register()),
            RegWrite::byte(GYR_BANDWIDTH, self.gyro_rate.register()),
        ]
    }

    /// Converts raw counts with this configuration's ranges, in integers
    /// (rounded toward negative infinity).
    pub fn fixed(self, accel: [i16; 3], gyro: [i16; 3]) -> ImuFixed {
        let mg = self.accel_range.full_scale_mg();
        let mdps = i64::from(self.gyro_range.full_scale_mdps());
        ImuFixed {
            accel_mg: accel.map(|v| (i32::from(v) * mg) >> 15),
            gyro_mdps: gyro.map(|v| ((i64::from(v) * mdps) >> 15) as i32),
            accel_raw: accel,
            gyro_raw: gyro,
        }
    }

    /// Converts raw counts to the device-model channel counts: acceleration in
    /// mm/s² and angular rate in µrad/s, so a channel's value is in m/s² and
    /// rad/s (see [`INFO`]).
    pub fn channels(self, accel: [i16; 3], gyro: [i16; 3]) -> [i32; 6] {
        // Full scale in mm/s² (g = 9.80665 m/s²) × 2^8 and in µrad/s × 2^5:
        // a count converts with one 32×32→64-bit multiply and a shift, and no
        // 64-bit division (a library call on 32-bit MCUs).
        let a: i32 = match self.accel_range {
            AccelRange::G3 => 7_531_507,
            AccelRange::G6 => 15_063_014,
            AccelRange::G12 => 30_126_029,
            AccelRange::G24 => 60_252_058,
        };
        let g: i32 = match self.gyro_range {
            GyroRange::Dps2000 => 1_117_010_721,
            GyroRange::Dps1000 => 558_505_361,
            GyroRange::Dps500 => 279_252_680,
            GyroRange::Dps250 => 139_626_340,
            GyroRange::Dps125 => 69_813_170,
        };
        let scale = |v: i16, m: i32, shift: u32| ((i64::from(v) * i64::from(m)) >> shift) as i32;
        [
            scale(accel[0], a, 23),
            scale(accel[1], a, 23),
            scale(accel[2], a, 23),
            scale(gyro[0], g, 20),
            scale(gyro[1], g, 20),
            scale(gyro[2], g, 20),
        ]
    }

    /// Converts raw counts with this configuration's ranges.
    #[cfg(feature = "float")]
    pub fn sample(self, accel: [i16; 3], gyro: [i16; 3]) -> ImuSample {
        let g = self.accel_range.full_scale_g() / 32768.0 * STANDARD_GRAVITY;
        let rad = self.gyro_range.full_scale_dps() / 32768.0 * PI / 180.0;
        ImuSample {
            accel_mps2: accel.map(|v| f32::from(v) * g),
            gyro_radps: gyro.map(|v| f32::from(v) * rad),
            accel_raw: accel,
            gyro_raw: gyro,
        }
    }
}

/// One accelerometer + gyroscope reading in integer units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImuFixed {
    /// X, Y, Z acceleration in milli-g.
    pub accel_mg: [i32; 3],
    /// X, Y, Z angular rate in milli-degrees per second.
    pub gyro_mdps: [i32; 3],
    pub accel_raw: [i16; 3],
    pub gyro_raw: [i16; 3],
}

/// One accelerometer + gyroscope reading.
#[cfg(feature = "float")]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ImuSample {
    /// X, Y, Z acceleration in m/s².
    pub accel_mps2: [f32; 3],
    /// X, Y, Z angular rate in rad/s.
    pub gyro_radps: [f32; 3],
    pub accel_raw: [i16; 3],
    pub gyro_raw: [i16; 3],
}

pub(crate) fn decode_axes(bytes: [u8; 6]) -> [i16; 3] {
    [
        i16::from_le_bytes([bytes[0], bytes[1]]),
        i16::from_le_bytes([bytes[2], bytes[3]]),
        i16::from_le_bytes([bytes[4], bytes[5]]),
    ]
}

/// Decodes `TEMP_MSB`, `TEMP_LSB` to milli-degrees Celsius: an 11-bit two's
/// complement value in 0.125 °C steps, offset by 23 °C.
pub fn decode_temperature_mc(msb: u8, lsb: u8) -> i32 {
    let raw = (u16::from(msb) << 3) | u16::from(lsb >> 5);
    let raw = if raw > 1023 {
        i32::from(raw) - 2048
    } else {
        i32::from(raw)
    };
    raw * 125 + 23_000
}

/// Decodes `TEMP_MSB`, `TEMP_LSB` to degrees Celsius.
#[cfg(feature = "float")]
pub fn decode_temperature(msb: u8, lsb: u8) -> f32 {
    decode_temperature_mc(msb, lsb) as f32 / 1000.0
}

/// The error of a BMI088 operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error<E> {
    /// The register transfer failed.
    Register(RegisterError<E>),
    /// A chip ID did not match ([`ACCEL_CHIP_ID`], [`GYRO_CHIP_ID`]).
    WrongChip { accel: u8, gyro: u8 },
    /// A reading was requested before `init` set the ranges.
    NotInitialized,
    /// A transfer of an `init` step failed (`step` names it, such as
    /// `"accel power control"`), so a host can say which one. Feature
    /// `reasons` only.
    #[cfg(feature = "reasons")]
    Step {
        step: &'static str,
        error: RegisterError<E>,
    },
}

/// Tags a register error with the `init` step it happened in (`reasons`), or
/// just wraps it.
#[cfg(feature = "reasons")]
pub(crate) fn at<E>(step: &'static str) -> impl FnOnce(RegisterError<E>) -> Error<E> {
    move |error| Error::Step { step, error }
}

/// Tags a register error with the `init` step it happened in (`reasons`), or
/// just wraps it.
#[cfg(not(feature = "reasons"))]
pub(crate) fn at<E>(_step: &'static str) -> impl FnOnce(RegisterError<E>) -> Error<E> {
    Error::Register
}

impl<E> From<RegisterError<E>> for Error<E> {
    fn from(error: RegisterError<E>) -> Self {
        Self::Register(error)
    }
}

impl<E: fmt::Debug> HalError for Error<E> {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Register(error) => error.kind(),
            Self::WrongChip { .. } => ErrorKind::Unsupported,
            Self::NotInitialized => ErrorKind::Unavailable,
            #[cfg(feature = "reasons")]
            Self::Step { error, .. } => error.kind(),
        }
    }

    fn describe(&self, f: &mut dyn fmt::Write) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl<E: fmt::Debug> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Register(error) => write!(f, "BMI088: {error}"),
            Self::WrongChip { accel, gyro } => write!(
                f,
                "expected BMI088 chip IDs 0x{ACCEL_CHIP_ID:02x}/0x{GYRO_CHIP_ID:02x}, found 0x{accel:02x}/0x{gyro:02x}"
            ),
            Self::NotInitialized => f.write_str("BMI088 read before init"),
            #[cfg(feature = "reasons")]
            Self::Step { step, error } => write!(f, "BMI088 {step}: {error}"),
        }
    }
}

impl<E: fmt::Debug> core::error::Error for Error<E> {}

/// A BMI088 over a blocking I2C bus.
#[derive(Debug)]
pub struct Bmi088<I2C> {
    i2c: I2C,
    accel_address: u8,
    gyro_address: u8,
    config: Option<Config>,
    settings: Config,
    /// Batch reads come from the FIFOs (see [`with_fifo`](Self::with_fifo)).
    fifo: bool,
}

impl<I2C: I2c> Bmi088<I2C> {
    /// A BMI088 at [`ACCEL_ADDRESS`] and [`GYRO_ADDRESS`].
    pub fn new(i2c: I2C) -> Self {
        Self::with_addresses(i2c, ACCEL_ADDRESS, GYRO_ADDRESS)
    }

    /// A BMI088 at the given 7-bit addresses.
    pub fn with_addresses(i2c: I2C, accel_address: u8, gyro_address: u8) -> Self {
        Self {
            i2c,
            accel_address,
            gyro_address,
            config: None,
            settings: Config::default(),
            fifo: false,
        }
    }

    /// Enables the accelerometer and gyroscope FIFOs at `init`, so
    /// [`read_batch`](Self::read_batch) returns every sample since the last
    /// read rather than the latest one.
    pub fn with_fifo(mut self) -> Self {
        self.fifo = true;
        self
    }

    /// Whether the FIFOs are enabled.
    pub fn fifo(&self) -> bool {
        self.fifo
    }

    /// Sets the configuration `lemnos_device::Device::init` applies (the
    /// default matches the chip's reset values).
    pub fn with_config(mut self, config: Config) -> Self {
        self.settings = config;
        self
    }

    /// A BMI088 that an earlier [`init`](Self::init) already configured with
    /// `config`, for example after [`release`](Self::release) or when a
    /// runtime adapter wraps a borrowed bus per operation. Touches no
    /// registers.
    pub fn resume(i2c: I2C, accel_address: u8, gyro_address: u8, config: Config) -> Self {
        Self {
            config: Some(config),
            settings: config,
            ..Self::with_addresses(i2c, accel_address, gyro_address)
        }
    }

    /// The configuration `init` applied.
    pub fn config(&self) -> Option<Config> {
        self.config
    }

    fn accel(&mut self) -> I2cRegisters<&mut I2C> {
        I2cRegisters::new(&mut self.i2c, self.accel_address, AddressWidth::Bits8)
    }

    fn gyro(&mut self) -> I2cRegisters<&mut I2C> {
        I2cRegisters::new(&mut self.i2c, self.gyro_address, AddressWidth::Bits8)
    }

    /// Reads the accelerometer and gyroscope chip IDs.
    pub fn chip_ids(&mut self) -> Result<(u8, u8), Error<I2C::Error>> {
        Ok((
            self.accel().read8(ACC_CHIP_ID)?,
            self.gyro().read8(GYR_CHIP_ID)?,
        ))
    }

    /// Verifies both chips, soft-resets them, powers the accelerometer on and
    /// applies `config`. Takes about 37 ms of `delay`.
    ///
    /// A die resets as it receives the soft-reset byte, so the master may
    /// see that byte unacknowledged (a bit-banged `i2c-gpio` bus reports
    /// `EIO`). The soft-reset writes' results are ignored; the chip IDs are
    /// read again after the resets to confirm both dies came back.
    pub fn init(
        &mut self,
        delay: &mut impl DelayNs,
        config: Config,
    ) -> Result<(), Error<I2C::Error>> {
        self.check_ids("accel chip id", "gyro chip id")?;
        let _ = self.accel().write8(ACC_SOFTRESET, SOFTRESET);
        delay.delay_us(ACC_RESET_US);
        let _ = self.gyro().write8(GYR_SOFTRESET, SOFTRESET);
        delay.delay_us(GYR_RESET_US);
        self.check_ids("accel chip id after reset", "gyro chip id after reset")?;

        // Active mode, then accelerometer on; both writes happen in suspend mode.
        self.accel()
            .write8(ACC_PWR_CONF, 0x00)
            .map_err(at("accel power config"))?;
        delay.delay_us(ACC_SUSPEND_WRITE_US);
        self.accel()
            .write8(ACC_PWR_CTRL, 0x04)
            .map_err(at("accel power control"))?;
        delay.delay_us(ACC_POWER_ON_US);

        // One register per transfer: `write_sequence` would link the burst packer.
        for w in config.accel_writes() {
            self.accel()
                .write(w.address, w.bytes, w.value)
                .map_err(at("accel config"))?;
        }
        for w in config.gyro_writes() {
            self.gyro()
                .write(w.address, w.bytes, w.value)
                .map_err(at("gyro config"))?;
        }
        if self.fifo {
            self.setup_fifo()?;
        }
        self.config = Some(config);
        self.settings = config;
        Ok(())
    }

    fn check_ids(
        &mut self,
        accel_step: &'static str,
        gyro_step: &'static str,
    ) -> Result<(), Error<I2C::Error>> {
        let accel = self.accel().read8(ACC_CHIP_ID).map_err(at(accel_step))?;
        let gyro = self.gyro().read8(GYR_CHIP_ID).map_err(at(gyro_step))?;
        if accel != ACCEL_CHIP_ID || gyro != GYRO_CHIP_ID {
            return Err(Error::WrongChip { accel, gyro });
        }
        Ok(())
    }

    /// Starts both FIFOs in STREAM mode: every frame the sensors take is kept
    /// until read, and the newest are kept when a FIFO fills. The accelerometer
    /// stores its data at the configured output rate (no down-sampling).
    fn setup_fifo(&mut self) -> Result<(), Error<I2C::Error>> {
        self.accel()
            .write8(fifo::ACC_FIFO_DOWNS, fifo::ACC_DOWNS_NONE)
            .map_err(at("accel fifo"))?;
        self.accel()
            .write8(fifo::ACC_FIFO_CONFIG_0, fifo::ACC_CONFIG_0_STREAM)
            .map_err(at("accel fifo"))?;
        self.accel()
            .write8(fifo::ACC_FIFO_CONFIG_1, fifo::ACC_CONFIG_1_ACC)
            .map_err(at("accel fifo"))?;
        self.gyro()
            .write8(fifo::GYR_FIFO_CONFIG_0, 0x00)
            .map_err(at("gyro fifo"))?;
        self.gyro()
            .write8(fifo::GYR_FIFO_CONFIG_1, fifo::GYR_CONFIG_1_STREAM)
            .map_err(at("gyro fifo"))?;
        Ok(())
    }

    /// Reads the accelerometer FIFO into `out`, oldest first, at most
    /// [`MAX_SAMPLES`] (and `out.len()`). Returns the samples and how many
    /// frames the FIFO dropped on overflow since the last read.
    pub fn read_accel_fifo(
        &mut self,
        out: &mut [[i16; 3]],
    ) -> Result<(usize, u32), Error<I2C::Error>> {
        let cap = out.len().min(MAX_SAMPLES);
        let mut length = [0u8; 2];
        self.accel()
            .read_burst(fifo::ACC_FIFO_LENGTH_0, &mut length)?;
        // The byte count is 14 bits; an empty FIFO reads 0x8000.
        let bytes = usize::from(length[0]) | (usize::from(length[1] & 0x3f) << 8);
        // Never more frames than `out` takes, so a frame is not read and lost.
        let n = bytes.min(cap * fifo::ACC_FRAME_LEN);
        if n == 0 {
            return Ok((0, 0));
        }
        let mut buf = [0u8; MAX_SAMPLES * fifo::ACC_FRAME_LEN];
        self.accel()
            .read_burst(fifo::ACC_FIFO_DATA, &mut buf[..n])?;
        let parsed = fifo::parse_accel(&buf[..n], &mut out[..cap]);
        Ok((parsed.samples, parsed.skipped))
    }

    /// Reads the gyroscope FIFO into `out`, oldest first, at most
    /// [`MAX_SAMPLES`] (and `out.len()`); returns how many were read.
    pub fn read_gyro_fifo(&mut self, out: &mut [[i16; 3]]) -> Result<usize, Error<I2C::Error>> {
        let cap = out.len().min(MAX_SAMPLES);
        let status = self.gyro().read8(fifo::GYR_FIFO_STATUS)?;
        let frames = usize::from(status & 0x7f).min(cap);
        if frames == 0 {
            return Ok(0);
        }
        let mut buf = [0u8; MAX_SAMPLES * fifo::GYR_FRAME_LEN];
        let n = frames * fifo::GYR_FRAME_LEN;
        self.gyro().read_burst(fifo::GYR_FIFO_DATA, &mut buf[..n])?;
        for (sample, frame) in out
            .iter_mut()
            .zip(buf[..n].chunks_exact(fifo::GYR_FRAME_LEN))
        {
            *sample = fifo::axes(frame);
        }
        Ok(frames)
    }

    /// Raw accelerometer counts.
    pub fn read_accel_raw(&mut self) -> Result<[i16; 3], Error<I2C::Error>> {
        let mut bytes = [0u8; 6];
        self.accel().read_burst(ACC_DATA, &mut bytes)?;
        Ok(decode_axes(bytes))
    }

    /// Raw gyroscope counts.
    pub fn read_gyro_raw(&mut self) -> Result<[i16; 3], Error<I2C::Error>> {
        let mut bytes = [0u8; 6];
        self.gyro().read_burst(GYR_DATA, &mut bytes)?;
        Ok(decode_axes(bytes))
    }

    /// Reads both dies, converted to milli-g and milli-degrees per second.
    pub fn read_fixed(&mut self) -> Result<ImuFixed, Error<I2C::Error>> {
        let config = self.config.ok_or(Error::NotInitialized)?;
        Ok(config.fixed(self.read_accel_raw()?, self.read_gyro_raw()?))
    }

    /// Reads both dies and converts to SI units.
    #[cfg(feature = "float")]
    pub fn read(&mut self) -> Result<ImuSample, Error<I2C::Error>> {
        let config = self.config.ok_or(Error::NotInitialized)?;
        Ok(config.sample(self.read_accel_raw()?, self.read_gyro_raw()?))
    }

    /// The accelerometer die temperature in milli-degrees Celsius (updated
    /// every 1.28 s).
    pub fn temperature_mc(&mut self) -> Result<i32, Error<I2C::Error>> {
        let mut bytes = [0u8; 2];
        self.accel().read_burst(ACC_TEMP, &mut bytes)?;
        Ok(decode_temperature_mc(bytes[0], bytes[1]))
    }

    /// The accelerometer die temperature in °C.
    #[cfg(feature = "float")]
    pub fn temperature_c(&mut self) -> Result<f32, Error<I2C::Error>> {
        Ok(self.temperature_mc()? as f32 / 1000.0)
    }

    /// Gives the bus back.
    pub fn release(self) -> I2C {
        self.i2c
    }
}

impl AccelRate {
    /// The time between two samples, in microseconds.
    pub fn period_us(self) -> u32 {
        match self {
            Self::Hz12_5 => 80_000,
            Self::Hz25 => 40_000,
            Self::Hz50 => 20_000,
            Self::Hz100 => 10_000,
            Self::Hz200 => 5_000,
            Self::Hz400 => 2_500,
            Self::Hz800 => 1_250,
            Self::Hz1600 => 625,
        }
    }
}
