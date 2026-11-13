//! A hardware watchdog as a resource: the node `/dev/watchdogN` opened for writing, its timeout
//! requests on the descriptor, and the sysfs attributes beside it.
//!
//! Feeding writes one NUL byte. The magic close character `V` is never written, so a watchdog
//! opened here is never disarmed by it: dropping the handle only stops the feeding.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use lemnos_hal::ErrorKind;
use lemnos_hal::raw::RawWatchdog;

use crate::Claim;

/// The byte that feeds a watchdog. Any byte but the magic close character `V` feeds it.
const FEED: u8 = 0;

/// An open watchdog node, with its sysfs directory and the claim that keeps one owner on it.
pub(crate) struct LinuxWatchdog {
    file: File,
    sys_dir: PathBuf,
    _claim: Claim,
}

impl LinuxWatchdog {
    /// Opens `node` for writing, arming nothing: the timeout is set by the first request.
    pub(crate) fn open(node: &Path, sys_dir: PathBuf, claim: Claim) -> std::io::Result<Self> {
        let file = OpenOptions::new().write(true).open(node)?;
        Ok(Self {
            file,
            sys_dir,
            _claim: claim,
        })
    }

    fn seconds(path: &Path) -> Option<u32> {
        fs::read_to_string(path).ok()?.trim().parse().ok()
    }
}

impl RawWatchdog for LinuxWatchdog {
    fn feed(&mut self) -> Result<(), ErrorKind> {
        match self.file.write(&[FEED]) {
            Ok(1) => Ok(()),
            _ => Err(ErrorKind::Failed),
        }
    }

    fn set_timeout(&mut self, seconds: u32) -> Result<u32, ErrorKind> {
        lemnos_driver_hw_watchdog_sys::set_timeout(self.file.as_fd(), seconds)
            .map_err(|_| ErrorKind::Failed)
    }

    fn timeout(&mut self) -> Result<u32, ErrorKind> {
        lemnos_driver_hw_watchdog_sys::get_timeout(self.file.as_fd()).map_err(|_| ErrorKind::Failed)
    }

    fn timeout_range(&mut self) -> (Option<u32>, Option<u32>) {
        (
            Self::seconds(&self.sys_dir.join("min_timeout")),
            Self::seconds(&self.sys_dir.join("max_timeout")),
        )
    }

    fn has_attribute(&mut self, name: &str) -> bool {
        self.sys_dir.join(name).exists()
    }

    fn time_left(&mut self) -> Option<u32> {
        Self::seconds(&self.sys_dir.join("timeleft"))
    }
}
