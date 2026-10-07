//! Handing a hwmon fan back to the kernel when a userspace controller stops.
//!
//! There are two kinds of fan:
//!
//! - **Fans driven by a thermal cooling device**, such as Linux's `pwm-fan`.
//!   `pwm-fan` has no automatic `pwm1_enable` mode: 0 disables the PWM (full
//!   speed), 1 is the normal enabled state, and 2 keeps the supply regulator
//!   on. The thermal governor drives the fan through the cooling device
//!   (`/sys/class/thermal/cooling_deviceN`, type `pwm-fan`), whatever
//!   `pwm1_enable` says. The hand-back restores the `pwm1_enable` value read
//!   when the controller bound the fan, then makes the driver re-apply the
//!   governor's level. `pwm-fan` ignores a write of the state it already
//!   has, so the hand-back writes a neighbouring state and then the current
//!   one, and the driver re-emits `cooling-levels[state]`.
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
    /// Write `pwm1_enable = enable`, then re-apply the governor's state of
    /// each cooling device.
    CoolingDevice { enable: i32, devices: Vec<PathBuf> },
}

/// One fan's hand-back plan, worked out when the fan is bound (so it holds
/// the `pwm1_enable` value from before the controller changed anything).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanRestore {
    /// The fan's hwmon class directory.
    pub fan: PathBuf,
    pub kind: RestoreKind,
}

/// What a [`FanRestore::apply`] wrote to one cooling device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoolingNudge {
    /// The governor's state, written last.
    pub state: u32,
    /// The neighbouring state written first, if the device has more than one.
    pub via: Option<u32>,
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
    /// the current `pwm1_enable` and its cooling devices; otherwise
    /// `automatic_mode`. Call it when binding the fan, before writing to it.
    pub fn restore_plan(
        &self,
        thermal_root: &Path,
        automatic_mode: i32,
    ) -> Result<FanRestore, SysfsError> {
        let enable = self.mode()?;
        self.restore_plan_with(thermal_root, automatic_mode, enable)
    }

    /// [`Self::restore_plan`] with a known `pwm1_enable` for the
    /// cooling-device case (a stop helper that runs after the controller is
    /// gone uses the controller's recorded value, else
    /// [`MODE_MANUAL`](crate::MODE_MANUAL), `pwm-fan`'s boot default).
    pub fn restore_plan_with(
        &self,
        thermal_root: &Path,
        automatic_mode: i32,
        enable: i32,
    ) -> Result<FanRestore, SysfsError> {
        let devices = self.cooling_devices(thermal_root)?;
        let cooling = !devices.is_empty() || self.driver().as_deref() == Some(PWM_FAN_DRIVER);
        let kind = if cooling {
            RestoreKind::CoolingDevice { enable, devices }
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
    /// Hands the fan back. Every step is attempted; the first error is
    /// returned.
    pub fn apply(&self) -> Result<Vec<CoolingNudge>, SysfsError> {
        let fan = HwmonFan::new(&self.fan);
        match &self.kind {
            RestoreKind::Automatic { mode } => fan.set_mode(*mode).map(|()| Vec::new()),
            RestoreKind::CoolingDevice { enable, devices } => {
                let mut first = fan.set_mode(*enable).err();
                let mut nudges = Vec::new();
                for device in devices {
                    match reapply_cooling_state(device) {
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
    /// `cooling <enable> <fan> <device>...`, tab separated), for a state file
    /// a stop helper reads.
    pub fn to_line(&self) -> String {
        match &self.kind {
            RestoreKind::Automatic { mode } => {
                format!("automatic\t{mode}\t{}", self.fan.display())
            }
            RestoreKind::CoolingDevice { enable, devices } => {
                let mut line = format!("cooling\t{enable}\t{}", self.fan.display());
                for device in devices {
                    line.push('\t');
                    line.push_str(&device.display().to_string());
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
                devices: fields.map(PathBuf::from).collect(),
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
                "{}: pwm1_enable={enable}, {} cooling device(s) re-applied",
                self.fan.display(),
                devices.len()
            ),
        }
    }
}

/// Makes a cooling device's driver re-apply the governor's state: writes a
/// neighbouring state, then the current one.
pub fn reapply_cooling_state(device: &Path) -> Result<CoolingNudge, SysfsError> {
    let path = device.join("cur_state");
    let state: u32 = sysfs::read_parsed(&path)?;
    let max: u32 = sysfs::read_parsed_optional(&device.join("max_state"))?.unwrap_or(0);
    let via = if state > 0 {
        Some(state - 1)
    } else {
        (max > 0).then_some(1)
    };
    if let Some(via) = via {
        sysfs::write(&path, via)?;
    }
    sysfs::write(&path, state)?;
    Ok(CoolingNudge { state, via })
}
