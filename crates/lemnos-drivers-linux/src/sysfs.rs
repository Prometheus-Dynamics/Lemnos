//! Small helpers for sysfs attributes.

use crate::SysfsError;
use lemnos_hal::ErrorKind;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Reads an attribute, trimmed.
pub fn read(path: &Path) -> Result<String, SysfsError> {
    fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .map_err(|e| SysfsError::io(path, "read", &e))
}

/// Reads an attribute, or `None` if it does not exist.
pub fn read_optional(path: &Path) -> Result<Option<String>, SysfsError> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s.trim().to_string())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(SysfsError::io(path, "read", &e)),
    }
}

/// Parses an attribute as `T`.
pub fn parse<T: std::str::FromStr>(path: &Path, text: &str) -> Result<T, SysfsError> {
    text.parse().map_err(|_| {
        SysfsError::new(
            ErrorKind::Failed,
            path,
            format!("unexpected contents {text:?}"),
        )
    })
}

/// Reads and parses an attribute.
pub fn read_parsed<T: std::str::FromStr>(path: &Path) -> Result<T, SysfsError> {
    let text = read(path)?;
    parse(path, &text)
}

/// Reads and parses an attribute, or `None` if it does not exist.
pub fn read_parsed_optional<T: std::str::FromStr>(path: &Path) -> Result<Option<T>, SysfsError> {
    read_optional(path)?
        .map(|text| parse(path, &text))
        .transpose()
}

/// Writes an attribute.
pub fn write(path: &Path, value: impl std::fmt::Display) -> Result<(), SysfsError> {
    fs::write(path, value.to_string()).map_err(|e| SysfsError::io(path, "write", &e))
}

/// The class devices under `root` (for example `/sys/class/hwmon`), sorted.
pub fn entries(root: &Path) -> Result<Vec<PathBuf>, SysfsError> {
    let mut entries = match fs::read_dir(root) {
        Ok(dir) => dir
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(SysfsError::io(root, "list", &e)),
    };
    entries.sort();
    Ok(entries)
}

/// Whether the device behind a class device sits at I2C `bus`/`address`:
/// some component of its resolved path is the kernel's `<bus>-<address>`
/// client name (`1-0018`).
pub fn is_i2c_client(class_device: &Path, bus: u32, address: u16) -> bool {
    let client = format!("{bus}-{address:04x}");
    let resolved = fs::canonicalize(class_device).unwrap_or_else(|_| class_device.to_path_buf());
    let linked = fs::canonicalize(class_device.join("device")).ok();
    [Some(resolved), linked]
        .into_iter()
        .flatten()
        .any(|path| path.components().any(|c| c.as_os_str() == client.as_str()))
}
