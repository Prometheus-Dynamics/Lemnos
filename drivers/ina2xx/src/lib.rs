//! Texas Instruments INA226, INA238 and INA260 power monitors on I2C, served as
//! `power-monitor/1`: bus volts, shunt volts, amps and watts.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `chip`: `"ina226"`, `"ina238"` or `"ina260"` (required, any case).
//! - `address`: the 7-bit I2C address, default `0x40`.
//! - `shunt_micro_ohms` (integer) or `shunt_ohms` (number): the external shunt.
//! - `max_current_micro_amps` (integer) or `max_current_a` (number): the largest
//!   expected current, which sets the current resolution.
//!
//! The INA260 has an integrated 2 mΩ shunt, so its shunt and current settings are
//! ignored.
//!
//! Binding does not touch the bus. The chip is verified and calibrated on the
//! first read, and again after any read fails, so a chip that is absent at bind,
//! or that resets, is picked up by a later read. Until then health reports
//! `Degraded`. The driver never gives up and never switches itself off. It makes
//! no retries of its own, so pacing between attempts (capped at 8 s) is the
//! host's job, done by how often it reads.
//!
//! On a device the board gives to the host (`owner = "host"`) the operating
//! system's driver runs the chip, and this one only reads it: the first read
//! verifies the chip and reads its shunt range (INA238), writing nothing but
//! register addresses, so its configuration and calibration stay the owner's.
//! Current and power then come from the shunt voltage and the board's
//! `shunt_*` setting rather than from the chip's calibrated registers, which
//! are scaled by a calibration this driver didn't choose. The INA260's are
//! fixed, and read as they are.

#![no_std]

pub mod chip;

#[cfg(test)]
mod tests;

use chip::{Calibration, DEFAULT_ADDRESS, Model};
use lemnos_contract_power_monitor::{PowerMonitor, PowerReading};
use lemnos_core::{
    AddressWidth, Config, Description, Health, I2c, Item, Lifecycle, MethodId, Result, Status,
    Unit, driver, status_of,
};
use lemnos_hal::HalError;

const READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};

#[driver("ina2xx", provides = [PowerMonitor])]
pub struct Ina2xx {
    #[needs]
    bus: I2c,
    chip: chip::Ina,
    verified: bool,
    /// The operating system's driver runs the chip: read it, never set it up.
    host_owned: bool,
}

impl Lifecycle for Ina2xx {
    type Needs = Ina2xxNeeds;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;
    // A one-byte register pointer moves the chip's pointer and nothing else.
    const I2C_REGISTER_ADDRESS: Option<AddressWidth> = Some(AddressWidth::Bits8);

    fn start(needs: Ina2xxNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            bus: needs.bus,
            chip: chip_from_config(cfg)?,
            verified: false,
            host_owned: cfg.host_owned(),
        })
    }

    fn describe(&self, out: &mut Description) {
        // Every span is in nano-units (the register scale of the chip, see `Setup::spans`).
        let spans = self.chip.setup.spans(self.chip.attached());
        out.set_address(self.chip.address());
        let bus = Item::value("bus_voltage", READ)
            .unit(Unit::Volt, -9)
            .field("bus_v")
            .range(0, spans.bus_max_nv)
            .step(spans.bus_lsb_nv);
        out.add(bus);
        let shunt = Item::value("shunt_voltage", READ)
            .unit(Unit::Volt, -9)
            .field("shunt_v");
        out.add(match (spans.shunt_max_nv, spans.shunt_lsb_nv) {
            (Some(max), Some(step)) => shunt.range(-max, max).step(step),
            _ => shunt,
        });
        out.add(
            Item::value("current", READ)
                .unit(Unit::Ampere, -9)
                .field("current_a")
                .range(-spans.current_max_na, spans.current_max_na)
                .step(spans.current_lsb_na),
        );
        out.add(
            Item::value("power", READ)
                .unit(Unit::Watt, -9)
                .field("power_w")
                .range(0, spans.power_max_nw)
                .step(spans.power_lsb_nw.max(1)),
        );
    }

    fn health(&self) -> Health {
        if self.verified {
            Health::Ok
        } else {
            Health::Degraded("chip not verified: retried on each read")
        }
    }
}

impl PowerMonitor for Ina2xx {
    fn read(&mut self) -> Result<PowerReading> {
        if !self.verified {
            let verified = if self.host_owned {
                self.chip.attach(&mut self.bus)
            } else {
                self.chip.init(&mut self.bus)
            };
            verified.map_err(|error| status_of(error.kind()))?;
            self.verified = true;
        }
        match self.chip.read_fixed(&mut self.bus) {
            Ok(raw) => Ok(power_reading(raw)),
            Err(error) => {
                // A failed transfer may mean a reset: verify and calibrate again
                // on the next read.
                self.verified = false;
                Err(status_of(error.kind()))
            }
        }
    }
}

fn power_reading(raw: chip::ReadingFixed) -> PowerReading {
    PowerReading {
        bus_v: raw.bus_voltage_uv as f32 * 1e-6,
        shunt_v: raw.shunt_voltage_nv as f32 * 1e-9,
        current_a: raw.current_na as f32 * 1e-9,
        power_w: raw.power_nw as f32 * 1e-9,
    }
}

/// Builds the chip from the board file's settings. Anything missing or out of
/// range is `INVALID_ARGUMENT`.
fn chip_from_config(cfg: &Config<'_>) -> Result<chip::Ina> {
    let model = match cfg.str("chip") {
        Some(name) if name.eq_ignore_ascii_case("ina226") => Model::Ina226,
        Some(name) if name.eq_ignore_ascii_case("ina238") => Model::Ina238,
        Some(name) if name.eq_ignore_ascii_case("ina260") => Model::Ina260,
        _ => return Err(Status::INVALID_ARGUMENT),
    };
    let address = match cfg.int("address") {
        None => DEFAULT_ADDRESS,
        Some(address) => u8::try_from(address)
            .ok()
            .filter(|address| *address <= 0x7f)
            .ok_or(Status::INVALID_ARGUMENT)?,
    };
    // A missing or unusable value becomes 0, which the chip rejects.
    let shunt_micro_ohms = match cfg.int("shunt_micro_ohms") {
        Some(value) => u32::try_from(value).unwrap_or(0),
        None => micro(cfg.float("shunt_ohms").unwrap_or(0.0)),
    };
    let max_current_micro_amps = match cfg.int("max_current_micro_amps") {
        Some(value) => u32::try_from(value).unwrap_or(0),
        None => micro(cfg.float("max_current_a").unwrap_or(0.0)),
    };
    chip::Ina::new(
        model,
        address,
        Calibration::from_micro(shunt_micro_ohms, max_current_micro_amps),
    )
    .map_err(|_| Status::INVALID_ARGUMENT)
}

/// A value in its base unit, as millionths. NaN and negative values become 0.
fn micro(value: f64) -> u32 {
    (value * 1e6 + 0.5) as u32
}
