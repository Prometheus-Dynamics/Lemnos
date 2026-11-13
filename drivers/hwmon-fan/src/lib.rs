//! A Linux hwmon fan (`pwm1`, `pwm1_enable`, `fan1_input`) served as `fan/1`. Duty is per
//! mille, 0 to 1000.
//!
//! Use this for a fan the kernel exposes through a hwmon device, such as one behind an embedded
//! controller. A fan on a PWM channel of the board is `pwm-channel-fan`, which needs no kernel
//! driver in between. The fan's safe state is full duty, set by the controller (`fan-control` runs it
//! at full duty on a stale input) and by this driver when it lets the fan go.
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
//! The first `set_duty` takes the fan over: it sets manual mode and writes `pwm1`. The kernel applies
//! the duty only in manual mode. `release`, and dropping the driver, put the fan at full duty and
//! leave it in manual mode. The fan is not handed back to the kernel's thermal governor: a driver
//! that stops leaves the hardware in its safe state, not under another controller. The host drops a
//! driver when its device stops or is rebound, and when a call into the driver panics. A process
//! killed with `SIGKILL` runs no code, so the fan keeps the duty it was last given.
//!
//! Failure rule: an attribute that cannot be read or written fails that call and is tried again
//! on the next one. The driver never switches itself off.

mod sysfs;

#[cfg(test)]
mod tests;

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
    /// Whether this driver has taken the fan: `pwm1_enable` is manual since its first write.
    manual: bool,
    /// Whether the last call failed.
    failing: bool,
    /// The operating system owns this fan: nothing is ever opened for writing.
    reads_only: bool,
}

impl Lifecycle for HwmonFan {
    type Needs = HwmonFanNeeds;
    // Reaches the kernel's files itself, and on a host-owned device only reads them.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ReadsOnly;

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
        let mut driver = Self::new(&fan);
        driver.reads_only = cfg.host_owned();
        Ok(driver)
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
    fn new(fan: &Path) -> Self {
        Self {
            pwm: fan.join("pwm1"),
            enable: fan.join("pwm1_enable"),
            rpm: fan.join("fan1_input"),
            manual: false,
            failing: false,
            reads_only: false,
        }
    }

    fn note<T>(&mut self, result: Result<T>) -> Result<T> {
        self.failing = result.is_err();
        result
    }

    /// Takes the fan over if it is not yet, and sets its duty.
    fn drive(&mut self, per_mille: u16) -> Result<()> {
        // Every write this driver makes starts here. On a host-owned fan none is made, so there
        // is never anything to put at the safe state either.
        if self.reads_only {
            return Err(Status::NOT_PERMITTED);
        }
        if !self.manual {
            sysfs::write_number(&self.enable, MODE_MANUAL)?;
            self.manual = true;
        }
        sysfs::write_number(&self.pwm, duty_to_pwm(per_mille))
    }

    /// Puts the fan at full duty, its safe state, if this driver has taken it. It stays in manual
    /// mode: nothing is handed to the kernel's governor. A failed write is tried again on the next
    /// release, or when the driver is dropped.
    fn put_at_safe_state(&mut self) -> Result<()> {
        if !self.manual {
            return Ok(());
        }
        sysfs::write_number(&self.pwm, PWM_MAX)
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
        let result = self.put_at_safe_state();
        self.note(result)
    }
}

impl Drop for HwmonFan {
    fn drop(&mut self) {
        // Nothing can report an error from here, so a failed write is dropped.
        let _ = self.put_at_safe_state();
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
