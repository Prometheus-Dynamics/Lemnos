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
    Config, ConfigError, Description, Health, I2c, Item, Key, Kind, Lifecycle, MethodId, Result,
    Status, Unit, driver, status_of,
};

const POSITION: MethodId = MethodId {
    contract: 0,
    method: 3,
};

/// The settings this driver reads from the board file's `config`.
const KEYS: &[Key] = &[
    Key::required("chip", Kind::Str),
    Key::optional("address", Kind::Int),
    Key::optional("min", Kind::Int),
    Key::optional("max", Kind::Int),
    Key::optional("direction", Kind::Str),
    Key::optional("settle_us", Kind::Int),
];

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

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        let chip = chip_of(cfg).ok_or(ConfigError::new(
            "chip",
            "must be dw9714, dw9807, dw9817 or ak7375",
        ))?;
        if cfg
            .int("address")
            .is_some_and(|address| !(0x08..=0x77).contains(&address))
        {
            return Err(ConfigError::new(
                "address",
                "must be a 7-bit address from 0x08 to 0x77",
            ));
        }
        if cfg
            .str("direction")
            .is_some_and(|name| direction_named(name).is_none())
        {
            return Err(ConfigError::new(
                "direction",
                "must be unknown, near or far",
            ));
        }
        if cfg
            .int("settle_us")
            .is_some_and(|us| u32::try_from(us).is_err())
        {
            return Err(ConfigError::new(
                "settle_us",
                "must be from 0 to 4294967295",
            ));
        }
        let highest = i64::from(chip.format().max_position());
        for key in ["min", "max"] {
            if cfg
                .int(key)
                .is_some_and(|value| !(0..=highest).contains(&value))
            {
                return Err(ConfigError::new(
                    key,
                    "must be a position within the chip's range",
                ));
            }
        }
        let min = cfg.int("min").unwrap_or(0);
        let max = cfg.int("max").unwrap_or(highest);
        if min > max {
            return Err(ConfigError::new("min", "must not be above max"));
        }
        Ok(())
    }

    fn start(needs: VcmNeeds, cfg: &Config<'_>) -> Result<Self> {
        // The chip has no default; check_config has already refused its absence.
        let chip = chip_of(cfg).ok_or(Status::INVALID_ARGUMENT)?;
        let format = chip.format();
        let (min, max) = range_of(cfg, &format);
        Ok(Self {
            bus: needs.bus,
            address: address_of(cfg),
            format,
            min,
            max,
            direction: direction_of(cfg),
            settle_us: cfg.int("settle_us").and_then(|us| u32::try_from(us).ok()),
            phase: Phase::Off,
            position: None,
            fault: "not powered: powers up on the next move",
        })
    }

    fn describe(&self, out: &mut Description) {
        out.set_address(self.address);
        // The span the board's configured limits allow; one step is one code.
        out.add(
            Item::value("position", POSITION)
                .unit(Unit::One, 0)
                .range(i64::from(self.min), i64::from(self.max))
                .step(1),
        );
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

/// The focus direction a `direction` name gives, in any case.
fn direction_named(name: &str) -> Option<FocusDirection> {
    if name.eq_ignore_ascii_case("unknown") {
        Some(FocusDirection::Unknown)
    } else if name.eq_ignore_ascii_case("near") {
        Some(FocusDirection::TowardNear)
    } else if name.eq_ignore_ascii_case("far") {
        Some(FocusDirection::TowardFar)
    } else {
        None
    }
}

/// The `direction` config, `unknown` when the board leaves it out. Checked by `check_config`.
fn direction_of(cfg: &Config<'_>) -> FocusDirection {
    cfg.str("direction")
        .and_then(direction_named)
        .unwrap_or(FocusDirection::Unknown)
}

/// The `chip` config: one of the built-in chips.
fn chip_of(cfg: &Config<'_>) -> Option<Chip> {
    cfg.str("chip").and_then(Chip::from_name)
}

/// The `address` config, or the chip's usual address. Checked by `check_config`.
fn address_of(cfg: &Config<'_>) -> u8 {
    cfg.int("address")
        .and_then(|address| u8::try_from(address).ok())
        .unwrap_or(DEFAULT_ADDRESS)
}

/// The `min` and `max` config, `min` from 0 and `max` from the chip's highest position when the
/// board leaves them out. Checked by `check_config`.
fn range_of(cfg: &Config<'_>, format: &Format) -> (u16, u16) {
    let min = cfg
        .int("min")
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(0);
    let max = cfg
        .int("max")
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(format.max_position());
    (min, max)
}
