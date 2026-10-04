//! Bosch BMM150 geomagnetic sensor over embedded-hal I2C, without `std` or
//! allocation.
//!
//! [`Bmm150`] powers the chip up, verifies its ID, reads its factory trim
//! registers, sets the repetition [`Preset`] and [`DataRate`], and returns
//! [`MagneticField`] readings in µT using Bosch's floating-point
//! compensation. [`asynch::Bmm150`] is the same over embedded-hal-async.

#![no_std]
#![forbid(unsafe_code)]

pub mod asynch;

#[cfg(test)]
mod tests;

use core::fmt;
use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;
use lemnos_hal::register::{AddressWidth, I2cRegisters, RegisterBus, RegisterError};
use lemnos_hal::{ErrorKind, HalError};

/// The 7-bit address with SDO and CSB2 low; 0x11-0x13 are the alternatives.
pub const DEFAULT_ADDRESS: u8 = 0x10;
/// The value of the chip ID register.
pub const CHIP_ID: u8 = 0x32;

pub(crate) const REG_CHIP_ID: u16 = 0x40;
pub(crate) const REG_DATA: u16 = 0x42;
pub(crate) const REG_POWER: u16 = 0x4b;
pub(crate) const REG_OP_MODE: u16 = 0x4c;
pub(crate) const REG_REP_XY: u16 = 0x51;
pub(crate) const REG_REP_Z: u16 = 0x52;
pub(crate) const REG_TRIM_X1: u16 = 0x5d;
pub(crate) const REG_TRIM_Z4: u16 = 0x62;
pub(crate) const REG_TRIM_Z2: u16 = 0x68;
/// Start-up time from suspend to sleep mode.
pub(crate) const POWER_ON_US: u32 = 3_000;

/// Bosch's repetition presets: more repetitions mean less noise, more
/// current, and a lower maximum data rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Preset {
    /// 3 XY / 3 Z repetitions.
    LowPower,
    /// 9 XY / 15 Z repetitions.
    #[default]
    Regular,
    /// 15 XY / 27 Z repetitions.
    Enhanced,
    /// 47 XY / 83 Z repetitions.
    HighAccuracy,
}

impl Preset {
    /// `(REP_XY, REP_Z)` register values: `nXY = 1 + 2 × REP_XY`, `nZ = 1 + REP_Z`.
    pub fn registers(self) -> (u8, u8) {
        match self {
            Self::LowPower => (0x01, 0x02),
            Self::Regular => (0x04, 0x0e),
            Self::Enhanced => (0x07, 0x1a),
            Self::HighAccuracy => (0x17, 0x52),
        }
    }
}

/// Output data rate in normal mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DataRate {
    Hz2,
    Hz6,
    Hz8,
    #[default]
    Hz10,
    Hz15,
    Hz20,
    Hz25,
    Hz30,
}

impl DataRate {
    fn bits(self) -> u8 {
        match self {
            Self::Hz10 => 0b000,
            Self::Hz2 => 0b001,
            Self::Hz6 => 0b010,
            Self::Hz8 => 0b011,
            Self::Hz15 => 0b100,
            Self::Hz20 => 0b101,
            Self::Hz25 => 0b110,
            Self::Hz30 => 0b111,
        }
    }
}

/// Measurement settings applied by `init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Config {
    pub preset: Preset,
    pub data_rate: DataRate,
}

impl Config {
    /// The op-mode register: data rate in bits 5-3, normal mode (00) in bits 2-1.
    pub(crate) fn op_mode(self) -> u8 {
        self.data_rate.bits() << 3
    }
}

/// Factory trim values, read once at `init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Trim {
    pub x1: i8,
    pub y1: i8,
    pub x2: i8,
    pub y2: i8,
    pub z1: u16,
    pub z2: i16,
    pub z3: i16,
    pub z4: i16,
    pub xy1: u8,
    pub xy2: i8,
    pub xyz1: u16,
}

impl Trim {
    /// Decodes the three trim blocks at 0x5D (2 bytes), 0x62 (4) and 0x68 (10).
    pub fn from_registers(x1y1: [u8; 2], z4x2y2: [u8; 4], rest: [u8; 10]) -> Self {
        Self {
            x1: x1y1[0] as i8,
            y1: x1y1[1] as i8,
            z4: i16::from_le_bytes([z4x2y2[0], z4x2y2[1]]),
            x2: z4x2y2[2] as i8,
            y2: z4x2y2[3] as i8,
            z2: i16::from_le_bytes([rest[0], rest[1]]),
            z1: u16::from_le_bytes([rest[2], rest[3]]),
            xyz1: u16::from_le_bytes([rest[4], rest[5] & 0x7f]),
            z3: i16::from_le_bytes([rest[6], rest[7]]),
            xy2: rest[8] as i8,
            xy1: rest[9],
        }
    }

    fn compensate_xy(&self, raw: i16, rhall: u16, a1: i8, a2: i8) -> Option<f32> {
        if raw == RawSample::XY_OVERFLOW {
            return None;
        }
        let r0 = if rhall != 0 { rhall } else { self.xyz1 };
        if r0 == 0 {
            return None;
        }
        let r = f32::from(self.xyz1) * 16384.0 / f32::from(r0) - 16384.0;
        let sensitivity =
            f32::from(self.xy2) * (r * r / 268_435_456.0) + r * f32::from(self.xy1) / 16384.0;
        let scaled = f32::from(raw) * ((sensitivity + 256.0) * (f32::from(a2) + 160.0));
        Some((scaled / 8192.0 + f32::from(a1) * 8.0) / 16.0)
    }

    fn compensate_z(&self, raw: i16, rhall: u16) -> Option<f32> {
        if raw == RawSample::Z_OVERFLOW
            || self.z1 == 0
            || self.z2 == 0
            || self.xyz1 == 0
            || rhall == 0
        {
            return None;
        }
        let offset = f32::from(raw) - f32::from(self.z4);
        let hall = f32::from(self.z3) * (f32::from(rhall) - f32::from(self.xyz1));
        let gain = f32::from(self.z2) + f32::from(self.z1) * f32::from(rhall) / 32768.0;
        Some((offset * 131_072.0 - hall) / (gain * 4.0) / 16.0)
    }

    /// Converts a raw sample to µT. An axis is `None` when it overflowed or the
    /// trim cannot compensate it.
    pub fn compensate(&self, raw: RawSample) -> MagneticField {
        MagneticField {
            x_ut: self.compensate_xy(raw.x, raw.rhall, self.x1, self.x2),
            y_ut: self.compensate_xy(raw.y, raw.rhall, self.y1, self.y2),
            z_ut: self.compensate_z(raw.z, raw.rhall),
        }
    }
}

/// The uncompensated data registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawSample {
    /// 13-bit X.
    pub x: i16,
    /// 13-bit Y.
    pub y: i16,
    /// 15-bit Z.
    pub z: i16,
    /// 14-bit Hall resistance.
    pub rhall: u16,
    /// Whether a new measurement was ready.
    pub data_ready: bool,
}

impl RawSample {
    /// The X/Y value the chip reports on overflow.
    pub const XY_OVERFLOW: i16 = -4096;
    /// The Z value the chip reports on overflow.
    pub const Z_OVERFLOW: i16 = -16384;

    /// Decodes the 8 data registers starting at 0x42.
    pub fn from_registers(bytes: [u8; 8]) -> Self {
        Self {
            x: i16::from_le_bytes([bytes[0], bytes[1]]) >> 3,
            y: i16::from_le_bytes([bytes[2], bytes[3]]) >> 3,
            z: i16::from_le_bytes([bytes[4], bytes[5]]) >> 1,
            rhall: u16::from_le_bytes([bytes[6], bytes[7]]) >> 2,
            data_ready: bytes[6] & 0x01 != 0,
        }
    }
}

/// A compensated field reading in µT.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MagneticField {
    pub x_ut: Option<f32>,
    pub y_ut: Option<f32>,
    pub z_ut: Option<f32>,
}

/// The error of a BMM150 operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error<E> {
    /// The register transfer failed.
    Register(RegisterError<E>),
    /// The chip ID register did not read [`CHIP_ID`].
    WrongChip { found: u8 },
    /// `read` was called before `init` loaded the trim.
    NotInitialized,
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
        }
    }
}

impl<E: fmt::Debug> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Register(error) => write!(f, "BMM150: {error}"),
            Self::WrongChip { found } => {
                write!(
                    f,
                    "expected BMM150 chip ID 0x{CHIP_ID:02x}, found 0x{found:02x}"
                )
            }
            Self::NotInitialized => f.write_str("BMM150 read before init"),
        }
    }
}

impl<E: fmt::Debug> core::error::Error for Error<E> {}

/// A BMM150 over a blocking I2C bus.
#[derive(Debug)]
pub struct Bmm150<I2C> {
    i2c: I2C,
    address: u8,
    trim: Option<Trim>,
}

impl<I2C: I2c> Bmm150<I2C> {
    /// A chip at 7-bit `address`; call [`init`](Self::init) next.
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

    pub fn address(&self) -> u8 {
        self.address
    }

    /// The factory trim, once [`init`](Self::init) has read it.
    pub fn trim(&self) -> Option<&Trim> {
        self.trim.as_ref()
    }

    fn registers(&mut self) -> I2cRegisters<&mut I2C> {
        I2cRegisters::new(&mut self.i2c, self.address, AddressWidth::Bits8)
    }

    /// Leaves suspend mode, verifies the chip, reads its trim and starts
    /// continuous measurement with `config`.
    pub fn init(
        &mut self,
        delay: &mut impl DelayNs,
        config: Config,
    ) -> Result<(), Error<I2C::Error>> {
        self.registers().write8(REG_POWER, 0x01)?;
        delay.delay_us(POWER_ON_US);
        let mut regs = self.registers();
        let found = regs.read8(REG_CHIP_ID)?;
        if found != CHIP_ID {
            return Err(Error::WrongChip { found });
        }
        let (mut x1y1, mut z4x2y2, mut rest) = ([0u8; 2], [0u8; 4], [0u8; 10]);
        regs.read_burst(REG_TRIM_X1, &mut x1y1)?;
        regs.read_burst(REG_TRIM_Z4, &mut z4x2y2)?;
        regs.read_burst(REG_TRIM_Z2, &mut rest)?;
        let (rep_xy, rep_z) = config.preset.registers();
        regs.write8(REG_REP_XY, rep_xy)?;
        regs.write8(REG_REP_Z, rep_z)?;
        regs.write8(REG_OP_MODE, config.op_mode())?;
        self.trim = Some(Trim::from_registers(x1y1, z4x2y2, rest));
        Ok(())
    }

    /// Reads the data registers without compensation.
    pub fn read_raw(&mut self) -> Result<RawSample, Error<I2C::Error>> {
        let mut bytes = [0u8; 8];
        self.registers().read_burst(REG_DATA, &mut bytes)?;
        Ok(RawSample::from_registers(bytes))
    }

    /// Reads and compensates the latest measurement.
    pub fn read(&mut self) -> Result<MagneticField, Error<I2C::Error>> {
        let trim = self.trim.ok_or(Error::NotInitialized)?;
        Ok(trim.compensate(self.read_raw()?))
    }

    /// Returns to suspend mode; registers reset, so `init` must run again.
    pub fn power_down(&mut self) -> Result<(), Error<I2C::Error>> {
        self.registers().write8(REG_POWER, 0x00)?;
        self.trim = None;
        Ok(())
    }

    /// Gives the bus back.
    pub fn release(self) -> I2C {
        self.i2c
    }
}
