//! The BMM150 geomagnetic sensor as a magnetometer, over an I2C bus.
//!
//! The first read after bind powers the chip up and returns `AGAIN`. Each
//! later read checks the chip, reads its factory trim, starts normal mode,
//! and returns the field in tesla. The chip needs about 3 ms after power-up;
//! the host's next read provides that gap, so the driver never sleeps.
//!
//! Any bus or chip fault puts the chip back to power-up on the next read. The
//! driver never gives up: it reports the fault, and the host decides how long
//! to back off.
//!
//! An axis the chip cannot measure (overflow, or a trim that cannot compensate
//! it) reads as NaN.

#![no_std]

mod chip;
#[cfg(test)]
mod tests;

pub use chip::{DataRate, Preset};

use chip::{Fault, Settings, Trim};
use lemnos_contract_magnetometer::Magnetometer;
use lemnos_core::{
    Config, ConfigValue, Description, Health, I2c, Item, Lifecycle, MethodId, Result, Status, Unit,
    Vector3, driver,
};

/// The 7-bit address with SDO and CSB2 low; 0x11-0x13 are the alternatives.
pub const DEFAULT_ADDRESS: u8 = 0x10;

const READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// Where the chip is in its power-up sequence.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// Powered down, or the last operation failed.
    Off,
    /// Power is on; the next read starts the chip up.
    Starting,
    /// Measuring, with the trim the chip reported at start-up.
    Ready(Trim),
}

#[driver("bmm150", provides = [Magnetometer])]
pub struct Bmm150 {
    #[needs]
    bus: I2c,
    address: u8,
    settings: Settings,
    phase: Phase,
    /// The last conversion, repeated by a read with no new one.
    last: Option<Vector3>,
    /// Why the chip is not measuring, while it is not.
    fault: &'static str,
}

impl Lifecycle for Bmm150 {
    type Needs = Bmm150Needs;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;

    fn start(needs: Bmm150Needs, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            bus: needs.bus,
            address: address(cfg)?,
            settings: settings(cfg)?,
            phase: Phase::Off,
            last: None,
            fault: "not started",
        })
    }

    fn describe(&self, out: &mut Description) {
        for (name, field) in [
            ("magnetic_field.x", "x"),
            ("magnetic_field.y", "y"),
            ("magnetic_field.z", "z"),
        ] {
            out.add(Item::value(name, READ).unit(Unit::Tesla, 0).field(field));
        }
    }

    fn health(&self) -> Health {
        match self.phase {
            Phase::Ready(_) => Health::Ok,
            Phase::Starting => Health::Degraded("powering up"),
            Phase::Off => Health::Failed(self.fault),
        }
    }
}

impl Bmm150 {
    /// Records a fault, so the next read powers the chip up again.
    fn fail(&mut self, fault: Fault) -> Status {
        self.phase = Phase::Off;
        self.fault = fault.reason();
        fault.status()
    }
}

impl Magnetometer for Bmm150 {
    fn read(&mut self) -> Result<Vector3> {
        let trim = match self.phase {
            Phase::Off => {
                if let Err(fault) = chip::power_on(&mut self.bus, self.address) {
                    return Err(self.fail(fault));
                }
                self.phase = Phase::Starting;
                return Err(Status::AGAIN);
            }
            Phase::Starting => match chip::start_up(&mut self.bus, self.address, self.settings) {
                Ok(trim) => {
                    self.phase = Phase::Ready(trim);
                    trim
                }
                Err(fault) => return Err(self.fail(fault)),
            },
            Phase::Ready(trim) => trim,
        };
        let raw = match chip::read_raw(&mut self.bus, self.address) {
            Ok(raw) => raw,
            Err(fault) => return Err(self.fail(fault)),
        };
        if !raw.data_ready {
            return self.last.ok_or(Status::AGAIN);
        }
        let field = trim.compensate_fixed(raw).to_vector();
        self.last = Some(field);
        Ok(field)
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

/// The `preset` (a name) and `data_rate` (whole hertz) config.
fn settings(cfg: &Config<'_>) -> Result<Settings> {
    let preset = match cfg.get("preset") {
        None => Preset::default(),
        Some(ConfigValue::Str(name)) => Preset::from_name(name).ok_or(Status::INVALID_ARGUMENT)?,
        Some(_) => return Err(Status::INVALID_ARGUMENT),
    };
    let data_rate = match cfg.get("data_rate") {
        None => DataRate::default(),
        Some(ConfigValue::Int(hz)) => DataRate::from_hz(hz).ok_or(Status::INVALID_ARGUMENT)?,
        Some(_) => return Err(Status::INVALID_ARGUMENT),
    };
    Ok(Settings { preset, data_rate })
}
