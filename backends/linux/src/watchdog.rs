//! A hardware watchdog as a resource: the node `/dev/watchdogN` opened for writing, its timeout
//! requests on the descriptor, and the sysfs attributes beside it.
//!
//! Feeding writes one NUL byte. The magic close character `V` is never written, so a watchdog
//! opened here is never disarmed by it: dropping the handle only stops the feeding.
//!
//! Held across the drop: lemnosd's root phase holds a keep-alive device's watchdog as root ([`hold`]),
//! so the unfed hand-over from the initramfs ends at the root phase and the node's owner (root 0600
//! until udev applies the rule) does not matter. The held descriptor stays for the daemon's life; each
//! bind gets a copy of it (the same open file), so a rebind after the drop needs no access to the node
//! by name either. Dropping a copy closes nothing the device sees.
//!
//! The hold has two forms. The first start opens the node and feeds it. A restart is handed the
//! descriptor the service manager kept in its file descriptor store, and keeps it as it is: an open
//! of a running watchdog feeds it, so a restart that opened the node by name would feed a daemon that
//! died before it was healthy, for good. A stored descriptor is taken only when it is a character
//! device with the node's own device number; any other is refused by the caller, by name.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lemnos_hal::ErrorKind;
use lemnos_hal::raw::RawWatchdog;

use crate::Claim;

/// The byte that feeds a watchdog. Any byte but the magic close character `V` feeds it.
const FEED: u8 = 0;

/// The nodes held across the drop, by path: each held once, by [`hold`].
static HELD: Mutex<Vec<(PathBuf, File)>> = Mutex::new(Vec::new());

/// What [`hold`] did with a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Held {
    /// The node was opened and fed. Its descriptor is the one to keep in the service's store.
    Fed,
    /// A stored descriptor for the node was taken as it is. Nothing was written to the node.
    Adopted,
}

/// Holds `node` across the drop, as the root phase runs (still root). A stored descriptor that is a
/// character device with the node's own device number is taken and kept as it is ([`Held::Adopted`]);
/// the first one that matches is taken out of `stored`. Otherwise the node is opened as root,
/// close-on-exec as std opens every file, fed once, and kept ([`Held::Fed`]). A node already held is
/// fed again and kept once.
///
/// # Errors
/// The open or the feed's error, when the node is opened.
pub(crate) fn hold(node: &Path, stored: &mut Vec<File>) -> std::io::Result<Held> {
    let mut held = HELD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    hold_in(&mut held, node, stored)
}

/// [`hold`] over the given list of held nodes, so the tests keep their own.
fn hold_in(
    held: &mut Vec<(PathBuf, File)>,
    node: &Path,
    stored: &mut Vec<File>,
) -> std::io::Result<Held> {
    if let Some((_, file)) = held.iter_mut().find(|(path, _)| path == node) {
        feed_once(file)?;
        return Ok(Held::Fed);
    }
    let want = fs::metadata(node)
        .ok()
        .filter(|meta| meta.file_type().is_char_device())
        .map(|meta| meta.rdev());
    if let Some(at) = stored.iter().position(|file| is_node(file, want)) {
        let file = stored.swap_remove(at);
        held.push((node.to_path_buf(), file));
        return Ok(Held::Adopted);
    }
    let mut file = OpenOptions::new().write(true).open(node)?;
    feed_once(&mut file)?;
    held.push((node.to_path_buf(), file));
    Ok(Held::Fed)
}

/// Whether `file` is a character device whose number is `want` (the node's own, when it is one).
fn is_node(file: &File, want: Option<u64>) -> bool {
    match (file.metadata(), want) {
        (Ok(meta), Some(rdev)) => meta.file_type().is_char_device() && meta.rdev() == rdev,
        _ => false,
    }
}

fn feed_once(file: &mut File) -> std::io::Result<()> {
    match file.write(&[FEED])? {
        1 => Ok(()),
        _ => Err(std::io::Error::from(std::io::ErrorKind::WriteZero)),
    }
}

/// Copies of the held descriptors, with their nodes: what the service manager keeps for a restart.
pub(crate) fn held_copies() -> Vec<(PathBuf, File)> {
    let held = HELD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    held.iter()
        .filter_map(|(path, file)| Some((path.clone(), file.try_clone().ok()?)))
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `/dev/null` is a character device with a device number, and writing to it changes nothing: a
    /// node for the hold's tests that needs no root to make.
    fn null_device() -> File {
        OpenOptions::new().write(true).open("/dev/null").unwrap()
    }

    #[test]
    fn a_stored_descriptor_for_the_node_is_adopted_and_nothing_is_opened() {
        let node = Path::new("/dev/null");
        let mut held = Vec::new();
        let mut stored = vec![null_device()];
        assert_eq!(
            hold_in(&mut held, node, &mut stored).unwrap(),
            Held::Adopted
        );
        assert_eq!(held.len(), 1, "the adopted descriptor is the one held");
        assert!(
            stored.is_empty(),
            "the adopted descriptor is taken out of the store"
        );
    }

    #[test]
    fn a_stored_descriptor_that_is_not_the_nodes_device_is_left_and_the_node_is_fed() {
        // A different character device (/dev/zero, another device number) and a regular file: neither
        // is the node, so neither is taken, and the node is opened by name and fed.
        let zero = OpenOptions::new().write(true).open("/dev/zero").unwrap();
        let regular = tempfile_in_temp("stored-regular");
        let mut stored = vec![zero, regular];
        let node = Path::new("/dev/null");
        let mut held = Vec::new();
        assert_eq!(hold_in(&mut held, node, &mut stored).unwrap(), Held::Fed);
        assert_eq!(
            stored.len(),
            2,
            "a refused descriptor stays with the caller, to be named and closed"
        );
    }

    fn tempfile_in_temp(name: &str) -> File {
        let path =
            std::env::temp_dir().join(format!("lemnos-watchdog-{}-{name}", std::process::id()));
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let _ = fs::remove_file(&path);
        file
    }
}
