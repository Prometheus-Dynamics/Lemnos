//! The BMI088 accelerometer and gyroscope, served as `accelerometer/1` (m/s²)
//! and `gyroscope/1` (rad/s), over I2C.
//!
//! The chip is two dies on one bus at two addresses. Each contract reads its
//! own die, so the two are independent: a missing gyroscope leaves the
//! accelerometer working. Health is the device's, and names the first die that
//! is not working. A die nobody has read yet is not working.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `accel_address` (default `0x18`) and `gyro_address` (default `0x68`): the
//!   7-bit addresses.
//! - `accel_range` (g: 3, 6, 12 or 24; default 6) and `accel_rate` (Hz: 12.5,
//!   25, 50, 100, 200, 400, 800 or 1600; default 100).
//! - `gyro_range` (degrees per second: 125, 250, 500, 1000 or 2000; default
//!   2000) and `gyro_rate` (Hz: 100, 200, 400, 1000 or 2000; default 2000).
//! - `fifo` (bool; default false): read through the chip's FIFOs. See below.
//!
//! Power-up is spread over reads, and the driver never sleeps. Each die's first
//! read soft-resets it and returns `AGAIN`; the next steps check the chip ID,
//! power the die on and write its range and rate, one step a read. The host's
//! read interval gives the chip time between steps. Each step can be retried,
//! and the range and rate are read back, so a write that landed too early fails
//! its readback and is written again on a later read. A failed transfer reports
//! on that read, and the die starts over on a later one (a failed power-up step
//! is retried in place). A wrong chip ID is refused and nothing is written. The
//! driver never gives up and never switches itself off.
//!
//! Without `fifo`, each read takes the die's current sample.
//!
//! With `fifo`, each read drains the die's whole FIFO and returns the newest
//! sample. The older samples the batch held are dropped, and nothing is
//! stamped: the contract returns one sample and has no time field. Health
//! reports `Degraded` on the read that found the FIFO overflowed, so the host
//! sees samples were lost. A read that finds no new sample returns the last
//! one. A read while a die is still powering up returns `AGAIN`, with or
//! without the FIFO.
//!
//! Not in this driver: the gyroscope alignment and the sample-time stamps that
//! need a host clock, and the auto-calibration that needs its estimators. The
//! contracts have no clock and the driver has no timer.

#![no_std]

mod chip;
#[cfg(test)]
mod tests;

use chip::{Failure, Kind, Setting};
use lemnos_contract_accelerometer::Accelerometer;
use lemnos_contract_gyroscope::Gyroscope;
use lemnos_core::{
    Config, ConfigValue, Description, Health, I2c, Item, Lifecycle, MethodId, Result, Status, Unit,
    Vector3, driver,
};

/// The accelerometer's 7-bit address with SDO1 low.
pub const DEFAULT_ACCEL_ADDRESS: u8 = 0x18;
/// The gyroscope's 7-bit address with SDO2 low.
pub const DEFAULT_GYRO_ADDRESS: u8 = 0x68;

const ACCEL_READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};
const GYRO_READ: MethodId = MethodId {
    contract: 1,
    method: 1,
};

/// Where a die is in its power-up sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Not started, or its last failure sent it back here.
    Off,
    /// Soft-reset; the next step checks the chip ID again.
    Reset,
    /// Accelerometer only: suspend mode is off; the next step powers it on.
    Power,
    /// The next step writes the range and rate (and the FIFO setup).
    Configure,
    /// Measuring.
    Ready,
}

/// What a die is doing, as a health reason.
#[derive(Debug, Clone, Copy)]
enum Note {
    NotStarted,
    Powering,
    WrongId,
    Transfer,
    Overflow,
}

fn text(kind: Kind, note: Note) -> &'static str {
    match (kind, note) {
        (Kind::Accel, Note::NotStarted) => "accelerometer: not started",
        (Kind::Accel, Note::Powering) => "accelerometer: powering up",
        (Kind::Accel, Note::WrongId) => "accelerometer: wrong chip id",
        (Kind::Accel, Note::Transfer) => "accelerometer: transfer failed",
        (Kind::Accel, Note::Overflow) => "accelerometer: FIFO overflowed, samples lost",
        (Kind::Gyro, Note::NotStarted) => "gyroscope: not started",
        (Kind::Gyro, Note::Powering) => "gyroscope: powering up",
        (Kind::Gyro, Note::WrongId) => "gyroscope: wrong chip id",
        (Kind::Gyro, Note::Transfer) => "gyroscope: transfer failed",
        (Kind::Gyro, Note::Overflow) => "gyroscope: FIFO overflowed, samples lost",
    }
}

/// One die: its address, settings, power-up phase and newest sample.
struct Die {
    kind: Kind,
    address: u8,
    fifo: bool,
    setting: Setting,
    phase: Phase,
    /// The newest sample; a read with no new one repeats it.
    last: Option<[i16; 3]>,
    note: &'static str,
    /// The last FIFO drain overflowed, so samples were lost.
    lost: bool,
}

impl Die {
    fn new(kind: Kind, address: u8, fifo: bool, setting: Setting) -> Self {
        Self {
            kind,
            address,
            fifo,
            setting,
            phase: Phase::Off,
            last: None,
            note: text(kind, Note::NotStarted),
            lost: false,
        }
    }

    fn set(&mut self, phase: Phase) {
        self.phase = phase;
        if phase != Phase::Ready {
            self.note = text(self.kind, Note::Powering);
        }
    }

    /// Runs one step of power-up, or reads the die once it is ready. `None`
    /// while the die is still powering up.
    fn sample(&mut self, bus: &mut I2c) -> chip::Step<Option<[i16; 3]>> {
        let (address, kind) = (self.address, self.kind);
        match self.phase {
            Phase::Off => {
                chip::check_id(bus, address, kind)?;
                chip::soft_reset(bus, address, kind);
                self.set(Phase::Reset);
                return Ok(None);
            }
            Phase::Reset => {
                chip::check_id(bus, address, kind)?;
                match kind {
                    Kind::Accel => {
                        chip::accel_power_config(bus, address)?;
                        self.set(Phase::Power);
                    }
                    Kind::Gyro => self.set(Phase::Configure),
                }
                return Ok(None);
            }
            Phase::Power => {
                chip::accel_power_control(bus, address)?;
                self.set(Phase::Configure);
                return Ok(None);
            }
            Phase::Configure => {
                chip::configure(bus, address, kind, self.setting, self.fifo)?;
                self.phase = Phase::Ready;
            }
            Phase::Ready => {}
        }
        if self.fifo {
            self.drain(bus)
        } else {
            chip::read_axes(bus, address, kind).map(Some)
        }
    }

    /// Drains the die's FIFO and keeps its newest sample.
    fn drain(&mut self, bus: &mut I2c) -> chip::Step<Option<[i16; 3]>> {
        let (address, kind) = (self.address, self.kind);
        let newest = match kind {
            Kind::Accel => {
                let bytes = chip::accel_fifo_bytes(bus, address)?;
                let drain = chip::drain_accel(bus, address, bytes)?;
                self.lost = drain.skipped > 0;
                drain.last
            }
            Kind::Gyro => {
                let frames = chip::gyro_fifo_frames(bus, address)?;
                self.lost = frames >= chip::GYRO_FIFO_FRAMES;
                chip::drain_gyro(bus, address, frames)?
            }
        };
        if newest.is_some() {
            self.last = newest;
        }
        Ok(self.last)
    }

    /// Records a failure. A wrong chip, or a failure while measuring, sends the
    /// die back to power-up for a later read. Any other failure is retried in
    /// place.
    fn fail(&mut self, failure: &Failure) -> Status {
        let wrong_id = matches!(failure, Failure::WrongChip);
        self.note = text(
            self.kind,
            if wrong_id {
                Note::WrongId
            } else {
                Note::Transfer
            },
        );
        if wrong_id || self.phase == Phase::Ready {
            self.phase = Phase::Off;
            self.last = None;
            self.lost = false;
        }
        failure.status()
    }

    /// Why the die is not fully working, if it is not.
    fn trouble(&self) -> Option<&'static str> {
        if self.phase != Phase::Ready {
            Some(self.note)
        } else if self.lost {
            Some(text(self.kind, Note::Overflow))
        } else {
            None
        }
    }
}

/// A read of one die: its sample in SI units, `AGAIN` while it powers up, or
/// the failure's status.
fn read_die(die: &mut Die, bus: &mut I2c) -> Result<Vector3> {
    match die.sample(bus) {
        Ok(Some(raw)) => Ok(chip::to_vector(raw, die.setting.scale)),
        Ok(None) => Err(Status::AGAIN),
        Err(failure) => Err(die.fail(&failure)),
    }
}

/// The BMI088's two dies on one I2C bus.
#[driver("bmi088", provides = [Accelerometer, Gyroscope])]
pub struct Bmi088 {
    #[needs]
    bus: I2c,
    accel: Die,
    gyro: Die,
}

impl Lifecycle for Bmi088 {
    type Needs = Bmi088Needs;

    fn start(needs: Bmi088Needs, cfg: &Config<'_>) -> Result<Self> {
        let fifo = match cfg.get("fifo") {
            None => false,
            Some(ConfigValue::Bool(on)) => on,
            Some(_) => return Err(Status::INVALID_ARGUMENT),
        };
        let accel = chip::accel_setting(
            int(cfg, "accel_range", 6)?,
            number(cfg, "accel_rate", 100.0)?,
        )
        .ok_or(Status::INVALID_ARGUMENT)?;
        let gyro = chip::gyro_setting(int(cfg, "gyro_range", 2000)?, int(cfg, "gyro_rate", 2000)?)
            .ok_or(Status::INVALID_ARGUMENT)?;
        Ok(Self {
            bus: needs.bus,
            accel: Die::new(
                Kind::Accel,
                address(cfg, "accel_address", DEFAULT_ACCEL_ADDRESS)?,
                fifo,
                accel,
            ),
            gyro: Die::new(
                Kind::Gyro,
                address(cfg, "gyro_address", DEFAULT_GYRO_ADDRESS)?,
                fifo,
                gyro,
            ),
        })
    }

    fn describe(&self, out: &mut Description) {
        for (name, field) in [
            ("acceleration.x", "x"),
            ("acceleration.y", "y"),
            ("acceleration.z", "z"),
        ] {
            out.add(
                Item::value(name, ACCEL_READ)
                    .unit(Unit::MetrePerSecondSquared, 0)
                    .field(field),
            );
        }
        for (name, field) in [
            ("angular_rate.x", "x"),
            ("angular_rate.y", "y"),
            ("angular_rate.z", "z"),
        ] {
            out.add(
                Item::value(name, GYRO_READ)
                    .unit(Unit::RadianPerSecond, 0)
                    .field(field),
            );
        }
    }

    fn health(&self) -> Health {
        match self.accel.trouble().or_else(|| self.gyro.trouble()) {
            Some(reason) => Health::Degraded(reason),
            None => Health::Ok,
        }
    }
}

impl Accelerometer for Bmi088 {
    fn read(&mut self) -> Result<Vector3> {
        read_die(&mut self.accel, &mut self.bus)
    }
}

impl Gyroscope for Bmi088 {
    fn read(&mut self) -> Result<Vector3> {
        read_die(&mut self.gyro, &mut self.bus)
    }
}

/// An integer setting, or `default` when absent. Any other type is refused.
fn int(cfg: &Config<'_>, key: &str, default: i64) -> Result<i64> {
    match cfg.get(key) {
        None => Ok(default),
        Some(ConfigValue::Int(value)) => Ok(value),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}

/// A number setting (an integer is accepted), or `default` when absent.
fn number(cfg: &Config<'_>, key: &str, default: f64) -> Result<f64> {
    match cfg.get(key) {
        None => Ok(default),
        Some(ConfigValue::Int(value)) => Ok(value as f64),
        Some(ConfigValue::Float(value)) => Ok(value),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}

/// A 7-bit address that is not reserved, or `default` when absent.
fn address(cfg: &Config<'_>, key: &str, default: u8) -> Result<u8> {
    let value = int(cfg, key, i64::from(default))?;
    if (0x08..=0x77).contains(&value) {
        Ok(value as u8)
    } else {
        Err(Status::INVALID_ARGUMENT)
    }
}
