//! Where each name's bytes come from. Every producer reads its source now, when a reader opens
//! the name; nothing is collected in the background. A source that is not there is `NOT_FOUND`,
//! and one over its bound is `INVALID_ARGUMENT`: a file is never cut short.

use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

use lemnos_core::{Result, Status};
use lemnos_linux_sys::statfs;
use lemnos_slot_store::boots;
use lemnos_slot_store::store::Layout;

use crate::index::Name;

/// The places the producers read from, as the board file names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The data partition (boot history, storage).
    pub data: PathBuf,
    /// The boot partition (trial reason, storage).
    pub boot: PathBuf,
    /// The block devices, for the storage life estimates.
    pub sys_block: PathBuf,
    /// The lemnos module's directory (its `parameters` are the kernel name).
    pub module: PathBuf,
    /// The image's generated files, when the board file names them.
    pub tree_overlay: Option<PathBuf>,
    pub tree_boot: Option<PathBuf>,
    pub tree_modprobe: Option<PathBuf>,
}

/// The bytes of `name`, `NOT_FOUND` when it is not there now, `INVALID_ARGUMENT` over its bound.
pub fn produce(name: Name, paths: &Paths) -> Result<Vec<u8>> {
    let bytes = match name {
        Name::Boots => boots_file(paths),
        Name::Storage => storage(paths),
        Name::TrialReason => trial_reason(paths),
        Name::TreeOverlay => tree_file(paths.tree_overlay.as_deref(), name.bound()),
        Name::TreeBoot => tree_file(paths.tree_boot.as_deref(), name.bound()),
        Name::TreeModprobe => tree_file(paths.tree_modprobe.as_deref(), name.bound()),
        Name::Kernel => kernel(paths),
    }?;
    if bytes.is_empty() {
        return Err(Status::NOT_FOUND);
    }
    if bytes.len() > name.bound() {
        return Err(Status::INVALID_ARGUMENT);
    }
    Ok(bytes)
}

fn boots_file(paths: &Paths) -> Result<Vec<u8>> {
    let layout = Layout::new(paths.data.clone(), paths.boot.clone());
    let entries = boots::load(&layout);
    Ok(boots::render(&entries).into_bytes())
}

/// The data and boot partitions' sizes and free space, then each block device's life estimates
/// where the kernel exposes them. A partition that can't be measured is left out.
fn storage(paths: &Paths) -> Result<Vec<u8>> {
    let mut out = String::new();
    for (label, dir) in [("data", &paths.data), ("boot", &paths.boot)] {
        if let (Ok(total), Ok(free)) = (statfs::total_bytes(dir), statfs::free_bytes(dir)) {
            let _ = writeln!(out, "{label} total {total} free {free}");
        }
    }
    if let Ok(entries) = fs::read_dir(&paths.sys_block) {
        let mut devices: Vec<String> = entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("mmcblk"))
            .collect();
        devices.sort();
        for device in devices {
            for (key, file) in [("life", "life_time"), ("eol", "pre_eol_info")] {
                let path = paths.sys_block.join(&device).join("device").join(file);
                if let Some(value) = read_trimmed(&path) {
                    let _ = writeln!(out, "{key} {device} {value}");
                }
            }
        }
    }
    Ok(out.into_bytes())
}

/// The trial reason kept on the boot partition, when there is one.
fn trial_reason(paths: &Paths) -> Result<Vec<u8>> {
    let text = read_trimmed(&paths.boot.join("trial-reason")).ok_or(Status::NOT_FOUND)?;
    Ok(format!("{text}\n").into_bytes())
}

/// A file the board file names, as it is. Absent when the key is not given or the file is not
/// there.
fn tree_file(path: Option<&Path>, bound: usize) -> Result<Vec<u8>> {
    let path = path.ok_or(Status::NOT_FOUND)?;
    read_capped(path, bound)
}

/// The lemnos module's parameters in effect, one `name=value` line each, sorted by name.
/// Absent while the module is not loaded.
fn kernel(paths: &Paths) -> Result<Vec<u8>> {
    let dir = paths.module.join("parameters");
    let entries = fs::read_dir(&dir).map_err(|_| Status::NOT_FOUND)?;
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut out = String::new();
    for name in names {
        if let Some(value) = read_trimmed(&dir.join(&name)) {
            let _ = writeln!(out, "{name}={value}");
        }
    }
    Ok(out.into_bytes())
}

/// The file's bytes, read at most one byte past `bound`: a larger file is then seen as over its
/// bound, without reading all of it.
fn read_capped(path: &Path, bound: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = fs::File::open(path).map_err(|_| Status::NOT_FOUND)?;
    let mut bytes = Vec::new();
    file.take(bound as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Status::IO)?;
    Ok(bytes)
}

/// A file's text without its surrounding whitespace, `None` when it can't be read or is empty.
fn read_trimmed(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}
