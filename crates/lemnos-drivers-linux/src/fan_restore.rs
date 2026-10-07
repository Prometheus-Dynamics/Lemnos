//! Handing a hwmon fan back to the kernel when a userspace controller stops.
//!
//! There are two kinds of fan:
//!
//! - **Fans driven by a thermal cooling device**, such as Linux's `pwm-fan`.
//!   `pwm-fan` has no automatic `pwm1_enable` mode: 0 disables the PWM (full
//!   speed), 1 is the normal enabled state, and 2 keeps the supply regulator
//!   on. The thermal governor drives the fan through the cooling device
//!   (`/sys/class/thermal/cooling_deviceN`, type `pwm-fan`), whatever
//!   `pwm1_enable` says. Writing `pwm1` also moves the cooling device's
//!   `cur_state` to the matching level, so the state at stop is the
//!   controller's, not the governor's. The hand-back therefore:
//!   1. restores the `pwm1_enable` value read when the controller bound the
//!      fan;
//!   2. writes back the governor's `cur_state` recorded just before the
//!      controller's first write (`pwm-fan` ignores a write of the state it
//!      already has, so when they are equal a neighbouring state goes
//!      first), and the driver re-emits `cooling-levels[state]`;
//!   3. makes the governor re-evaluate now, by writing each thermal zone
//!      bound to the cooling device its own `policy` back (which rebinds the
//!      governor and runs an update). Without this a zone with no polling
//!      (`step_wise`, empty `polling_delay`) only re-evaluates on its next
//!      trip crossing.
//!
//!   Without a record (a stop helper after a crash before the first write
//!   was recorded), step 2 is skipped.
//! - **Fans with a true automatic mode** (most fan-controller chips): the
//!   hand-back writes that `pwm1_enable` mode (2 unless the board says
//!   otherwise).
//!
//! A fan counts as cooling-device driven when its driver is `pwm-fan` or a
//! thermal cooling device is linked to its device.

use crate::{HwmonFan, SysfsError, sysfs};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// The driver whose fans are driven through a thermal cooling device, and the
/// type of the cooling device it registers.
pub const PWM_FAN_DRIVER: &str = "pwm-fan";

/// How a fan goes back to the kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreKind {
    /// Write this `pwm1_enable` mode, the chip's automatic control.
    Automatic { mode: i32 },
    /// Write `pwm1_enable = enable`, restore each cooling device's recorded
    /// governor state, and make the zones bound to it re-evaluate.
    CoolingDevice {
        enable: i32,
        devices: Vec<CoolingRecord>,
    },
}

/// A cooling device and the governor's state recorded before a controller
/// took the fan over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoolingRecord {
    /// `/sys/class/thermal/cooling_deviceN`.
    pub device: PathBuf,
    /// The governor's `cur_state`, if recorded.
    pub state: Option<u32>,
}

impl CoolingRecord {
    /// Records the device's current state.
    pub fn read(device: impl Into<PathBuf>) -> Result<Self, SysfsError> {
        let device = device.into();
        let state = sysfs::read_parsed(&device.join("cur_state"))?;
        Ok(Self {
            device,
            state: Some(state),
        })
    }

    /// A device without a recorded state.
    pub fn unrecorded(device: impl Into<PathBuf>) -> Self {
        Self {
            device: device.into(),
            state: None,
        }
    }
}

/// One fan's hand-back plan, worked out when the fan is bound (so it holds
/// the `pwm1_enable` value from before the controller changed anything).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanRestore {
    /// The fan's hwmon class directory.
    pub fan: PathBuf,
    pub kind: RestoreKind,
}

/// What a [`FanRestore::apply`] did to one cooling device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoolingNudge {
    /// The recorded governor state written back, if there was a record.
    pub state: Option<u32>,
    /// The neighbouring state written first (the device was already at the
    /// recorded state).
    pub via: Option<u32>,
    /// Thermal zones bound to the device whose governor was made to
    /// re-evaluate.
    pub zones: u32,
}

impl HwmonFan {
    /// The kernel driver bound to the fan's device (`device/driver`), such as
    /// `pwm-fan`.
    pub fn driver(&self) -> Option<String> {
        let link = fs::read_link(self.root().join("device/driver")).ok()?;
        Some(link.file_name()?.to_string_lossy().into_owned())
    }

    /// The thermal cooling devices under `thermal_root`
    /// (`/sys/class/thermal`) that belong to this fan: those linked to its
    /// device, else, for a `pwm-fan` fan, every cooling device of type
    /// `pwm-fan` (re-applying another fan's state is harmless).
    pub fn cooling_devices(&self, thermal_root: &Path) -> Result<Vec<PathBuf>, SysfsError> {
        let fan_device = fs::canonicalize(self.root().join("device")).ok();
        let mut linked = Vec::new();
        let mut typed = Vec::new();
        for entry in sysfs::entries(thermal_root)? {
            let is_cooling = entry
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("cooling_device"));
            if !is_cooling {
                continue;
            }
            if let Some(fan_device) = &fan_device {
                let parent = fs::canonicalize(entry.join("device")).ok();
                let resolved = fs::canonicalize(&entry).ok();
                if parent.as_ref() == Some(fan_device)
                    || resolved.is_some_and(|p| p.starts_with(fan_device))
                {
                    linked.push(entry);
                    continue;
                }
            }
            if sysfs::read_optional(&entry.join("type"))?.as_deref() == Some(PWM_FAN_DRIVER) {
                typed.push(entry);
            }
        }
        if !linked.is_empty() {
            return Ok(linked);
        }
        if self.driver().as_deref() == Some(PWM_FAN_DRIVER) {
            return Ok(typed);
        }
        Ok(Vec::new())
    }

    /// The hand-back plan for this fan, read now: for a cooling-device fan,
    /// the current `pwm1_enable` and the governor's state of its cooling
    /// devices; otherwise `automatic_mode`. Call it when binding the fan, and
    /// [`FanRestore::record_states`] again just before the first write.
    pub fn restore_plan(
        &self,
        thermal_root: &Path,
        automatic_mode: i32,
    ) -> Result<FanRestore, SysfsError> {
        let enable = self.mode()?;
        let mut plan = self.restore_plan_with(thermal_root, automatic_mode, enable)?;
        plan.record_states()?;
        Ok(plan)
    }

    /// [`Self::restore_plan`] with a known `pwm1_enable` and no recorded
    /// governor states, for a stop helper without the controller's record
    /// (it uses [`MODE_MANUAL`](crate::MODE_MANUAL), `pwm-fan`'s boot
    /// default).
    pub fn restore_plan_with(
        &self,
        thermal_root: &Path,
        automatic_mode: i32,
        enable: i32,
    ) -> Result<FanRestore, SysfsError> {
        let devices = self.cooling_devices(thermal_root)?;
        let cooling = !devices.is_empty() || self.driver().as_deref() == Some(PWM_FAN_DRIVER);
        let kind = if cooling {
            RestoreKind::CoolingDevice {
                enable,
                devices: devices.into_iter().map(CoolingRecord::unrecorded).collect(),
            }
        } else {
            RestoreKind::Automatic {
                mode: automatic_mode,
            }
        };
        Ok(FanRestore {
            fan: self.root().to_path_buf(),
            kind,
        })
    }
}

impl FanRestore {
    /// Records the governor's current state of each cooling device: call it
    /// just before the controller's first write to the fan (writing `pwm1`
    /// moves `cur_state`).
    pub fn record_states(&mut self) -> Result<(), SysfsError> {
        if let RestoreKind::CoolingDevice { devices, .. } = &mut self.kind {
            for record in devices {
                *record = CoolingRecord::read(&record.device)?;
            }
        }
        Ok(())
    }

    /// Hands the fan back. Every step is attempted; the first error is
    /// returned.
    pub fn apply(&self) -> Result<Vec<CoolingNudge>, SysfsError> {
        let fan = HwmonFan::new(&self.fan);
        match &self.kind {
            RestoreKind::Automatic { mode } => fan.set_mode(*mode).map(|()| Vec::new()),
            RestoreKind::CoolingDevice { enable, devices } => {
                let mut first = fan.set_mode(*enable).err();
                let mut nudges = Vec::new();
                for record in devices {
                    match restore_cooling_device(record) {
                        Ok(nudge) => nudges.push(nudge),
                        Err(error) => {
                            first.get_or_insert(error);
                        }
                    }
                }
                first.map_or(Ok(nudges), Err)
            }
        }
    }

    /// The plan as one line (`automatic <mode> <fan>` or
    /// `cooling <enable> <fan> <device>[=<state>]...`, tab separated), for a
    /// state file a stop helper reads.
    pub fn to_line(&self) -> String {
        match &self.kind {
            RestoreKind::Automatic { mode } => {
                format!("automatic\t{mode}\t{}", self.fan.display())
            }
            RestoreKind::CoolingDevice { enable, devices } => {
                let mut line = format!("cooling\t{enable}\t{}", self.fan.display());
                for record in devices {
                    line.push('\t');
                    line.push_str(&record.device.display().to_string());
                    if let Some(state) = record.state {
                        line.push_str(&format!("={state}"));
                    }
                }
                line
            }
        }
    }

    /// Parses [`Self::to_line`].
    pub fn from_line(line: &str) -> Option<Self> {
        let mut fields = line.trim_end_matches('\n').split('\t');
        let kind = fields.next()?;
        let value: i32 = fields.next()?.parse().ok()?;
        let fan = PathBuf::from(fields.next().filter(|f| !f.is_empty())?);
        let kind = match kind {
            "automatic" => RestoreKind::Automatic { mode: value },
            "cooling" => RestoreKind::CoolingDevice {
                enable: value,
                devices: fields
                    .map(|field| match field.rsplit_once('=') {
                        Some((device, state)) => state.parse().ok().map(|state| CoolingRecord {
                            device: PathBuf::from(device),
                            state: Some(state),
                        }),
                        None => Some(CoolingRecord::unrecorded(field)),
                    })
                    .collect::<Option<Vec<_>>>()?,
            },
            _ => return None,
        };
        Some(Self { fan, kind })
    }
}

impl fmt::Display for FanRestore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            RestoreKind::Automatic { mode } => {
                write!(f, "{}: pwm1_enable={mode}", self.fan.display())
            }
            RestoreKind::CoolingDevice { enable, devices } => write!(
                f,
                "{}: pwm1_enable={enable}, {} cooling device(s) handed back",
                self.fan.display(),
                devices.len()
            ),
        }
    }
}

/// Hands one cooling device back to its governor: writes the recorded
/// state (after a neighbouring one when the device is already there, so the
/// driver re-emits it), then makes every thermal zone bound to the device
/// re-evaluate ([`kick_governors`]).
pub fn restore_cooling_device(record: &CoolingRecord) -> Result<CoolingNudge, SysfsError> {
    let path = record.device.join("cur_state");
    let mut via = None;
    if let Some(state) = record.state {
        let current: u32 = sysfs::read_parsed(&path)?;
        if current == state {
            let max: u32 =
                sysfs::read_parsed_optional(&record.device.join("max_state"))?.unwrap_or(0);
            via = if state > 0 {
                Some(state - 1)
            } else {
                (max > 0).then_some(1)
            };
            if let Some(via) = via {
                sysfs::write(&path, via)?;
            }
        }
        sysfs::write(&path, state)?;
    }
    let zones = kick_governors(&record.device)?;
    Ok(CoolingNudge {
        state: record.state,
        via,
        zones,
    })
}

/// Makes the governor of every thermal zone bound to `device` re-evaluate
/// now: writes each zone's `policy` back to it, which rebinds the governor
/// and runs an update. Zones are siblings of the device
/// (`/sys/class/thermal/thermal_zoneN`) whose `cdevK` links resolve to it.
/// Returns how many zones were kicked.
pub fn kick_governors(device: &Path) -> Result<u32, SysfsError> {
    let Some(thermal_root) = device.parent() else {
        return Ok(0);
    };
    let target = fs::canonicalize(device).unwrap_or_else(|_| device.to_path_buf());
    let mut kicked = 0;
    for zone in sysfs::entries(thermal_root)? {
        let is_zone = zone
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("thermal_zone"));
        if !is_zone || !zone_binds(&zone, &target)? {
            continue;
        }
        let policy_path = zone.join("policy");
        if let Some(policy) = sysfs::read_optional(&policy_path)? {
            sysfs::write(&policy_path, &policy)?;
            kicked += 1;
        }
    }
    Ok(kicked)
}

/// Whether one of the zone's `cdevK` links resolves to `target`.
fn zone_binds(zone: &Path, target: &Path) -> Result<bool, SysfsError> {
    Ok(sysfs::entries(zone)?.iter().any(|entry| {
        let is_cdev_link = entry.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            n.strip_prefix("cdev")
                .is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))
        });
        is_cdev_link && fs::canonicalize(entry).is_ok_and(|p| p == target)
    }))
}
