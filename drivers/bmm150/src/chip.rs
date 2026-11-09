//! The BMM150's registers, factory trim, modes and compensation. The driver in
//! `lib.rs` owns the bus and the power state; this module is the chip alone.

use lemnos_core::{I2c, Status, Vector3, status_of};
use lemnos_hal::register::{AddressWidth, I2cRegisters, RegisterBus, RegisterError};
use lemnos_hal::{ErrorKind, HalError};

/// The value of the chip ID register.
pub const CHIP_ID: u8 = 0x32;

pub const REG_CHIP_ID: u16 = 0x40;
pub const REG_DATA: u16 = 0x42;
pub const REG_POWER: u16 = 0x4b;
pub const REG_OP_MODE: u16 = 0x4c;
pub const REG_REP_XY: u16 = 0x51;
pub const REG_REP_Z: u16 = 0x52;
pub const REG_TRIM_X1: u16 = 0x5d;
pub const REG_TRIM_Z4: u16 = 0x62;
pub const REG_TRIM_Z2: u16 = 0x68;

/// The chip's repetition presets: more repetitions mean less noise, more
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
    /// The preset named `name` in the config (`low_power`, `regular`, `enhanced`, `high_accuracy`).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "low_power" => Some(Self::LowPower),
            "regular" => Some(Self::Regular),
            "enhanced" => Some(Self::Enhanced),
            "high_accuracy" => Some(Self::HighAccuracy),
            _ => None,
        }
    }

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
    /// The rate for a whole number of hertz, if the chip has one.
    pub fn from_hz(hz: i64) -> Option<Self> {
        match hz {
            2 => Some(Self::Hz2),
            6 => Some(Self::Hz6),
            8 => Some(Self::Hz8),
            10 => Some(Self::Hz10),
            15 => Some(Self::Hz15),
            20 => Some(Self::Hz20),
            25 => Some(Self::Hz25),
            30 => Some(Self::Hz30),
            _ => None,
        }
    }

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

/// Measurement settings applied at start-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Settings {
    pub preset: Preset,
    pub data_rate: DataRate,
}

impl Settings {
    /// The op-mode register: data rate in bits 5-3, normal mode (00) in bits 2-1.
    fn op_mode(self) -> u8 {
        self.data_rate.bits() << 3
    }
}

/// Why a chip operation failed. Every fault leaves the chip to be powered up
/// again on the next read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// A register transfer failed.
    Bus(ErrorKind),
    /// The chip ID register did not read [`CHIP_ID`].
    WrongChip,
}

impl From<RegisterError<ErrorKind>> for Fault {
    fn from(error: RegisterError<ErrorKind>) -> Self {
        Self::Bus(error.kind())
    }
}

impl Fault {
    pub fn status(self) -> Status {
        match self {
            Self::Bus(kind) => status_of(kind),
            Self::WrongChip => Status::NOT_SUPPORTED,
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Self::Bus(_) => "bus error",
            Self::WrongChip => "chip ID mismatch",
        }
    }
}

/// Factory trim values, read at start-up.
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

    /// The Hall resistance X/Y compensation divides by: the sample's, or the
    /// trim reference when the sample has none.
    fn xy_reference(&self, raw: i16, rhall: u16) -> Option<u16> {
        if raw == RawSample::XY_OVERFLOW {
            return None;
        }
        let r0 = if rhall != 0 { rhall } else { self.xyz1 };
        (r0 != 0).then_some(r0)
    }

    fn z_compensable(&self, raw: i16, rhall: u16) -> bool {
        raw != RawSample::Z_OVERFLOW && self.z1 != 0 && self.z2 != 0 && self.xyz1 != 0 && rhall != 0
    }

    /// The X/Y compensation without its final `/ 16`, so the result keeps
    /// 1/16 µT resolution. Every intermediate fits `i32`.
    fn compensate_xy_fixed(&self, raw: i16, rhall: u16, a1: i8, a2: i8) -> Option<i32> {
        let r0 = self.xy_reference(raw, rhall)?;
        let r = i32::from(
            ((i32::from(self.xyz1) * 16384 / i32::from(r0)) as u16).wrapping_sub(0x4000) as i16,
        );
        let sensitivity =
            (i32::from(self.xy2) * (r * r / 128) + r * i32::from(self.xy1) * 128) / 512 + 0x10_0000;
        let gain = sensitivity * (i32::from(a2) + 0xa0) / 4096;
        Some(i32::from(raw) * gain / 8192 + i32::from(a1) * 8)
    }

    /// The Z compensation without its final `/ 16`.
    fn compensate_z_fixed(&self, raw: i16, rhall: u16) -> Option<i32> {
        if !self.z_compensable(raw, rhall) {
            return None;
        }
        let hall = i32::from(self.z3) * (i32::from(rhall) - i32::from(self.xyz1)) / 4;
        let offset = (i32::from(raw) - i32::from(self.z4)) * 32768;
        let gain =
            i32::from(self.z2) + (i32::from(self.z1) * (i32::from(rhall) * 2) + 32768) / 65536;
        (gain != 0).then(|| (offset - hall) / gain)
    }

    /// Converts a raw sample to 1/16 µT with integer arithmetic only. An axis
    /// is `None` when it overflowed or the trim cannot compensate it.
    pub fn compensate_fixed(&self, raw: RawSample) -> MagneticFieldFixed {
        MagneticFieldFixed {
            x_ut16: self.compensate_xy_fixed(raw.x, raw.rhall, self.x1, self.x2),
            y_ut16: self.compensate_xy_fixed(raw.y, raw.rhall, self.y1, self.y2),
            z_ut16: self.compensate_z_fixed(raw.z, raw.rhall),
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

/// A compensated field in 1/16 µT (divide by 16 for µT).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MagneticFieldFixed {
    pub x_ut16: Option<i32>,
    pub y_ut16: Option<i32>,
    pub z_ut16: Option<i32>,
}

impl MagneticFieldFixed {
    /// The field in tesla. An axis without a value is NaN.
    pub fn to_vector(self) -> Vector3 {
        Vector3 {
            x: tesla(self.x_ut16),
            y: tesla(self.y_ut16),
            z: tesla(self.z_ut16),
        }
    }
}

/// One 1/16 µT count in tesla is `1 / 16 × 10^-6`.
fn tesla(ut16: Option<i32>) -> f32 {
    match ut16 {
        Some(counts) => counts as f32 / 16.0e6,
        None => f32::NAN,
    }
}

/// Powers the chip up. It needs about 3 ms before its registers answer, so
/// the caller waits for its next read before calling [`start_up`].
pub fn power_on(bus: &mut I2c, address: u8) -> Result<(), Fault> {
    I2cRegisters::new(bus, address, AddressWidth::Bits8).write8(REG_POWER, 0x01)?;
    Ok(())
}

/// Checks the chip ID, reads the factory trim and starts normal mode with
/// `settings`.
pub fn start_up(bus: &mut I2c, address: u8, settings: Settings) -> Result<Trim, Fault> {
    let mut regs = I2cRegisters::new(bus, address, AddressWidth::Bits8);
    if regs.read8(REG_CHIP_ID)? != CHIP_ID {
        return Err(Fault::WrongChip);
    }
    let (mut x1y1, mut z4x2y2, mut rest) = ([0u8; 2], [0u8; 4], [0u8; 10]);
    regs.read_burst(REG_TRIM_X1, &mut x1y1)?;
    regs.read_burst(REG_TRIM_Z4, &mut z4x2y2)?;
    regs.read_burst(REG_TRIM_Z2, &mut rest)?;
    let (rep_xy, rep_z) = settings.preset.registers();
    regs.write8(REG_REP_XY, rep_xy)?;
    regs.write8(REG_REP_Z, rep_z)?;
    regs.write8(REG_OP_MODE, settings.op_mode())?;
    Ok(Trim::from_registers(x1y1, z4x2y2, rest))
}

/// Reads the data registers without compensation.
pub fn read_raw(bus: &mut I2c, address: u8) -> Result<RawSample, Fault> {
    let mut bytes = [0u8; 8];
    I2cRegisters::new(bus, address, AddressWidth::Bits8).read_burst(REG_DATA, &mut bytes)?;
    Ok(RawSample::from_registers(bytes))
}
