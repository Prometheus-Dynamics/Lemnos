//! The BMI088's registers, settings and FIFO frames, and the conversion of
//! counts to SI units. The chip is two dies on one bus, the accelerometer and
//! the gyroscope, each at its own 7-bit address. This module holds no bus and no
//! power state: `lib.rs` owns both and lends the bus to each operation.

use core::f32::consts::PI;
use lemnos_core::{I2c, Status, Vector3, status_of};
use lemnos_hal::register::{AddressWidth, I2cRegisters, RegisterBus, RegisterError};
use lemnos_hal::{ErrorKind, HalError};

/// Standard gravity, for converting g to m/s².
const STANDARD_GRAVITY: f32 = 9.806_65;

/// The value of each die's chip ID register.
const ACCEL_CHIP_ID: u8 = 0x1e;
const GYRO_CHIP_ID: u8 = 0x0f;
/// Written to a die's soft-reset register.
const SOFT_RESET: u8 = 0xb6;

const ACC_CHIP_ID: u16 = 0x00;
const ACC_DATA: u16 = 0x12;
const ACC_FIFO_LENGTH_0: u16 = 0x24;
const ACC_FIFO_DATA: u16 = 0x26;
const ACC_CONF: u16 = 0x40;
const ACC_RANGE: u16 = 0x41;
const ACC_FIFO_DOWNS: u16 = 0x45;
const ACC_FIFO_CONFIG_0: u16 = 0x48;
const ACC_FIFO_CONFIG_1: u16 = 0x49;
const ACC_PWR_CONF: u16 = 0x7c;
const ACC_PWR_CTRL: u16 = 0x7d;
const ACC_SOFTRESET: u16 = 0x7e;

const GYR_CHIP_ID: u16 = 0x00;
const GYR_DATA: u16 = 0x02;
const GYR_FIFO_STATUS: u16 = 0x0e;
const GYR_RANGE: u16 = 0x0f;
const GYR_BANDWIDTH: u16 = 0x10;
const GYR_LPM1: u16 = 0x11;
const GYR_SOFTRESET: u16 = 0x14;
const GYR_FIFO_CONFIG_0: u16 = 0x3d;
const GYR_FIFO_CONFIG_1: u16 = 0x3e;
const GYR_FIFO_DATA: u16 = 0x3f;

/// Gyroscope FIFO capacity, in frames: a fill this high has overflowed.
pub(crate) const GYRO_FIFO_FRAMES: usize = 100;

/// The accelerometer FIFO's capacity, bytes. A length the chip reports above it is clamped.
pub(crate) const ACC_FIFO_BYTES: usize = 1024;
/// The accelerometer FIFO's capacity, in data frames.
pub(crate) const ACC_FIFO_FRAMES: usize = ACC_FIFO_BYTES / ACCEL_FRAME;

const ACCEL_FRAME: usize = 7;
const GYRO_FRAME: usize = 6;
/// The accelerometer frame header: the top six bits are the signature, the
/// bottom two are the INT tags, which are ignored.
const HEADER_MASK: u8 = 0xfc;
/// Data frame: header, then X, Y, Z.
const HEADER_DATA: u8 = 0x84;
/// Skip frame (the FIFO overflowed): header, then the number of frames lost.
const HEADER_SKIP: u8 = 0x40;
/// Sensor time frame, written when a read finds the FIFO empty: header, then 3
/// bytes. Not a sample.
const HEADER_TIME: u8 = 0x44;
/// Configuration change: header, then one byte. Not a sample.
const HEADER_CONFIG: u8 = 0x48;
/// Sample drop: header, then one byte. Not a sample.
const HEADER_DROP: u8 = 0x50;

/// Bytes of accelerometer FIFO read per burst. A frame can straddle two bursts:
/// the bytes of a frame cut short are carried into the next burst.
const ACCEL_CHUNK: usize = 8 * ACCEL_FRAME;
/// Gyroscope frames read per burst.
const GYRO_CHUNK_FRAMES: usize = 8;

/// Which die.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Accel,
    Gyro,
}

impl Kind {
    fn id_register(self) -> u16 {
        match self {
            Self::Accel => ACC_CHIP_ID,
            Self::Gyro => GYR_CHIP_ID,
        }
    }

    fn expected_id(self) -> u8 {
        match self {
            Self::Accel => ACCEL_CHIP_ID,
            Self::Gyro => GYRO_CHIP_ID,
        }
    }

    fn reset_register(self) -> u16 {
        match self {
            Self::Accel => ACC_SOFTRESET,
            Self::Gyro => GYR_SOFTRESET,
        }
    }

    fn data_register(self) -> u16 {
        match self {
            Self::Accel => ACC_DATA,
            Self::Gyro => GYR_DATA,
        }
    }
}

/// A die's range and rate as register values, the scale of one count, and the
/// nominal sample period.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Setting {
    range: u8,
    rate: u8,
    /// SI units per count: m/s² for the accelerometer, rad/s for the gyroscope.
    pub(crate) scale: f32,
    /// The nominal time between samples, ns.
    pub(crate) period_ns: u64,
}

/// The accelerometer's full scale (`range_g`: 3, 6, 12 or 24) and output data
/// rate (`rate_hz`: 12.5 to 1600). `None` for any other value.
pub(crate) fn accel_setting(range_g: i64, rate_hz: f64) -> Option<Setting> {
    let (range, full_scale_g) = match range_g {
        3 => (0x00, 3.0),
        6 => (0x01, 6.0),
        12 => (0x02, 12.0),
        24 => (0x03, 24.0),
        _ => return None,
    };
    // Tenths of a hertz, so 12.5 is exact.
    let tenths = (rate_hz * 10.0 + 0.5) as i64;
    let rate = match tenths {
        125 => 0x05,
        250 => 0x06,
        500 => 0x07,
        1_000 => 0x08,
        2_000 => 0x09,
        4_000 => 0x0a,
        8_000 => 0x0b,
        16_000 => 0x0c,
        _ => return None,
    };
    Some(Setting {
        // Normal bandwidth (0xA) in bits 7-4, the rate code in bits 3-0.
        range,
        rate: 0xa0 | rate,
        scale: full_scale_g * STANDARD_GRAVITY / 32_768.0,
        // 10 s in tenths of a hertz: exact for every rate above.
        period_ns: 10_000_000_000 / tenths as u64,
    })
}

/// The gyroscope's full scale (`range_dps`: 125, 250, 500, 1000 or 2000) and
/// output data rate (`rate_hz`: 100, 200, 400, 1000 or 2000), each with the
/// filter bandwidth that goes with it. `None` for any other value.
pub(crate) fn gyro_setting(range_dps: i64, rate_hz: i64) -> Option<Setting> {
    let (range, full_scale_dps) = match range_dps {
        125 => (0x04, 125.0),
        250 => (0x03, 250.0),
        500 => (0x02, 500.0),
        1_000 => (0x01, 1_000.0),
        2_000 => (0x00, 2_000.0),
        _ => return None,
    };
    let rate = match rate_hz {
        100 => 0x05,
        200 => 0x04,
        400 => 0x03,
        1_000 => 0x02,
        2_000 => 0x00,
        _ => return None,
    };
    Some(Setting {
        range,
        rate,
        scale: full_scale_dps / 32_768.0 * PI / 180.0,
        period_ns: 1_000_000_000 / rate_hz as u64,
    })
}

/// Why a die operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The chip ID is not this die's.
    WrongChip,
    /// A transfer failed, or a verified write read back a different value.
    Register(RegisterError<ErrorKind>),
}

impl Failure {
    pub(crate) fn status(&self) -> Status {
        match self {
            Self::WrongChip => Status::NOT_SUPPORTED,
            Self::Register(error) => status_of(error.kind()),
        }
    }
}

impl From<RegisterError<ErrorKind>> for Failure {
    fn from(error: RegisterError<ErrorKind>) -> Self {
        Self::Register(error)
    }
}

pub(crate) type Step<T> = Result<T, Failure>;

fn bus_regs(bus: &mut I2c, address: u8) -> I2cRegisters<&mut I2c> {
    I2cRegisters::new(bus, address, AddressWidth::Bits8)
}

/// Verifies the chip ID of `kind` at `address`.
pub(crate) fn check_id(bus: &mut I2c, address: u8, kind: Kind) -> Step<()> {
    let found = bus_regs(bus, address).read8(kind.id_register())?;
    if found == kind.expected_id() {
        Ok(())
    } else {
        Err(Failure::WrongChip)
    }
}

/// Soft-resets a die. The die resets as it takes the byte, so it may not
/// acknowledge it: the result is ignored, and the chip ID is checked after.
pub(crate) fn soft_reset(bus: &mut I2c, address: u8, kind: Kind) {
    let _ = bus_regs(bus, address).write8(kind.reset_register(), SOFT_RESET);
}

/// Takes the accelerometer out of its power-on suspend mode (active mode).
pub(crate) fn accel_power_config(bus: &mut I2c, address: u8) -> Step<()> {
    bus_regs(bus, address).write8(ACC_PWR_CONF, 0x00)?;
    Ok(())
}

/// Turns the accelerometer on.
pub(crate) fn accel_power_control(bus: &mut I2c, address: u8) -> Step<()> {
    bus_regs(bus, address).write8(ACC_PWR_CTRL, 0x04)?;
    Ok(())
}

/// Writes `value` to `address` and reads it back, comparing the bits in `mask`.
/// Other bits are the chip's to define (reserved, or read-only status), so they
/// are not compared. A mismatch means the chip did not take the write yet.
pub(crate) fn write_checked<B: RegisterBus<BusError = ErrorKind>>(
    regs: &mut B,
    address: u16,
    value: u8,
    mask: u8,
) -> Step<()> {
    regs.write8(address, value)?;
    let read = regs.read8(address)?;
    if read & mask == value & mask {
        Ok(())
    } else {
        Err(Failure::Register(RegisterError::Mismatch {
            address,
            wrote: u32::from(value),
            read: u32::from(read),
        }))
    }
}

/// Writes a die's range and rate, each read back, and, with `fifo`, starts its
/// FIFO in stream mode: every frame is kept until read, and the newest are kept
/// when the FIFO fills. The accelerometer keeps every sample at its output rate
/// (no down-sampling). A readback mismatch means the chip is not ready to take
/// settings yet, so the step is retried.
pub(crate) fn configure(
    bus: &mut I2c,
    address: u8,
    kind: Kind,
    setting: Setting,
    fifo: bool,
) -> Step<()> {
    let mut regs = bus_regs(bus, address);
    match kind {
        Kind::Accel => {
            // Range is bits 1-0; the rate code is bits 3-0 (bits 7-4 are the
            // bandwidth and reserved bits).
            write_checked(&mut regs, ACC_RANGE, setting.range, 0x03)?;
            write_checked(&mut regs, ACC_CONF, setting.rate, 0x0f)?;
            if fifo {
                regs.write8(ACC_FIFO_DOWNS, 0x80)?;
                regs.write8(ACC_FIFO_CONFIG_0, 0x02)?;
                regs.write8(ACC_FIFO_CONFIG_1, 0x50)?;
            }
        }
        Kind::Gyro => {
            regs.write8(GYR_LPM1, 0x00)?;
            write_checked(&mut regs, GYR_RANGE, setting.range, 0x07)?;
            write_checked(&mut regs, GYR_BANDWIDTH, setting.rate, 0x07)?;
            if fifo {
                regs.write8(GYR_FIFO_CONFIG_0, 0x00)?;
                regs.write8(GYR_FIFO_CONFIG_1, 0x80)?;
            }
        }
    }
    Ok(())
}

/// The die's latest sample, read straight from its data registers.
pub(crate) fn read_axes(bus: &mut I2c, address: u8, kind: Kind) -> Step<[i16; 3]> {
    let mut bytes = [0u8; 6];
    bus_regs(bus, address).read_burst(kind.data_register(), &mut bytes)?;
    Ok(axes(&bytes))
}

/// The accelerometer FIFO's fill, in bytes, at most the FIFO's capacity. The
/// count is 14 bits and an empty FIFO reads 0x8000; a count the FIFO cannot
/// hold is clamped to what it can.
pub(crate) fn accel_fifo_bytes(bus: &mut I2c, address: u8) -> Step<usize> {
    let mut length = [0u8; 2];
    bus_regs(bus, address).read_burst(ACC_FIFO_LENGTH_0, &mut length)?;
    let bytes = usize::from(length[0]) | (usize::from(length[1] & 0x3f) << 8);
    Ok(bytes.min(ACC_FIFO_BYTES))
}

/// One thing the accelerometer FIFO held, oldest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Frame {
    /// A data frame: one sample.
    Sample([i16; 3]),
    /// A skip frame: the FIFO overflowed and this many frames were dropped
    /// before the frames after it.
    Skipped(u32),
}

/// Reads all `bytes` of the accelerometer FIFO, oldest first, handing each
/// frame to `each`. A frame that straddles two bursts is carried into the next.
pub(crate) fn drain_accel(
    bus: &mut I2c,
    address: u8,
    bytes: usize,
    each: &mut impl FnMut(Frame),
) -> Step<()> {
    // Up to one burst plus the carried bytes of a frame cut short (at most 6).
    let mut buf = [0u8; ACCEL_CHUNK + ACCEL_FRAME];
    let mut carried = 0;
    let mut left = bytes;
    while left > 0 {
        let n = left.min(ACCEL_CHUNK);
        let end = carried + n;
        bus_regs(bus, address).read_burst(ACC_FIFO_DATA, &mut buf[carried..end])?;
        let used = scan_accel(&buf[..end], each);
        buf.copy_within(used..end, 0);
        carried = end - used;
        left -= n;
    }
    Ok(())
}

/// Walks accelerometer FIFO bytes, oldest first, handing data frames and skip
/// frames to `each`; time, configuration and drop frames are stepped over.
/// Returns the bytes used; a frame cut short at the end is left for the caller
/// to carry.
pub(crate) fn scan_accel(bytes: &[u8], each: &mut impl FnMut(Frame)) -> usize {
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] & HEADER_MASK {
            HEADER_DATA => {
                let end = i + ACCEL_FRAME;
                if end > bytes.len() {
                    break;
                }
                each(Frame::Sample(axes(&bytes[i + 1..end])));
                i = end;
            }
            HEADER_SKIP => {
                let Some(&count) = bytes.get(i + 1) else {
                    break;
                };
                each(Frame::Skipped(u32::from(count)));
                i += 2;
            }
            HEADER_TIME => {
                if i + 4 > bytes.len() {
                    break;
                }
                i += 4;
            }
            HEADER_CONFIG | HEADER_DROP => {
                if i + 2 > bytes.len() {
                    break;
                }
                i += 2;
            }
            // Not a frame header: step over it.
            _ => i += 1,
        }
    }
    i
}

/// The gyroscope FIFO's fill, in frames, at most the FIFO's capacity (the
/// register's top bit is not a count).
pub(crate) fn gyro_fifo_frames(bus: &mut I2c, address: u8) -> Step<usize> {
    let status = bus_regs(bus, address).read8(GYR_FIFO_STATUS)?;
    Ok(usize::from(status & 0x7f).min(GYRO_FIFO_FRAMES))
}

/// Reads `frames` gyroscope frames, oldest first, handing each to `each`.
pub(crate) fn drain_gyro(
    bus: &mut I2c,
    address: u8,
    frames: usize,
    each: &mut impl FnMut([i16; 3]),
) -> Step<()> {
    let mut buf = [0u8; GYRO_CHUNK_FRAMES * GYRO_FRAME];
    let mut left = frames;
    while left > 0 {
        let n = left.min(GYRO_CHUNK_FRAMES);
        let bytes = n * GYRO_FRAME;
        bus_regs(bus, address).read_burst(GYR_FIFO_DATA, &mut buf[..bytes])?;
        let (frames, _) = buf[..bytes].as_chunks::<GYRO_FRAME>();
        for frame in frames {
            each(axes(frame));
        }
        left -= n;
    }
    Ok(())
}

/// Three little-endian 16-bit counts from six bytes.
fn axes(bytes: &[u8]) -> [i16; 3] {
    let le = |at: usize| i16::from_le_bytes([bytes[at], bytes[at + 1]]);
    [le(0), le(2), le(4)]
}

/// Counts times SI units per count.
pub(crate) fn to_vector(raw: [i16; 3], scale: f32) -> Vector3 {
    Vector3 {
        x: f32::from(raw[0]) * scale,
        y: f32::from(raw[1]) * scale,
        z: f32::from(raw[2]) * scale,
    }
}
