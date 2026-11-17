//! Where a board's serial comes from, and the one format every source is normalised to.
//!
//! The format is lowercase hex with no `0x` and no separators, 8 to 32 digits, leading zeros
//! kept. A value that does not fit that is refused, never repaired into something else.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// The fewest digits a serial may have.
pub const MIN_DIGITS: usize = 8;
/// The most digits a serial may have.
pub const MAX_DIGITS: usize = 32;

/// Where the device tree's serial is, unless the driver says otherwise.
pub const DEVICE_TREE: &str = "/proc/device-tree/serial-number";
/// Where cpuinfo is, unless the driver says otherwise.
pub const CPUINFO: &str = "/proc/cpuinfo";
/// Where the kernel command line is, unless the driver says otherwise.
pub const CMDLINE: &str = "/proc/cmdline";

/// Where the board's serial is read from. The board file's `[board.serial]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerialSource {
    /// The device tree's `serial-number` string.
    DeviceTree { path: PathBuf },
    /// The `Serial` line of `/proc/cpuinfo`.
    Cpuinfo { path: PathBuf },
    /// Bytes of an nvmem or sysfs file: fuses, OTP or a SID block. `offset` and `length` are in
    /// bytes; `length` is 4 to 16.
    Nvmem {
        path: PathBuf,
        offset: u64,
        length: usize,
        byte_order: ByteOrder,
    },
    /// The value of one `key=value` word on the kernel command line.
    Cmdline { key: String, path: PathBuf },
}

/// The order of an nvmem cell's bytes. `big` (the default) reads the first byte as the most
/// significant one; `little` reverses them first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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

/// The serial settings a board file's identity device gives, by the keys `system-identity` reads
/// (`serial`, `serial_path`, ...). The one parse of them: the driver's settings check and bind,
/// the daemon's board serial, and slot-init (which gets them from the slot's command line, as the
/// `serial_*` keys without their prefix) all go through [`Settings::source`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settings<'a> {
    /// `serial`: the source kind (`device-tree`, `cpuinfo`, `nvmem` or `cmdline`), or none.
    pub kind: Option<&'a str>,
    /// `serial_path`: the file the kind reads, where the kind has a default when none is given.
    pub path: Option<&'a str>,
    /// `serial_offset`: for `nvmem`, the byte offset (default 0).
    pub offset: Option<i64>,
    /// `serial_length`: for `nvmem`, the cell's length in bytes (required).
    pub length: Option<i64>,
    /// `serial_byte_order`: for `nvmem`, `big` (default) or `little`.
    pub byte_order: Option<&'a str>,
    /// `serial_key`: for `cmdline`, the command-line key (required).
    pub key: Option<&'a str>,
}

/// A setting that is wrong: the key it is named by, and why, in words that follow the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingError {
    pub key: &'static str,
    pub reason: &'static str,
}

impl core::fmt::Display for SettingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "setting {:?} {}", self.key, self.reason)
    }
}

impl Settings<'_> {
    /// The source these settings name, or `None` when they name none (no `serial` key).
    ///
    /// # Errors
    ///
    /// When a key is missing for its kind, or a value is not one the kind takes.
    pub fn source(&self) -> Result<Option<SerialSource>, SettingError> {
        let Some(kind) = self.kind else {
            return Ok(None);
        };
        let path = self.path;
        let source = match kind {
            "device-tree" => SerialSource::DeviceTree {
                path: path.unwrap_or(DEVICE_TREE).into(),
            },
            "cpuinfo" => SerialSource::Cpuinfo {
                path: path.unwrap_or(CPUINFO).into(),
            },
            "cmdline" => SerialSource::Cmdline {
                key: self
                    .key
                    .ok_or(SettingError {
                        key: "serial_key",
                        reason: "is required for serial = cmdline",
                    })?
                    .to_string(),
                path: path.unwrap_or(CMDLINE).into(),
            },
            "nvmem" => SerialSource::Nvmem {
                path: path
                    .ok_or(SettingError {
                        key: "serial_path",
                        reason: "is required for serial = nvmem",
                    })?
                    .into(),
                offset: u64::try_from(self.offset.unwrap_or(0)).map_err(|_| SettingError {
                    key: "serial_offset",
                    reason: "must not be negative",
                })?,
                length: usize::try_from(self.length.ok_or(SettingError {
                    key: "serial_length",
                    reason: "is required for serial = nvmem",
                })?)
                .map_err(|_| SettingError {
                    key: "serial_length",
                    reason: "must not be negative",
                })?,
                byte_order: match self.byte_order.unwrap_or("big") {
                    "big" => ByteOrder::Big,
                    "little" => ByteOrder::Little,
                    _ => {
                        return Err(SettingError {
                            key: "serial_byte_order",
                            reason: "must be big or little",
                        });
                    }
                },
            },
            _ => {
                return Err(SettingError {
                    key: "serial",
                    reason: "must be device-tree, cpuinfo, cmdline or nvmem",
                });
            }
        };
        Ok(Some(source))
    }
}

/// The setting names a `<prefix>.serial.<name>=<value>` word takes: the `system-identity` key
/// without its `serial_` prefix (`serial_path` is `path`, and `serial` itself is `kind`).
pub const WORD_NAMES: [&str; 6] = ["kind", "path", "offset", "length", "byte_order", "key"];

/// The serial settings a command line's `<prefix>.serial.<name>=<value>` words name (see
/// [`WORD_NAMES`]). The last word of a name is the one taken, as the kernel takes it. A word that
/// is not one of the names is ignored.
///
/// # Errors
///
/// When `offset` or `length` is not a whole number. The settings' own check (which kind, which
/// keys) is [`Settings::source`], which the caller runs as it needs.
pub fn settings_from_words<'a>(cmdline: &'a str, prefix: &str) -> Result<Settings<'a>, String> {
    let head = format!("{prefix}.serial.");
    let mut settings = Settings::default();
    for word in cmdline.split_ascii_whitespace() {
        let Some((key, value)) = word.split_once('=') else {
            continue;
        };
        let Some(name) = key.strip_prefix(&head) else {
            continue;
        };
        match name {
            "kind" => settings.kind = Some(value),
            "path" => settings.path = Some(value),
            "offset" => settings.offset = Some(whole(value, "serial_offset")?),
            "length" => settings.length = Some(whole(value, "serial_length")?),
            "byte_order" => settings.byte_order = Some(value),
            "key" => settings.key = Some(value),
            _ => {}
        }
    }
    Ok(settings)
}

fn whole(value: &str, key: &str) -> Result<i64, String> {
    value
        .parse()
        .map_err(|_| format!("{key} {value:?} must be a whole number"))
}

/// The board's serial as these settings name its source, read and normalised. The daemon's board
/// serial and slot-init's machine id both take it from here.
///
/// # Errors
///
/// When the settings name no source, name one wrongly, or the source cannot be read or does not
/// fit the serial format. The text is what the log says.
pub fn read_serial(settings: &Settings<'_>) -> Result<String, String> {
    match settings.source() {
        Err(error) => Err(format!("the serial settings are wrong: {error}")),
        Ok(None) => Err("the identity device names no `serial` source".into()),
        Ok(Some(source)) => source.read().map_err(|error| error.to_string()),
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    #[test]
    fn no_kind_names_no_source() {
        assert_eq!(Settings::default().source(), Ok(None));
        assert!(
            read_serial(&Settings::default())
                .unwrap_err()
                .contains("no `serial` source")
        );
    }

    #[test]
    fn each_kind_takes_its_defaults_and_its_keys() {
        let dt = Settings {
            kind: Some("device-tree"),
            ..Default::default()
        };
        assert_eq!(
            dt.source(),
            Ok(Some(SerialSource::DeviceTree {
                path: DEVICE_TREE.into()
            }))
        );
        let nv = Settings {
            kind: Some("nvmem"),
            path: Some("/sys/bus/nvmem/x"),
            offset: Some(4),
            length: Some(8),
            byte_order: Some("little"),
            ..Default::default()
        };
        assert_eq!(
            nv.source(),
            Ok(Some(SerialSource::Nvmem {
                path: "/sys/bus/nvmem/x".into(),
                offset: 4,
                length: 8,
                byte_order: ByteOrder::Little,
            }))
        );
    }

    #[test]
    fn a_missing_or_wrong_key_is_named() {
        let nvmem = Settings {
            kind: Some("nvmem"),
            length: Some(8),
            ..Default::default()
        };
        assert_eq!(nvmem.source().unwrap_err().key, "serial_path");
        let cmdline = Settings {
            kind: Some("cmdline"),
            ..Default::default()
        };
        assert_eq!(cmdline.source().unwrap_err().key, "serial_key");
        let bad = Settings {
            kind: Some("usb"),
            ..Default::default()
        };
        assert_eq!(bad.source().unwrap_err().key, "serial");
        let negative = Settings {
            kind: Some("nvmem"),
            path: Some("p"),
            length: Some(8),
            offset: Some(-1),
            ..Default::default()
        };
        assert_eq!(negative.source().unwrap_err().key, "serial_offset");
    }

    #[test]
    fn the_words_name_the_settings_and_the_last_of_a_name_is_taken() {
        let line = "os.slot=a os.serial.kind=nvmem os.serial.path=/sys/x os.serial.offset=4 \
            os.serial.length=8 os.serial.byte_order=little os.serial.length=6 other.serial.kind=cmdline";
        let settings = settings_from_words(line, "os").unwrap();
        assert_eq!(settings.kind, Some("nvmem"));
        assert_eq!(settings.path, Some("/sys/x"));
        assert_eq!(settings.offset, Some(4));
        assert_eq!(
            settings.length,
            Some(6),
            "the last word of a name is the one taken"
        );
        assert_eq!(settings.byte_order, Some("little"));
        assert_eq!(settings.key, None);
        assert!(settings.source().unwrap().is_some());
    }

    #[test]
    fn no_serial_words_name_no_source() {
        let settings = settings_from_words("os.slot=a os.roothash=b", "os").unwrap();
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.source(), Ok(None));
    }

    #[test]
    fn a_number_that_is_not_whole_is_refused_by_its_word() {
        let error =
            settings_from_words("os.serial.kind=nvmem os.serial.offset=4x", "os").unwrap_err();
        assert!(
            error.contains("serial_offset") && error.contains("whole number"),
            "{error}"
        );
        let error = settings_from_words("os.serial.length=", "os").unwrap_err();
        assert!(error.contains("serial_length"), "{error}");
    }
}
