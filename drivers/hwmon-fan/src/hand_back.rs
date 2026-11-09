//! Putting a fan back the way the kernel had it.
//!
//! Before the driver's first write, [`Saved::capture`] records the fan's `pwm1` and
//! `pwm1_enable`. A fan the thermal governor drives through a cooling device (Linux's
//! `pwm-fan`, or any fan with a cooling device linked to it) also has each cooling device's
//! `cur_state` recorded. Writing `pwm1` moves that state, so [`Saved::restore`] writes the fan's own
//! values first and the governor's state after them. Then it makes each thermal zone bound to the
//! device re-evaluate by writing its `policy` back. Without that a zone that does not poll only
//! re-evaluates at its next trip crossing.

use crate::{PWM_MAX, sysfs};
use lemnos_core::{Result, Status};
use std::fs;
use std::path::{Path, PathBuf};

/// The kernel driver of a fan driven through a thermal cooling device.
pub const PWM_FAN_DRIVER: &str = "pwm-fan";

/// What the kernel had before the driver's first write.
pub struct Saved {
    pwm: u32,
    enable: i32,
    cooling: Vec<Cooling>,
}

struct Cooling {
    device: PathBuf,
    state: u32,
}

impl Saved {
    /// Records the state of a fan before it is taken over. `fan` is its hwmon directory and
    /// `thermal` is `/sys/class/thermal`. Fails, and so writes nothing, if an attribute cannot be
    /// read.
    pub fn capture(fan: &Path, thermal: &Path, pwm: &Path, enable: &Path) -> Result<Self> {
        let pwm_value = sysfs::read_number(pwm)?;
        let enable_value = sysfs::read_number(enable)?;
        let driver = driver_of(fan);
        let mut cooling = Vec::new();
        for device in cooling_devices(fan, thermal, driver.as_deref())? {
            let state = sysfs::read_number(&device.join("cur_state"))?;
            cooling.push(Cooling { device, state });
        }
        Ok(Self {
            pwm: pwm_value,
            enable: enable_value,
            cooling,
        })
    }

    /// Puts the fan back. Every step is tried, and the first error is returned. The saved
    /// values stay with the caller until this succeeds, so a later call can try again.
    pub fn restore(&self, pwm: &Path, enable: &Path) -> Result<()> {
        let mut first: Option<Status> = None;
        let mut note = |result: Result<()>| {
            if let Err(status) = result {
                first.get_or_insert(status);
            }
        };
        note(sysfs::write_number(pwm, self.pwm.min(PWM_MAX)));
        note(sysfs::write_number(enable, self.enable));
        for cooling in &self.cooling {
            note(restore_cooling(&cooling.device, cooling.state));
        }
        first.map_or(Ok(()), Err)
    }
}

/// Writes a cooling device's recorded state, then makes the zones bound to it re-evaluate.
fn restore_cooling(device: &Path, state: u32) -> Result<()> {
    let path = device.join("cur_state");
    if sysfs::read_number::<u32>(&path)? == state {
        // The driver ignores a write of the state it already shows, so a neighbouring state
        // goes first.
        let max: u32 = sysfs::read_number_optional(&device.join("max_state"))?.unwrap_or(0);
        let via = if state > 0 {
            Some(state - 1)
        } else if max > 0 {
            Some(1)
        } else {
            None
        };
        if let Some(via) = via {
            sysfs::write_number(&path, via)?;
        }
    }
    sysfs::write_number(&path, state)?;
    kick_governors(device)
}

/// Writes back the `policy` of every thermal zone bound to `device`, which rebinds the
/// governor and runs an update. Zones are the siblings of the device
/// (`/sys/class/thermal/thermal_zoneN`) with a `cdevK` link that resolves to it.
fn kick_governors(device: &Path) -> Result<()> {
    let Some(thermal) = device.parent() else {
        return Ok(());
    };
    let target = fs::canonicalize(device).unwrap_or_else(|_| device.to_path_buf());
    for zone in sysfs::entries(thermal)? {
        if !sysfs::named(&zone, "thermal_zone") || !zone_binds(&zone, &target)? {
            continue;
        }
        let policy = zone.join("policy");
        if let Some(text) = sysfs::read_text_optional(&policy)? {
            fs::write(&policy, text).map_err(|e| sysfs::io_status(&e))?;
        }
    }
    Ok(())
}

/// Whether one of the zone's `cdevK` links resolves to `target`.
fn zone_binds(zone: &Path, target: &Path) -> Result<bool> {
    for entry in sysfs::entries(zone)? {
        let is_cdev = entry.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .strip_prefix("cdev")
                .is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))
        });
        if is_cdev && fs::canonicalize(&entry).is_ok_and(|path| path == target) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The kernel driver bound to the fan's device (`device/driver`).
fn driver_of(fan: &Path) -> Option<String> {
    let link = fs::read_link(fan.join("device/driver")).ok()?;
    Some(link.file_name()?.to_string_lossy().into_owned())
}

/// The cooling devices that belong to the fan: those linked to its device, else, for a
/// `pwm-fan` fan, every cooling device of type `pwm-fan`.
fn cooling_devices(fan: &Path, thermal: &Path, driver: Option<&str>) -> Result<Vec<PathBuf>> {
    let fan_device = fs::canonicalize(fan.join("device")).ok();
    let mut linked = Vec::new();
    let mut typed = Vec::new();
    for entry in sysfs::entries(thermal)? {
        if !sysfs::named(&entry, "cooling_device") {
            continue;
        }
        if let Some(fan_device) = &fan_device {
            let parent = fs::canonicalize(entry.join("device")).ok();
            let resolved = fs::canonicalize(&entry).ok();
            if parent.as_ref() == Some(fan_device)
                || resolved.is_some_and(|path| path.starts_with(fan_device))
            {
                linked.push(entry);
                continue;
            }
        }
        if sysfs::read_text_optional(&entry.join("type"))?.as_deref() == Some(PWM_FAN_DRIVER) {
            typed.push(entry);
        }
    }
    if !linked.is_empty() {
        return Ok(linked);
    }
    if driver == Some(PWM_FAN_DRIVER) {
        return Ok(typed);
    }
    Ok(Vec::new())
}
