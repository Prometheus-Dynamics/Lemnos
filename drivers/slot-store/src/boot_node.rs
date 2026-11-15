//! The boot partition's block node, found from the boot chain's source and the board's
//! `boot_sources`. Shared by every driver that reads the boot partition (the slots driver, and the
//! security switch on a slot board), so that each finds it the same way. Every refusal is named.

use crate::disk::{self, Table};
use std::path::PathBuf;
#[cfg(feature = "testing")]
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
