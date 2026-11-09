//! A Linux hwmon fan (`pwm1`, `pwm1_enable`, `fan1_input`) served as `fan/1`. Duty is per
//! mille, 0 to 1000.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `hwmon`: the name of the hwmon directory, such as `"hwmon2"`. Or
//! - `name`: the hwmon `name` attribute to match, such as `"pwmfan"`. With neither, the first hwmon
//!   device that has a `pwm1` is used.
//! - `sys_root`: where sysfs is mounted, `"/sys"` by default. Tests point it at a fake tree.
//!
//! A missing fan fails the bind with `NOT_FOUND`, and the host retries the bind later.
//!
//! The first `set_duty` takes the fan over. Before its first write it saves `pwm1`, `pwm1_enable`
//! and, for a fan that the thermal governor drives through a cooling device, the governor's state
//! (see [`hand_back`]). It then sets manual mode and writes `pwm1`; the kernel applies the duty only
//! in manual mode. The saved state is put back on `release`, and when the driver is dropped. The
//! host drops a driver when its device stops or is rebound, and when a call into the driver
//! panics. Nothing else gives the fan back: a process killed with `SIGKILL` runs no code, so the
//! fan keeps the duty it was last given.
//!
//! Failure rule: an attribute that cannot be read or written fails that call and is tried again
//! on the next one. The driver never switches itself off.

mod hand_back;
mod sysfs;

#[cfg(test)]
mod tests;

use hand_back::Saved;
use lemnos_contract_fan::Fan;
use lemnos_core::{
    Config, Description, Health, Item, Lifecycle, MethodId, Result, Status, Unit, driver,
};
use std::path::{Path, PathBuf};

/// The largest raw `pwm1` value.
pub const PWM_MAX: u32 = 255;
/// `pwm1_enable` for manual control, the only mode in which `pwm1` sets the duty.
const MODE_MANUAL: i32 = 1;
/// The largest duty, per mille.
const MAX_DUTY: u16 = 1000;

/// Method numbers in the `fan/1` contract.
const SET_DUTY: MethodId = MethodId {
    contract: 0,
    method: 1,
};
const DUTY: MethodId = MethodId {
    contract: 0,
    method: 2,
};
const RPM: MethodId = MethodId {
    contract: 0,
    method: 3,
};
const RELEASE: MethodId = MethodId {
    contract: 0,
    method: 4,
};

/// A raw `pwm1` value as per mille, rounded.
pub fn pwm_to_duty(pwm: u32) -> u16 {
    ((pwm.min(PWM_MAX) * 1000 + PWM_MAX / 2) / PWM_MAX) as u16
}

/// Per mille as a raw `pwm1` value, rounded. Values above 1000 are clamped.
pub fn duty_to_pwm(duty: u16) -> u32 {
    (u32::from(duty.min(MAX_DUTY)) * PWM_MAX + 500) / 1000
}

#[driver("hwmon-fan", provides = [Fan])]
pub struct HwmonFan {
    /// The hwmon directory's attribute paths, built once at bind.
    pwm: PathBuf,
    enable: PathBuf,
    rpm: PathBuf,
    /// The hwmon directory, and `/sys/class/thermal` for the cooling devices.
    fan: PathBuf,
    thermal: PathBuf,
    /// What the kernel had before the first write. `Some` from the first `set_duty` until the
    /// fan is handed back.
    saved: Option<Saved>,
    /// Whether `pwm1_enable` is known to be manual since the takeover.
    manual: bool,
    /// Whether the last call failed.
    failing: bool,
}

impl Lifecycle for HwmonFan {
    type Needs = HwmonFanNeeds;

    fn start(_needs: HwmonFanNeeds, cfg: &Config<'_>) -> Result<Self> {
        let sys = Path::new(cfg.str("sys_root").unwrap_or("/sys"));
        let hwmon = sys.join("class/hwmon");
        let fan = match cfg.str("hwmon") {
            Some(dir) => {
                if dir.is_empty() || dir.contains('/') {
                    return Err(Status::INVALID_ARGUMENT);
                }
                let fan = hwmon.join(dir);
                if !fan.join("pwm1").exists() {
                    return Err(Status::NOT_FOUND);
                }
                fan
            }
            None => find(&hwmon, cfg.str("name"))?.ok_or(Status::NOT_FOUND)?,
        };
        Ok(Self::new(fan, sys.join("class/thermal")))
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::value("rpm", RPM).unit(Unit::RevolutionPerMinute, 0));
        out.add(Item::value("duty", DUTY).unit(Unit::One, -3));
        out.add(
            Item::control("duty", SET_DUTY)
                .unit(Unit::One, -3)
                .range(0, i64::from(MAX_DUTY)),
        );
        out.add(Item::action("release", RELEASE));
    }

    fn health(&self) -> Health {
        if self.failing {
            Health::Degraded("an attribute is unavailable: retried on the next call")
        } else {
            Health::Ok
        }
    }
}

impl HwmonFan {
    fn new(fan: PathBuf, thermal: PathBuf) -> Self {
        Self {
            pwm: fan.join("pwm1"),
            enable: fan.join("pwm1_enable"),
            rpm: fan.join("fan1_input"),
            fan,
            thermal,
            saved: None,
            manual: false,
            failing: false,
        }
    }

    fn note<T>(&mut self, result: Result<T>) -> Result<T> {
        self.failing = result.is_err();
        result
    }

    /// Takes the fan over if it is not yet, and sets its duty.
    fn drive(&mut self, per_mille: u16) -> Result<()> {
        if self.saved.is_none() {
            self.saved = Some(Saved::capture(
                &self.fan,
                &self.thermal,
                &self.pwm,
                &self.enable,
            )?);
        }
        if !self.manual {
            sysfs::write_number(&self.enable, MODE_MANUAL)?;
            self.manual = true;
        }
        sysfs::write_number(&self.pwm, duty_to_pwm(per_mille))
    }

    /// Puts the saved state back. Nothing is kept if this succeeds. On failure the saved state
    /// stays, so the next attempt tries the same restore again.
    fn hand_back(&mut self) -> Result<()> {
        let Some(saved) = &self.saved else {
            return Ok(());
        };
        saved.restore(&self.pwm, &self.enable)?;
        self.saved = None;
        self.manual = false;
        Ok(())
    }
}

impl Fan for HwmonFan {
    fn set_duty(&mut self, per_mille: u16) -> Result<()> {
        if per_mille > MAX_DUTY {
            return Err(Status::INVALID_ARGUMENT);
        }
        let result = self.drive(per_mille);
        self.note(result)
    }

    fn duty(&mut self) -> Result<u16> {
        let result = sysfs::read_number::<u32>(&self.pwm).map(pwm_to_duty);
        self.note(result)
    }

    fn rpm(&mut self) -> Result<Option<u32>> {
        let result = sysfs::read_number_optional(&self.rpm);
        self.note(result)
    }

    fn release(&mut self) -> Result<()> {
        let result = self.hand_back();
        self.note(result)
    }
}

impl Drop for HwmonFan {
    fn drop(&mut self) {
        // Nothing can report an error from here, so a failed restore is dropped. The saved
        // state is gone with the driver.
        let _ = self.hand_back();
    }
}

/// The first hwmon directory under `hwmon` that has a `pwm1` and, if `name` is given, a `name`
/// attribute equal to it.
fn find(hwmon: &Path, name: Option<&str>) -> Result<Option<PathBuf>> {
    for entry in sysfs::entries(hwmon)? {
        if !entry.join("pwm1").exists() {
            continue;
        }
        let found = sysfs::read_text_optional(&entry.join("name"))?;
        if name.is_none_or(|name| found.as_deref() == Some(name)) {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}
