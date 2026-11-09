//! A power switch: a GPIO output that enables a load (a USB port's power), with
//! an optional fault input. Served as `power-switch/1`.
//!
//! Settings come from the device's `config` in the board file, all booleans
//! unless noted:
//!
//! - `default_on` (default `true`): the level the output starts at. It is the
//!   line's initial value, set in the one configuration the driver makes, so the
//!   line is never driven through the other level.
//! - `active_low` (default `false`): the output's polarity.
//! - `fault_active_low` (default `true`): the fault input's polarity. With the
//!   default, a fault that pulls the pin low reads `true`.
//! - `enable_delay_ms`: must be absent or 0. A delay after turning on needs a
//!   timer the driver model does not give drivers, so any other value is refused.
//!
//! The driver never waits. `cycle` is not implemented yet: see its docs.

#![no_std]

#[cfg(test)]
mod tests;

use lemnos_contract_power_switch::PowerSwitch;
use lemnos_core::{
    Config, ConfigValue, Description, Item, Lifecycle, Line, MethodId, Result, Status, driver,
    status_of,
};
use lemnos_hal::raw::LineConfig;

/// The longest off time `cycle` accepts, in milliseconds.
pub const MAX_CYCLE_OFF_MS: u32 = 10_000;

const IS_ON: MethodId = MethodId {
    contract: 0,
    method: 2,
};
const FAULT: MethodId = MethodId {
    contract: 0,
    method: 3,
};

#[driver("gpio-power-switch", provides = [PowerSwitch])]
pub struct GpioPowerSwitch {
    #[needs]
    line: Line,
    #[needs(optional)]
    fault: Option<Line>,
    /// The level last driven (the pin is not read back).
    on: bool,
}

impl Lifecycle for GpioPowerSwitch {
    type Needs = GpioPowerSwitchNeeds;

    fn start(mut needs: GpioPowerSwitchNeeds, cfg: &Config<'_>) -> Result<Self> {
        if !matches!(cfg.get("enable_delay_ms"), None | Some(ConfigValue::Int(0))) {
            return Err(Status::INVALID_ARGUMENT);
        }
        let default_on = flag(cfg, "default_on", true)?;
        let mut output = LineConfig::output(default_on);
        output.active_low = flag(cfg, "active_low", false)?;
        needs.line.configure(&output).map_err(status_of)?;
        let mut input = LineConfig::input();
        input.active_low = flag(cfg, "fault_active_low", true)?;
        if let Some(fault) = needs.fault.as_mut() {
            fault.configure(&input).map_err(status_of)?;
        }
        Ok(Self {
            line: needs.line,
            fault: needs.fault,
            on: default_on,
        })
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::value("power.on", IS_ON));
        if self.fault.is_some() {
            out.add(Item::value("power.fault", FAULT));
        }
    }
}

impl PowerSwitch for GpioPowerSwitch {
    fn set(&mut self, on: bool) -> Result<()> {
        self.line.set(on).map_err(status_of)?;
        self.on = on;
        Ok(())
    }

    fn is_on(&mut self) -> Result<bool> {
        Ok(self.on)
    }

    fn fault(&mut self) -> Result<Option<bool>> {
        match self.fault.as_mut() {
            None => Ok(None),
            Some(fault) => fault.get().map(Some).map_err(status_of),
        }
    }

    /// Not implemented on this driver model. A turn-back-on has to be scheduled
    /// by something with a clock, and a driver gets neither a clock nor a
    /// deadline hook, so the driver cannot promise to restore the load. It
    /// refuses rather than cut power it cannot restore: an off time above
    /// [`MAX_CYCLE_OFF_MS`] is `INVALID_ARGUMENT`, zero does nothing, and any
    /// other time is `NOT_SUPPORTED` with the line untouched.
    fn cycle(&mut self, off_ms: u32) -> Result<()> {
        if off_ms > MAX_CYCLE_OFF_MS {
            return Err(Status::INVALID_ARGUMENT);
        }
        if off_ms == 0 {
            return Ok(());
        }
        Err(Status::NOT_SUPPORTED)
    }
}

/// A boolean setting: absent means `default`, and a non-boolean is refused.
fn flag(cfg: &Config<'_>, key: &str, default: bool) -> Result<bool> {
    match cfg.get(key) {
        None => Ok(default),
        Some(ConfigValue::Bool(value)) => Ok(value),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}
