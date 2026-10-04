//! Texas Instruments INA226, INA238 and INA260 power monitors over
//! embedded-hal I2C, without `std` or allocation.
//!
//! [`Ina`] verifies the chip, programs continuous conversion and the current
//! calibration, and reads bus voltage, shunt voltage, current and power in SI
//! units (plus die temperature on the INA238). [`asynch::Ina`] is the same over
//! embedded-hal-async.
//!
//! The calibration follows the datasheets: the current LSB is
//! `max_current_a / 2^15`, and the INA238 picks its ±40.96 mV shunt range
//! whenever `max_current_a × shunt_ohms` fits, for four times the resolution.

#![no_std]
#![forbid(unsafe_code)]

pub mod asynch;

#[cfg(test)]
mod tests;

use core::fmt;
use embedded_hal::i2c::I2c;
use lemnos_hal::register::{AddressWidth, I2cRegisters, RegWrite, RegisterBus, RegisterError};
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
    /// INA260: integrated 2 mΩ shunt; [`Config`] is ignored.
    Ina260,
}

impl Model {
    pub(crate) fn manufacturer_register(self) -> u16 {
        match self {
            Self::Ina238 => 0x3e,
            Self::Ina226 | Self::Ina260 => 0xfe,
        }
    }

    pub(crate) fn device_register(self) -> u16 {
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// Shunt resistance in ohms.
    pub shunt_ohms: f32,
    /// Largest expected current in amperes; sets the current resolution.
    pub max_current_a: f32,
}

impl Config {
    pub const fn new(shunt_ohms: f32, max_current_a: f32) -> Self {
        Self {
            shunt_ohms,
            max_current_a,
        }
    }
}

/// A [`Config`] the chip cannot be calibrated for.
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

/// One measurement, in SI units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    pub bus_voltage_v: f32,
    pub shunt_voltage_v: f32,
    pub current_a: f32,
    pub power_w: f32,
    /// Only the INA238 measures it.
    pub die_temperature_c: Option<f32>,
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
            Self::Register(error) => error.kind(),
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

/// Register writes and scale factors derived from a model and [`Config`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Setup {
    pub(crate) model: Model,
    writes: [RegWrite; 3],
    len: usize,
    current_lsb: f32,
    shunt_lsb: f32,
}

/// INA226 config: 16-sample averaging, 1.1 ms conversions, continuous shunt and bus.
const INA226_CONFIG: u16 = 0x4527;
/// INA260 config: as INA226 (bits 14-12 read back as 0b110).
const INA260_CONFIG: u16 = 0x6527;
/// INA238 ADC config: continuous shunt, bus and temperature, 1052 µs, 16 samples.
const INA238_ADC_CONFIG: u16 = 0xfb6a;
const INA238_ADCRANGE: u16 = 1 << 4;
const CAL_MAX: f32 = 0x7fff as f32;

impl Setup {
    pub(crate) fn new(model: Model, config: Config) -> Result<Self, ConfigError> {
        // Unused slots stay as no-op padding; `len` says how many to send.
        const PAD: RegWrite = RegWrite::word(0, 0);
        let calibrated = || {
            let positive = |value: f32| value.is_finite() && value > 0.0;
            if !positive(config.shunt_ohms) || !positive(config.max_current_a) {
                return Err(ConfigError(
                    "shunt_ohms and max_current_a must be positive and finite",
                ));
            }
            Ok(config.max_current_a / 32768.0)
        };
        let (writes, len, current_lsb, shunt_lsb) = match model {
            Model::Ina226 => {
                let current_lsb = calibrated()?;
                let cal = round_cal(0.00512 / (current_lsb * config.shunt_ohms))?;
                (
                    [
                        RegWrite::word(0x00, INA226_CONFIG),
                        RegWrite::word(0x05, cal),
                        PAD,
                    ],
                    2,
                    current_lsb,
                    2.5e-6,
                )
            }
            Model::Ina238 => {
                let current_lsb = calibrated()?;
                let full_scale = config.max_current_a * config.shunt_ohms;
                if full_scale > 0.16384 {
                    return Err(ConfigError(
                        "max_current_a × shunt_ohms exceeds the 163.84 mV shunt range",
                    ));
                }
                let low_range = full_scale <= 0.04096;
                let gain = if low_range { 4.0 } else { 1.0 };
                let cal = round_cal(819.2e6 * current_lsb * config.shunt_ohms * gain)?;
                (
                    [
                        RegWrite::word(0x00, if low_range { INA238_ADCRANGE } else { 0 }),
                        RegWrite::word(0x01, INA238_ADC_CONFIG),
                        RegWrite::word(0x02, cal),
                    ],
                    3,
                    current_lsb,
                    if low_range { 1.25e-6 } else { 5.0e-6 },
                )
            }
            Model::Ina260 => (
                [RegWrite::word(0x00, INA260_CONFIG), PAD, PAD],
                1,
                1.25e-3,
                0.0,
            ),
        };
        Ok(Self {
            model,
            writes,
            len,
            current_lsb,
            shunt_lsb,
        })
    }

    pub(crate) fn writes(&self) -> &[RegWrite] {
        &self.writes[..self.len]
    }

    /// The registers [`Setup::reading`] needs, in read order.
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
    pub(crate) fn reading(&self, raw: &[u32]) -> Reading {
        let signed = |value: u32| value as u16 as i16 as f32;
        match self.model {
            Model::Ina226 => Reading {
                shunt_voltage_v: signed(raw[0]) * self.shunt_lsb,
                bus_voltage_v: raw[1] as f32 * 1.25e-3,
                power_w: raw[2] as f32 * 25.0 * self.current_lsb,
                current_a: signed(raw[3]) * self.current_lsb,
                die_temperature_c: None,
            },
            Model::Ina238 => Reading {
                shunt_voltage_v: signed(raw[0]) * self.shunt_lsb,
                bus_voltage_v: raw[1] as f32 * 3.125e-3,
                die_temperature_c: Some(((raw[2] as u16 as i16) >> 4) as f32 * 0.125),
                current_a: signed(raw[3]) * self.current_lsb,
                power_w: raw[4] as f32 * 0.2 * self.current_lsb,
            },
            Model::Ina260 => {
                let current_a = signed(raw[0]) * self.current_lsb;
                Reading {
                    current_a,
                    bus_voltage_v: raw[1] as f32 * 1.25e-3,
                    power_w: raw[2] as f32 * 10.0e-3,
                    shunt_voltage_v: current_a * 0.002,
                    die_temperature_c: None,
                }
            }
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

fn round_cal(cal: f32) -> Result<u16, ConfigError> {
    if !(1.0..=CAL_MAX).contains(&cal) {
        return Err(ConfigError(
            "calibration out of range; adjust shunt_ohms or max_current_a",
        ));
    }
    Ok((cal + 0.5) as u16)
}

/// An INA2xx over a blocking I2C bus.
#[derive(Debug)]
pub struct Ina<I2C> {
    i2c: I2C,
    address: u8,
    setup: Setup,
}

impl<I2C: I2c> Ina<I2C> {
    /// A chip at 7-bit `address`. Validates `config` (ignored for the INA260)
    /// without touching the bus. Call [`init`](Self::init) once per power
    /// cycle; a driver rebuilt around a chip that is already initialized can
    /// [`read`](Self::read) straight away.
    pub fn new(i2c: I2C, address: u8, model: Model, config: Config) -> Result<Self, ConfigError> {
        Ok(Self {
            i2c,
            address,
            setup: Setup::new(model, config)?,
        })
    }

    /// An INA260 at 7-bit `address`.
    pub fn ina260(i2c: I2C, address: u8) -> Self {
        Self::new(i2c, address, Model::Ina260, Config::new(0.002, 1.0))
            .expect("the INA260 needs no calibration")
    }

    pub fn model(&self) -> Model {
        self.setup.model
    }

    pub fn address(&self) -> u8 {
        self.address
    }

    fn registers(&mut self) -> I2cRegisters<&mut I2C> {
        I2cRegisters::new(&mut self.i2c, self.address, AddressWidth::Bits8)
    }

    /// Reads the manufacturer and device IDs.
    pub fn ids(&mut self) -> Result<(u16, u16), Error<I2C::Error>> {
        let model = self.setup.model;
        let mut regs = self.registers();
        Ok((
            regs.read16(model.manufacturer_register())?,
            regs.read16(model.device_register())?,
        ))
    }

    /// Verifies the chip and starts continuous conversion with the
    /// calibration.
    pub fn init(&mut self) -> Result<(), Error<I2C::Error>> {
        let (manufacturer, device) = self.ids()?;
        self.setup.check_ids(manufacturer, device)?;
        let setup = self.setup;
        self.registers().write_sequence(setup.writes())?;
        Ok(())
    }

    /// Reads the latest conversion.
    pub fn read(&mut self) -> Result<Reading, Error<I2C::Error>> {
        let setup = self.setup;
        let mut raw = [0u32; 5];
        let mut regs = self.registers();
        for (slot, (register, bytes)) in raw.iter_mut().zip(setup.registers()) {
            *slot = regs.read(*register, *bytes)?;
        }
        Ok(setup.reading(&raw))
    }

    /// Gives the bus back.
    pub fn release(self) -> I2C {
        self.i2c
    }
}
