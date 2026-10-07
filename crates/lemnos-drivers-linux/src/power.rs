//! Regulators and clocks the kernel exposes to userspace, behind
//! `lemnos_hal::{Regulator, ClockOutput}`.
//!
//! - [`UserspaceRegulator`]: a supply wired to the kernel's
//!   `reg-userspace-consumer` driver, switched through its `state` attribute
//!   (`enabled`/`disabled`), with the voltage read from the regulator's
//!   `microvolts` when known.
//! - [`DebugfsClock`]: a clock's rate from debugfs
//!   (`/sys/kernel/debug/clk/<name>/clk_rate`). The kernel owns clock
//!   gating, so enabling and disabling are refused unless the clock is
//!   already in that state.

use crate::{SysfsError, sysfs};
use lemnos_hal::{ClockOutput, ErrorKind, Regulator};
use std::path::{Path, PathBuf};

impl From<ErrorKind> for SysfsError {
    fn from(kind: ErrorKind) -> Self {
        SysfsError::new(kind, PathBuf::new(), kind.to_string())
    }
}

/// A supply switched through a `reg-userspace-consumer` device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserspaceRegulator {
    consumer: PathBuf,
    regulator: Option<PathBuf>,
}

impl UserspaceRegulator {
    /// The consumer device's directory
    /// (`/sys/devices/platform/<consumer>`, the one with `state`).
    pub fn new(consumer: impl Into<PathBuf>) -> Self {
        Self {
            consumer: consumer.into(),
            regulator: None,
        }
    }

    /// Reads the voltage from the regulator's class directory
    /// (`/sys/class/regulator/regulator.N`).
    pub fn with_regulator(mut self, regulator: impl Into<PathBuf>) -> Self {
        self.regulator = Some(regulator.into());
        self
    }

    pub fn consumer(&self) -> &Path {
        &self.consumer
    }
}

impl Regulator for UserspaceRegulator {
    type Error = SysfsError;

    fn enable(&mut self) -> Result<(), SysfsError> {
        sysfs::write(&self.consumer.join("state"), "enabled")
    }

    fn disable(&mut self) -> Result<(), SysfsError> {
        sysfs::write(&self.consumer.join("state"), "disabled")
    }

    fn is_enabled(&mut self) -> Result<bool, SysfsError> {
        Ok(sysfs::read(&self.consumer.join("state"))? == "enabled")
    }

    fn voltage_uv(&mut self) -> Result<Option<u32>, SysfsError> {
        match &self.regulator {
            Some(dir) => sysfs::read_parsed_optional(&dir.join("microvolts")),
            None => Ok(None),
        }
    }
}

/// A clock's rate from debugfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugfsClock {
    dir: PathBuf,
}

impl DebugfsClock {
    /// The clock's debugfs directory (`/sys/kernel/debug/clk/<name>`).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The clock called `name` under `debugfs` (`/sys/kernel/debug`).
    pub fn named(debugfs: &Path, name: &str) -> Self {
        Self::new(debugfs.join("clk").join(name))
    }

    fn enabled(&self) -> Result<bool, SysfsError> {
        Ok(sysfs::read_parsed::<u32>(&self.dir.join("clk_enable_count"))? > 0)
    }
}

impl ClockOutput for DebugfsClock {
    type Error = SysfsError;

    /// Succeeds only if the kernel already runs the clock.
    fn enable(&mut self) -> Result<(), SysfsError> {
        if self.enabled()? {
            Ok(())
        } else {
            Err(SysfsError::new(
                ErrorKind::Unsupported,
                &self.dir,
                "the kernel gates this clock; userspace cannot start it",
            ))
        }
    }

    /// Succeeds only if the clock is already stopped.
    fn disable(&mut self) -> Result<(), SysfsError> {
        if self.enabled()? {
            Err(SysfsError::new(
                ErrorKind::Unsupported,
                &self.dir,
                "the kernel gates this clock; userspace cannot stop it",
            ))
        } else {
            Ok(())
        }
    }

    fn rate_hz(&mut self) -> Result<u32, SysfsError> {
        let rate: u64 = sysfs::read_parsed(&self.dir.join("clk_rate"))?;
        u32::try_from(rate)
            .map_err(|_| SysfsError::new(ErrorKind::Unsupported, &self.dir, "rate above 4.29 GHz"))
    }
}
