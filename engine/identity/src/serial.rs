//! Where a board's serial comes from, and the one format every source is normalised to.
//!
//! The format is lowercase hex with no `0x` and no separators, 8 to 32 digits, leading zeros
//! kept. A value that does not fit that is refused, never repaired into something else.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The fewest digits a serial may have.
pub const MIN_DIGITS: usize = 8;
/// The most digits a serial may have.
pub const MAX_DIGITS: usize = 32;

const DEVICE_TREE: &str = "/proc/device-tree/serial-number";
const CPUINFO: &str = "/proc/cpuinfo";
const CMDLINE: &str = "/proc/cmdline";

/// Where the board's serial is read from. The board file's `[board.serial]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SerialSource {
    /// The device tree's `serial-number` string.
    DeviceTree {
        #[serde(default = "device_tree_path")]
        path: PathBuf,
    },
    /// The `Serial` line of `/proc/cpuinfo`.
    Cpuinfo {
        #[serde(default = "cpuinfo_path")]
        path: PathBuf,
    },
    /// Bytes of an nvmem or sysfs file: fuses, OTP or a SID block. `offset` and `length` are in
    /// bytes; `length` is 4 to 16.
    Nvmem {
        path: PathBuf,
        #[serde(default)]
        offset: u64,
        length: usize,
        #[serde(default)]
        byte_order: ByteOrder,
    },
    /// The value of one `key=value` word on the kernel command line.
    Cmdline {
        key: String,
        #[serde(default = "cmdline_path")]
        path: PathBuf,
    },
}

/// The order of an nvmem cell's bytes. `big` (the default) reads the first byte as the most
/// significant one; `little` reverses them first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ByteOrder {
    #[default]
    Big,
    Little,
}

/// Why a serial could not be read or does not fit the format.
#[derive(Debug, thiserror::Error)]
pub enum SerialError {
    /// The source could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The source was read and its value does not fit.
    #[error("{0}")]
    Refused(String),
}

impl SerialSource {
    /// Reads the source and returns the serial in the normalised form.
    ///
    /// # Errors
    ///
    /// When the source cannot be read, or its value does not fit the serial format.
    pub fn read(&self) -> Result<String, SerialError> {
        match self {
            Self::DeviceTree { path } => normalise(&read_text(path)?).map_err(refused),
            Self::Cpuinfo { path } => {
                let text = read_text(path)?;
                let raw = text
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        (key.trim() == "Serial").then_some(value)
                    })
                    .ok_or_else(|| SerialError::Refused("no Serial line in cpuinfo".into()))?;
                normalise(raw).map_err(refused)
            }
            Self::Nvmem {
                path,
                offset,
                length,
                byte_order,
            } => {
                let bytes = read_cell(path, *offset, *length)?;
                normalise_bytes(&bytes, *byte_order).map_err(refused)
            }
            Self::Cmdline { key, path } => {
                let text = read_text(path)?;
                let prefix = format!("{key}=");
                // The kernel keeps the last of a repeated word, and so does this.
                let value = text
                    .split_ascii_whitespace()
                    .filter_map(|word| word.strip_prefix(&prefix))
                    .next_back()
                    .ok_or_else(|| {
                        SerialError::Refused(format!("no {key}= on the command line"))
                    })?;
                normalise(value).map_err(refused)
            }
        }
    }
}

fn refused(why: String) -> SerialError {
    SerialError::Refused(why)
}

fn read_text(path: &Path) -> Result<String, SerialError> {
    let bytes = std::fs::read(path).map_err(|source| SerialError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    String::from_utf8(bytes)
        .map_err(|_| SerialError::Refused(format!("{} is not text", path.display())))
}

fn read_cell(path: &Path, offset: u64, length: usize) -> Result<Vec<u8>, SerialError> {
    let read = |source| SerialError::Read {
        path: path.to_path_buf(),
        source,
    };
    let mut file = File::open(path).map_err(read)?;
    file.seek(SeekFrom::Start(offset)).map_err(read)?;
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes).map_err(read)?;
    Ok(bytes)
}

/// Normalises the text of a serial: trims the ends (spaces, newlines and the NUL a device tree
/// string ends with), drops a `0x` prefix and the separators `:`, `-`, `_`, `.` and spaces, and
/// lowercases the digits.
///
/// # Errors
///
/// When what is left is not 8 to 32 hex digits, or is all zeros.
pub fn normalise(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim_matches(|c: char| c.is_ascii_whitespace() || c == '\0');
    let body = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    let mut digits = String::with_capacity(body.len());
    for c in body.chars() {
        match c {
            ':' | '-' | '_' | '.' | ' ' => {}
            c if c.is_ascii_hexdigit() => digits.push(c.to_ascii_lowercase()),
            _ => return Err(format!("{c:?} is not a hex digit")),
        }
    }
    if !(MIN_DIGITS..=MAX_DIGITS).contains(&digits.len()) {
        return Err(format!(
            "{} hex digits; a serial has {MIN_DIGITS} to {MAX_DIGITS}",
            digits.len()
        ));
    }
    if digits.bytes().all(|b| b == b'0') {
        return Err("the serial is all zeros, so it was never programmed".into());
    }
    Ok(digits)
}

/// Turns the bytes of an nvmem cell into the serial's digits. Four to sixteen bytes; all zeros
/// (never programmed) and all ones (erased) are refused.
///
/// # Errors
///
/// When the length is outside 4 to 16 bytes, or the bytes are all zeros or all ones.
pub fn normalise_bytes(bytes: &[u8], order: ByteOrder) -> Result<String, String> {
    if !(4..=16).contains(&bytes.len()) {
        return Err(format!("{} bytes; a serial cell is 4 to 16", bytes.len()));
    }
    if bytes.iter().all(|&b| b == 0) || bytes.iter().all(|&b| b == 0xff) {
        return Err("the cell is all zeros or all ones, so it was never programmed".into());
    }
    let mut ordered = bytes.to_vec();
    if order == ByteOrder::Little {
        ordered.reverse();
    }
    let hex: String = ordered.iter().map(|b| format!("{b:02x}")).collect();
    normalise(&hex)
}

fn device_tree_path() -> PathBuf {
    DEVICE_TREE.into()
}

fn cpuinfo_path() -> PathBuf {
    CPUINFO.into()
}

fn cmdline_path() -> PathBuf {
    CMDLINE.into()
}
