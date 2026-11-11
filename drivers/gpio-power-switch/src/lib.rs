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
//!
//! The driver never waits. It turns the pin on and off, and anything timed is up to the caller.

#![no_std]

#[cfg(test)]
mod tests;

use lemnos_contract_power_switch::PowerSwitch;
use lemnos_core::{
    Config, ConfigValue, Description, Item, Lifecycle, Line, MethodId, Result, Status, driver,
    status_of,
};
use lemnos_hal::raw::LineConfig;

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
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;

    fn start(mut needs: GpioPowerSwitchNeeds, cfg: &Config<'_>) -> Result<Self> {
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
}

/// A boolean setting: absent means `default`, and a non-boolean is refused.
fn flag(cfg: &Config<'_>, key: &str, default: bool) -> Result<bool> {
    match cfg.get(key) {
        None => Ok(default),
        Some(ConfigValue::Bool(value)) => Ok(value),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}
