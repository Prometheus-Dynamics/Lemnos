//! The boot partition's block node, found from the boot chain's source and the board's
//! `boot_sources`. Shared by every driver that reads the boot partition (the slots driver, and the
//! security switch on a slot board), so that each finds it the same way. Every refusal is named.

use crate::disk::{self, Table};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Where the boot chain's source and the boot disk are read from: the device tree's `chosen` node,
/// sysfs and the device nodes. The real paths on the board; a test may stand a tree in.
#[derive(Debug, Clone)]
pub struct BootRoots {
    pub chosen: PathBuf,
    pub sys: PathBuf,
    pub dev: PathBuf,
}

impl BootRoots {
    /// The board's own paths.
    #[must_use]
    pub fn system() -> Self {
        Self {
            chosen: PathBuf::from("/proc/device-tree/chosen"),
            sys: PathBuf::from("/sys"),
            dev: PathBuf::from("/dev"),
        }
    }
}

/// A test's boot roots, held by the test that stands a board tree in. Only a build with the
/// `testing` feature has it; a board never reads it.
#[cfg(feature = "testing")]
pub static TEST_ROOTS: Mutex<Option<BootRoots>> = Mutex::new(None);

/// The roots the driver reads: the board's, or a test's tree while a test holds one.
fn current() -> BootRoots {
    #[cfg(feature = "testing")]
    if let Some(roots) = TEST_ROOTS.lock().ok().and_then(|g| g.clone()) {
        return roots;
    }
    BootRoots::system()
}

/// The boot partition's block node for the boot chain's source: the source is looked up in the
/// board's table, its one whole disk found under the controller, and that disk's partition 1
/// returned.
///
/// # Errors
///
/// Every refusal, named: no boot source, no disk for it, no partition 1.
pub fn resolve_node(roots: &BootRoots, table: &Table) -> Result<PathBuf, String> {
    let source = disk::boot_source(&roots.chosen)?;
    let whole = disk::boot_disk(&roots.sys, table, &source)?;
    disk::boot_partition(&roots.sys, &roots.dev, &whole)
        .ok_or_else(|| format!("{whole} has no partition 1 (the boot partition)"))
}

/// The boot disk's whole-disk node (`/dev/<disk>`) for the boot chain's source (see [`resolve_node`]).
///
/// # Errors
///
/// As [`resolve_node`], up to the whole disk.
pub fn resolve_disk(roots: &BootRoots, table: &Table) -> Result<PathBuf, String> {
    let source = disk::boot_source(&roots.chosen)?;
    let whole = disk::boot_disk(&roots.sys, table, &source)?;
    Ok(roots.dev.join(whole))
}

/// The boot partition's node on this board, for `table` (see [`resolve_node`]).
///
/// # Errors
///
/// As [`resolve_node`].
pub fn node_for_table(table: &Table) -> Result<PathBuf, String> {
    resolve_node(&current(), table)
}

/// The boot disk's node on this board, for `table` (see [`resolve_disk`]).
///
/// # Errors
///
/// As [`resolve_disk`].
pub fn disk_for_table(table: &Table) -> Result<PathBuf, String> {
    resolve_disk(&current(), table)
}

/// The boot partition's node for the `boot_sources` items a driver's config gives, on this board.
///
/// # Errors
///
/// The items do not parse as a table, or the node cannot be found.
pub fn node_for_items(items: &[&str]) -> Result<PathBuf, String> {
    let table = Table::parse(items)?;
    node_for_table(&table)
}

/// The device node of partition `number` of the whole disk `disk` (a node under the roots' `dev`), found
/// through sysfs: the kernel names each partition's entry under the disk's (`<disk>p<N>` or `<disk><N>`) and
/// gives its number in the entry's `partition` file, and its node is that name under `dev`. Nothing reads the
/// disk's table here, so the daemon can resolve it (its root phase has no more to read than the number).
///
/// # Errors
///
/// `disk` is not a node name, or the disk has no partition `number` in sysfs.
pub fn partition_node(disk: &Path, number: u32) -> Result<PathBuf, String> {
    let roots = current();
    let whole = disk
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} is not a disk's node", disk.display()))?;
    disk::partition_of(&roots.sys, whole, number)
        .map(|part| roots.dev.join(part.name))
        .ok_or_else(|| format!("{whole} has no partition {number} in sysfs"))
}

/// A boot node the daemon's root phase opened before it dropped to its own user: the dropped daemon
/// can't open the node by name (it is `root:disk 0660`), so every reader and writer of the boot
/// partition goes through [`open`], which hands out a copy of this one.
struct Handed {
    node: PathBuf,
    file: File,
    write: bool,
}

static HANDED: Mutex<Vec<Handed>> = Mutex::new(Vec::new());

/// Keeps `file`, opened on `node` by the root phase, for [`open`]. `write` says whether it was
/// opened read-write: only when the boot records on it are ours. A later hand-over of the same node
/// replaces the earlier one. The file is close-on-exec (the standard library opens every file so,
/// and [`File::try_clone`] keeps it), so no child process inherits it.
pub fn hand_over(node: &Path, file: File, write: bool) {
    let mut handed = HANDED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    handed.retain(|h| h.node != node);
    handed.push(Handed {
        node: node.to_path_buf(),
        file,
        write,
    });
}

/// Whether the root phase handed `node` over, and if so whether read-write.
#[must_use]
pub fn handed(node: &Path) -> Option<bool> {
    let handed = HANDED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    handed.iter().find(|h| h.node == node).map(|h| h.write)
}

/// Why a write to a node handed over read only is refused.
pub const READ_ONLY: &str = "the daemon holds the boot partition read only: its root phase found the boot records are not ours, so nothing on it is written";

/// The boot node `node`, for reading, or for writing too: a copy of the file the root phase handed
/// over when it did (a write is refused when that file is read only), else the node opened by name
/// (a daemon that runs as root, or a test).
///
/// # Errors
///
/// A write on a node handed over read only, or the open's own error.
pub fn open(node: &Path, write: bool) -> io::Result<File> {
    {
        let handed = HANDED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(h) = handed.iter().find(|h| h.node == node) {
            if write && !h.write {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, READ_ONLY));
            }
            return h.file.try_clone();
        }
    }
    OpenOptions::new().read(true).write(write).open(node)
}
