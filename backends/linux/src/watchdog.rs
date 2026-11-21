//! A hardware watchdog as a resource: the node `/dev/watchdogN` opened for writing, its timeout
//! requests on the descriptor, and the sysfs attributes beside it.
//!
//! Feeding writes one NUL byte. The magic close character `V` is never written, so a watchdog
//! opened here is never disarmed by it: dropping the handle only stops the feeding.
//!
//! Held across the drop: lemnosd's root phase opens a keep-alive device's watchdog as root and feeds
//! it once ([`hold`]), so the unfed hand-over from the initramfs ends at the root phase and the node's
//! owner (root 0600 until udev applies the rule) does not matter. The held descriptor stays for the
//! daemon's life; each bind gets a copy of it (the same open file), so a rebind after the drop needs
//! no access to the node by name either. Dropping a copy closes nothing the device sees.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lemnos_hal::ErrorKind;
use lemnos_hal::raw::RawWatchdog;

use crate::Claim;

/// The byte that feeds a watchdog. Any byte but the magic close character `V` feeds it.
const FEED: u8 = 0;

/// The nodes held across the drop, by path: each opened once, as root, by [`hold`].
static HELD: Mutex<Vec<(PathBuf, File)>> = Mutex::new(Vec::new());

/// Opens `node` for writing as the root phase runs (close-on-exec, as std opens every file) and
/// feeds it once, then keeps the descriptor for the binds after the drop. A node already held is fed
/// again and kept once.
///
/// # Errors
/// The open or the feed's error.
pub(crate) fn hold(node: &Path) -> std::io::Result<()> {
    let mut held = HELD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, file)) = held.iter_mut().find(|(path, _)| path == node) {
        return feed_once(file);
    }
    let mut file = OpenOptions::new().write(true).open(node)?;
    feed_once(&mut file)?;
    held.push((node.to_path_buf(), file));
    Ok(())
}

fn feed_once(file: &mut File) -> std::io::Result<()> {
    match file.write(&[FEED])? {
        1 => Ok(()),
        _ => Err(std::io::Error::from(std::io::ErrorKind::WriteZero)),
    }
}

/// A copy of the held descriptor for `node`, when the root phase held it.
fn held(node: &Path) -> Option<std::io::Result<File>> {
    let held = HELD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    held.iter()
        .find(|(path, _)| path == node)
        .map(|(_, file)| file.try_clone())
}

/// An open watchdog node, with its sysfs directory and the claim that keeps one owner on it.
pub(crate) struct LinuxWatchdog {
    file: File,
    sys_dir: PathBuf,
    _claim: Claim,
}

impl LinuxWatchdog {
    /// Opens `node` for writing, arming nothing: the timeout is set by the first request. A node the
    /// root phase held is not opened again: the bind takes a copy of the held descriptor.
    pub(crate) fn open(node: &Path, sys_dir: PathBuf, claim: Claim) -> std::io::Result<Self> {
        let file = match held(node) {
            Some(copy) => copy?,
            None => OpenOptions::new().write(true).open(node)?,
        };
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
