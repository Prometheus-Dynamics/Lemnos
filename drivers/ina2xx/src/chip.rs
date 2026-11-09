//! The INA2xx register layer: chip identity, calibration, and the conversion of
//! raw register values into integer units. It holds no bus; each operation is
//! given one.
//!
//! The calibration follows the datasheets: the current LSB is the maximum
//! current / 2^15 (rounded to whole nanoamperes), and the INA238 picks its
//! ±40.96 mV shunt range whenever maximum current × shunt fits, for four times
//! the resolution. All of it is integer arithmetic.
//!
//! Every chip is read in one I2C transaction per poll. The datasheets do not
//! auto-increment the pointer on reads, so each register is addressed
//! explicitly.

use core::fmt;
use lemnos_hal::i2c::{Error as I2cError, I2c, Operation};
use lemnos_hal::register::{
    AddressWidth, Endian, I2cRegisters, RegWrite, RegisterBus, RegisterError, decode_value,
};
use lemnos_hal::{ErrorKind, HalError};

/// The default 7-bit address (A0 and A1 tied to ground).
pub const DEFAULT_ADDRESS: u8 = 0x40;
/// The manufacturer ID every supported chip reports ("TI").
pub const MANUFACTURER_TI: u16 = 0x5449;

/// A supported chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Model {
    /// INA226: external shunt, ±81.92 mV shunt range.
    Ina226,
    /// INA238: external shunt, ±163.84 mV or ±40.96 mV shunt range, die
    /// temperature.
    Ina238,
    /// INA260: integrated 2 mΩ shunt; [`Calibration`] is ignored.
    Ina260,
}

impl Model {
    pub(crate) fn manufacturer_register(self) -> u8 {
        match self {
            Self::Ina238 => 0x3e,
            Self::Ina226 | Self::Ina260 => 0xfe,
        }
    }

    pub(crate) fn device_register(self) -> u8 {
        match self {
            Self::Ina238 => 0x3f,
            Self::Ina226 | Self::Ina260 => 0xff,
        }
    }

    /// Whether `device_id` (the chip's device or die ID register) belongs to
    /// this model. The INA238 keeps a revision in the low four bits.
    pub fn matches_device_id(self, device_id: u16) -> bool {
        match self {
            Self::Ina226 => device_id == 0x2260,
            Self::Ina238 => device_id >> 4 == 0x238,
            Self::Ina260 => device_id == 0x2270,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Ina226 => "INA226",
            Self::Ina238 => "INA238",
            Self::Ina260 => "INA260",
        }
    }
}

/// The external shunt and the largest current to measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Calibration {
    /// Shunt resistance in micro-ohms.
    pub shunt_micro_ohms: u32,
    /// Largest expected current in microamperes; sets the current resolution.
    pub max_current_micro_amps: u32,
}

impl Calibration {
    pub const fn from_micro(shunt_micro_ohms: u32, max_current_micro_amps: u32) -> Self {
        Self {
            shunt_micro_ohms,
            max_current_micro_amps,
        }
    }
}

/// A [`Calibration`] the chip cannot be calibrated for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigError(pub &'static str);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "INA2xx configuration: {}", self.0)
    }
}

impl core::error::Error for ConfigError {}

impl HalError for ConfigError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Configuration
    }
}

/// One measurement in integer units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadingFixed {
    pub bus_voltage_uv: u32,
    pub shunt_voltage_nv: i32,
    pub current_na: i64,
    pub power_nw: u64,
    /// Only the INA238 measures it.
    pub die_temperature_mc: Option<i32>,
}

/// The error of an INA2xx operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error<E> {
    /// The register transfer failed.
    Register(RegisterError<E>),
    /// The chip at the address is not the configured model.
    WrongChip {
        model: Model,
        manufacturer: u16,
        device: u16,
    },
}

impl<E> From<RegisterError<E>> for Error<E> {
    fn from(error: RegisterError<E>) -> Self {
        Self::Register(error)
    }
}

impl<E: fmt::Debug> HalError for Error<E> {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Register(error) => HalError::kind(error),
            Self::WrongChip { .. } => ErrorKind::Unsupported,
        }
    }
}

impl<E: fmt::Debug> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Register(error) => write!(f, "INA2xx: {error}"),
            Self::WrongChip {
                model,
                manufacturer,
                device,
            } => write!(
                f,
                "expected an {}, found manufacturer 0x{manufacturer:04x} device 0x{device:04x}",
                model.name()
            ),
        }
    }
}

impl<E: fmt::Debug> core::error::Error for Error<E> {}

/// Register writes and scale factors derived from a model and [`Calibration`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Setup {
    pub(crate) model: Model,
    writes: [RegWrite; 3],
    len: usize,
    current_lsb_na: u32,
    shunt_lsb_nv: u32,
}

/// INA226 config: 16-sample averaging, 1.1 ms conversions, continuous shunt and bus.
pub(crate) const INA226_CONFIG: u16 = 0x4527;
/// INA260 config: as INA226 (bits 14-12 read back as 0b110).
pub(crate) const INA260_CONFIG: u16 = 0x6527;
/// INA238 ADC config: continuous shunt, bus and temperature, 1052 µs, 16 samples.
pub(crate) const INA238_ADC_CONFIG: u16 = 0xfb6a;
pub(crate) const INA238_ADCRANGE: u16 = 1 << 4;
/// INA238 shunt ranges, in picovolts (µA × µΩ).
const INA238_RANGE_PV: u64 = 163_840_000_000;
const INA238_LOW_RANGE_PV: u64 = 40_960_000_000;
/// INA260 integrated shunt: 1.25 mA per count.
const INA260_CURRENT_LSB_NA: u32 = 1_250_000;

impl Setup {
    pub(crate) fn new(model: Model, calibration: Calibration) -> Result<Self, ConfigError> {
        // Unused slots stay as no-op padding; `len` says how many to send.
        const PAD: RegWrite = RegWrite::word(0, 0);
        let shunt = u64::from(calibration.shunt_micro_ohms);
        // max / 2^15, from µA to nA, rounded.
        let current_lsb = || {
            let lsb = (u64::from(calibration.max_current_micro_amps) * 1000 + (1 << 14)) >> 15;
            if shunt == 0 || lsb == 0 {
                return Err(ConfigError(
                    "shunt and maximum current must be positive (at least 33 µA)",
                ));
            }
            u32::try_from(lsb).map_err(|_| ConfigError("maximum current too large"))
        };
        let (writes, len, current_lsb_na, shunt_lsb_nv) = match model {
            Model::Ina226 => {
                let lsb = current_lsb()?;
                // CAL = 0.00512 / (LSB_A × R_Ω) = 5.12e12 / (LSB_nA × R_µΩ).
                let cal = round_cal(5_120_000_000_000, u64::from(lsb) * shunt)?;
                (
                    [
                        RegWrite::word(0x00, INA226_CONFIG),
                        RegWrite::word(0x05, cal),
                        PAD,
                    ],
                    2,
                    lsb,
                    2_500,
                )
            }
            Model::Ina238 => {
                let lsb = current_lsb()?;
                let full_scale = u64::from(calibration.max_current_micro_amps) * shunt;
                if full_scale > INA238_RANGE_PV {
                    return Err(ConfigError(
                        "maximum current × shunt exceeds the 163.84 mV shunt range",
                    ));
                }
                let low_range = full_scale <= INA238_LOW_RANGE_PV;
                let gain = if low_range { 4 } else { 1 };
                // CAL = 819.2e6 × LSB_A × R_Ω × gain = LSB_nA × R_µΩ × gain × 8192 / 1e10.
                let cal = round_cal(u64::from(lsb) * shunt * gain * 8192, 10_000_000_000)?;
                (
                    [
                        RegWrite::word(0x00, if low_range { INA238_ADCRANGE } else { 0 }),
                        RegWrite::word(0x01, INA238_ADC_CONFIG),
                        RegWrite::word(0x02, cal),
                    ],
                    3,
                    lsb,
                    if low_range { 1_250 } else { 5_000 },
                )
            }
            // The integrated 2 mΩ shunt: 1.25 mA × 2 mΩ = 2.5 µV per current count.
            Model::Ina260 => (
                [RegWrite::word(0x00, INA260_CONFIG), PAD, PAD],
                1,
                INA260_CURRENT_LSB_NA,
                2_500,
            ),
        };
        Ok(Self {
            model,
            writes,
            len,
            current_lsb_na,
            shunt_lsb_nv,
        })
    }

    pub(crate) fn writes(&self) -> &[RegWrite] {
        &self.writes[..self.len]
    }

    /// The registers the readings need, in read order: (pointer, bytes).
    pub(crate) fn registers(&self) -> &'static [(u16, u8)] {
        match self.model {
            // shunt, bus, power, current
            Model::Ina226 => &[(0x01, 2), (0x02, 2), (0x03, 2), (0x04, 2)],
            // shunt, bus, die temperature, current, power (24-bit)
            Model::Ina238 => &[(0x04, 2), (0x05, 2), (0x06, 2), (0x07, 2), (0x08, 3)],
            // current, bus, power
            Model::Ina260 => &[(0x01, 2), (0x02, 2), (0x03, 2)],
        }
    }

    /// Converts raw register values, in [`Setup::registers`] order.
    pub(crate) fn reading_fixed(&self, raw: &[u32]) -> ReadingFixed {
        let signed = |value: u32| i64::from(value as u16 as i16);
        let lsb = i64::from(self.current_lsb_na);
        let shunt = |value: u32| (signed(value) * i64::from(self.shunt_lsb_nv)) as i32;
        match self.model {
            Model::Ina226 => ReadingFixed {
                shunt_voltage_nv: shunt(raw[0]),
                bus_voltage_uv: raw[1] * 1_250,
                power_nw: u64::from(raw[2]) * 25 * lsb as u64,
                current_na: signed(raw[3]) * lsb,
                die_temperature_mc: None,
            },
            Model::Ina238 => ReadingFixed {
                shunt_voltage_nv: shunt(raw[0]),
                bus_voltage_uv: raw[1] * 3_125,
                die_temperature_mc: Some(i32::from((raw[2] as u16 as i16) >> 4) * 125),
                current_na: signed(raw[3]) * lsb,
                // POWER × 0.2 × current LSB.
                power_nw: div_u64(u64::from(raw[4]) * lsb as u64, 5),
            },
            Model::Ina260 => ReadingFixed {
                current_na: signed(raw[0]) * lsb,
                shunt_voltage_nv: shunt(raw[0]),
                bus_voltage_uv: raw[1] * 1_250,
                power_nw: u64::from(raw[2]) * 10_000_000,
                die_temperature_mc: None,
            },
        }
    }

    pub(crate) fn check_ids<E>(&self, manufacturer: u16, device: u16) -> Result<(), Error<E>> {
        if manufacturer == MANUFACTURER_TI && self.model.matches_device_id(device) {
            Ok(())
        } else {
            Err(Error::WrongChip {
                model: self.model,
                manufacturer,
                device,
            })
        }
    }
}

/// `n / d` for `d != 0`. On 32-bit targets a shift-and-subtract loop of a
/// few dozen bytes stands in for the compiler's 64-bit division routine
/// (about 800 bytes on Cortex-M); 64-bit targets divide natively.
fn div_u64(n: u64, d: u64) -> u64 {
    #[cfg(target_pointer_width = "64")]
    {
        n / d
    }
    #[cfg(not(target_pointer_width = "64"))]
    {
        let (mut quotient, mut remainder) = (0u64, 0u64);
        for bit in (0..64).rev() {
            remainder = (remainder << 1) | ((n >> bit) & 1);
            if remainder >= d {
                remainder -= d;
                quotient |= 1 << bit;
            }
        }
        quotient
    }
}

/// `numerator / denominator`, rounded, as a 15-bit calibration register.
fn round_cal(numerator: u64, denominator: u64) -> Result<u16, ConfigError> {
    let cal = match numerator.checked_add(denominator / 2) {
        Some(n) if denominator != 0 => div_u64(n, denominator),
        _ => 0,
    };
    match u16::try_from(cal) {
        Ok(cal @ 1..=0x7fff) => Ok(cal),
        _ => Err(ConfigError(
            "calibration out of range; adjust the shunt or maximum current",
        )),
    }
}

/// Most registers one poll reads (the INA238 reads five).
pub(crate) const MAX_POLL_REGISTERS: usize = 5;
/// Data bytes of one poll: the INA238's four 16-bit registers and its 24-bit power.
pub(crate) const POLL_BYTES: usize = 11;

/// The pointer byte of each register of a poll.
pub(crate) fn poll_pointers(registers: &[(u16, u8)], pointers: &mut [u8; MAX_POLL_REGISTERS]) {
    for (pointer, &(register, _)) in pointers.iter_mut().zip(registers) {
        *pointer = register as u8;
    }
}

/// Builds one poll's transaction: for each register, a write of its pointer
/// and a read of its bytes. Returns the number of operations used.
pub(crate) fn poll_operations<'a>(
    registers: &[(u16, u8)],
    pointers: &'a [u8],
    data: &'a mut [u8],
    ops: &mut [Operation<'a>],
) -> usize {
    let mut rest = data;
    for (i, &(_, bytes)) in registers.iter().enumerate() {
        let (chunk, tail) = core::mem::take(&mut rest).split_at_mut(usize::from(bytes));
        rest = tail;
        ops[2 * i] = Operation::Write(&pointers[i..i + 1]);
        ops[2 * i + 1] = Operation::Read(chunk);
    }
    2 * registers.len()
}

/// The raw values of a poll's data, in `registers` order (big-endian, as the
/// datasheets send them).
pub(crate) fn poll_decode(registers: &[(u16, u8)], data: &[u8; POLL_BYTES]) -> [u32; 5] {
    let mut raw = [0u32; 5];
    let mut offset = 0;
    for (slot, &(_, bytes)) in raw.iter_mut().zip(registers) {
        let n = usize::from(bytes);
        *slot = decode_value(&data[offset..offset + n], Endian::Big);
        offset += n;
    }
    raw
}

/// One INA2xx at a 7-bit address. It holds no bus: [`ids`](Self::ids),
/// [`init`](Self::init) and [`read_fixed`](Self::read_fixed) take one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ina {
    model: Model,
    address: u8,
    pub(crate) setup: Setup,
}

impl Ina {
    /// A chip at 7-bit `address`. Validates `calibration` (ignored for the
    /// INA260) without touching the bus.
    pub fn new(model: Model, address: u8, calibration: Calibration) -> Result<Self, ConfigError> {
        Ok(Self {
            model,
            address,
            setup: Setup::new(model, calibration)?,
        })
    }

    pub fn model(&self) -> Model {
        self.model
    }

    pub fn address(&self) -> u8 {
        self.address
    }

    /// Reads the manufacturer and device IDs.
    pub fn ids<B: I2c>(&self, bus: &mut B) -> Result<(u16, u16), Error<B::Error>> {
        let mut regs = I2cRegisters::new(bus, self.address, AddressWidth::Bits8);
        let manufacturer = regs.read(u16::from(self.model.manufacturer_register()), 2)?;
        let device = regs.read(u16::from(self.model.device_register()), 2)?;
        Ok((manufacturer as u16, device as u16))
    }

    /// Verifies the chip and starts continuous conversion with the
    /// calibration.
    pub fn init<B: I2c>(&self, bus: &mut B) -> Result<(), Error<B::Error>> {
        let (manufacturer, device) = self.ids(bus)?;
        self.setup.check_ids(manufacturer, device)?;
        // One register per transfer: these chips do not auto-increment the
        // pointer on writes, so adjacent registers cannot share a message.
        let mut regs = I2cRegisters::new(bus, self.address, AddressWidth::Bits8);
        regs.write_sequence(self.setup.writes())?;
        Ok(())
    }

    /// Reads the latest conversion in integer units, in one I2C transaction.
    pub fn read_fixed<B: I2c>(&self, bus: &mut B) -> Result<ReadingFixed, Error<B::Error>> {
        let registers = self.setup.registers();
        let mut pointers = [0u8; MAX_POLL_REGISTERS];
        poll_pointers(registers, &mut pointers);
        let mut data = [0u8; POLL_BYTES];
        let mut ops: [Operation<'_>; 2 * MAX_POLL_REGISTERS] =
            core::array::from_fn(|_| Operation::Write(&[]));
        let len = poll_operations(registers, &pointers, &mut data, &mut ops);
        bus.transaction(self.address, &mut ops[..len])
            .map_err(|error| {
                Error::Register(RegisterError::bus(
                    ErrorKind::from_i2c(I2cError::kind(&error)),
                    error,
                ))
            })?;
        let raw = poll_decode(registers, &data);
        Ok(self.setup.reading_fixed(&raw))
    }
}

#[cfg(test)]
#[test]
fn div_u64_matches_native_division() {
    for (n, d) in [
        (0, 1),
        (7, 7),
        (u64::MAX, 3),
        (5_120_000_000_000, 610_350_000),
        (1 << 40, 10_000_000_000),
    ] {
        assert_eq!(div_u64(n, d), n / d);
    }
}
