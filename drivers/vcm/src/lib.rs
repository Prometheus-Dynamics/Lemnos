//! Voice-coil motor lens actuators (DW9714, DW9807, DW9817 and AK7375) on I2C,
//! served as `lens/1`: the range, a move, and the last position moved to.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `chip`: `"dw9714"`, `"dw9807"`, `"dw9817"` or `"ak7375"` (required, any
//!   case).
//! - `address`: the 7-bit I2C address, default `0x0c`.
//! - `min` and `max`: the positions the lens may move to, default `0` and the
//!   chip's highest position. They must lie within the chip's range.
//! - `direction`: which way focus moves as the position goes up, `"near"`,
//!   `"far"` or `"unknown"` (the default). Mounting decides it, and no chip's
//!   datasheet fixes it, so the driver never guesses: the board says it.
//! - `settle_us`: microseconds after a move before the lens is still, for any
//!   move size. Unset (the default) the driver answers `None`. It knows no
//!   settle figure for these chips and sets no slew or ringing-control mode, so
//!   the board supplies the figure the module's datasheet or measurement gives.
//!
//! Binding does not touch the bus. The first move powers the chip up and
//! returns `AGAIN`; the next move writes the position. The chip needs its
//! settle time between those two calls, which is the host's pacing: the driver
//! never sleeps. A move that fails reports its status, and the next move
//! powers the chip up again before writing, since a VCM cannot say whether it
//! was reset. The driver never gives up and never switches itself off. It
//! makes no retries of its own.
//!
//! VCMs report no position, so [`Lens::position`] returns the last position
//! written, or `NOT_SUPPORTED` before the first move.
//!
//! Power-up time (`power_up_us` in [`chip`]) is not the settle time: it is the
//! wait before the first move, and [`Lens::settle_us`] never reports it.

#![no_std]

pub mod chip;

#[cfg(test)]
mod tests;

use chip::{Chip, DEFAULT_ADDRESS, Format};
use lemnos_contract_lens::{FocusDirection, Lens, Range};
use lemnos_core::{
    Config, ConfigValue, Description, Health, I2c, Item, Lifecycle, MethodId, Result, Status, Unit,
    driver, status_of,
};

const POSITION: MethodId = MethodId {
    contract: 0,
    method: 3,
};

/// Where the chip is in its power-up sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Powered down, or the last operation failed.
    Off,
    /// Powered up; the next move is the first.
    Starting,
    /// Moving, with the last position written.
    Ready,
}

#[driver("vcm", provides = [Lens])]
pub struct Vcm {
    #[needs]
    bus: I2c,
    address: u8,
    format: Format,
    min: u16,
    max: u16,
    direction: FocusDirection,
    settle_us: Option<u32>,
    phase: Phase,
    /// The last position written, kept across faults.
    position: Option<u16>,
    /// Why the chip is not moving, while it is not.
    fault: &'static str,
}

impl Lifecycle for Vcm {
    type Needs = VcmNeeds;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;

    fn start(needs: VcmNeeds, cfg: &Config<'_>) -> Result<Self> {
        let chip = chip_from_config(cfg)?;
        let format = chip.format();
        let (min, max) = range_from_config(cfg, &format)?;
        let direction = direction_from_config(cfg)?;
        let settle_us = settle_from_config(cfg)?;
        Ok(Self {
            bus: needs.bus,
            address: address(cfg)?,
            format,
            min,
            max,
            direction,
            settle_us,
            phase: Phase::Off,
            position: None,
            fault: "not powered: powers up on the next move",
        })
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::value("position", POSITION).unit(Unit::One, 0));
    }

    fn health(&self) -> Health {
        match self.phase {
            Phase::Ready => Health::Ok,
            Phase::Starting => Health::Degraded("powering up: the next move writes the position"),
            Phase::Off => Health::Degraded(self.fault),
        }
    }
}

impl Vcm {
    /// Records a fault, so the next move powers the chip up again.
    fn fail(&mut self, kind: lemnos_core::ErrorKind) -> Status {
        self.phase = Phase::Off;
        self.fault = "bus error: powers up again on the next move";
        status_of(kind)
    }
}

impl Lens for Vcm {
    fn range(&mut self) -> Result<Range> {
        Ok(Range {
            min: self.min,
            max: self.max,
        })
    }

    fn set_position(&mut self, position: u16) -> Result<()> {
        match self.phase {
            Phase::Off => {
                if let Err(kind) = chip::power_up(&mut self.bus, self.address, &self.format) {
                    return Err(self.fail(kind));
                }
                self.phase = Phase::Starting;
                Err(Status::AGAIN)
            }
            Phase::Starting | Phase::Ready => {
                let target = position.clamp(self.min, self.max);
                match chip::move_to(&mut self.bus, self.address, &self.format, target) {
                    Ok(written) => {
                        self.phase = Phase::Ready;
                        self.position = Some(written);
                        Ok(())
                    }
                    Err(kind) => Err(self.fail(kind)),
                }
            }
        }
    }

    fn position(&mut self) -> Result<u16> {
        self.position.ok_or(Status::NOT_SUPPORTED)
    }

    fn direction(&mut self) -> Result<FocusDirection> {
        Ok(self.direction)
    }

    fn settle_us(&mut self, _distance: u16) -> Result<Option<u32>> {
        Ok(self.settle_us)
    }
}

/// The `direction` config: which way focus moves as the position goes up.
fn direction_from_config(cfg: &Config<'_>) -> Result<FocusDirection> {
    match cfg.get("direction") {
        None => Ok(FocusDirection::Unknown),
        Some(ConfigValue::Str(name)) if name.eq_ignore_ascii_case("unknown") => {
            Ok(FocusDirection::Unknown)
        }
        Some(ConfigValue::Str(name)) if name.eq_ignore_ascii_case("near") => {
            Ok(FocusDirection::TowardNear)
        }
        Some(ConfigValue::Str(name)) if name.eq_ignore_ascii_case("far") => {
            Ok(FocusDirection::TowardFar)
        }
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}

/// The `settle_us` config: a settle time for every move, if the board has one.
fn settle_from_config(cfg: &Config<'_>) -> Result<Option<u32>> {
    match cfg.get("settle_us") {
        None => Ok(None),
        Some(ConfigValue::Int(us)) => u32::try_from(us)
            .map(Some)
            .map_err(|_| Status::INVALID_ARGUMENT),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}

/// The `chip` config: one of the built-in chips.
fn chip_from_config(cfg: &Config<'_>) -> Result<Chip> {
    match cfg.get("chip") {
        Some(ConfigValue::Str(name)) => Chip::from_name(name).ok_or(Status::INVALID_ARGUMENT),
        _ => Err(Status::INVALID_ARGUMENT),
    }
}

/// The `address` config: a 7-bit address that is not reserved.
fn address(cfg: &Config<'_>) -> Result<u8> {
    match cfg.get("address") {
        None => Ok(DEFAULT_ADDRESS),
        Some(ConfigValue::Int(address)) if (0x08..=0x77).contains(&address) => Ok(address as u8),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}

/// The `min` and `max` config: positions within the chip's range, `min <= max`.
fn range_from_config(cfg: &Config<'_>, format: &Format) -> Result<(u16, u16)> {
    let highest = format.max_position();
    let min = position_config(cfg, "min", 0, highest)?;
    let max = position_config(cfg, "max", highest, highest)?;
    if min > max {
        return Err(Status::INVALID_ARGUMENT);
    }
    Ok((min, max))
}

fn position_config(cfg: &Config<'_>, key: &str, default: u16, highest: u16) -> Result<u16> {
    match cfg.get(key) {
        None => Ok(default),
        Some(ConfigValue::Int(value)) => u16::try_from(value)
            .ok()
            .filter(|value| *value <= highest)
            .ok_or(Status::INVALID_ARGUMENT),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}
