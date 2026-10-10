//! A device's applied calibration, kept across restarts: one TOML file per
//! device in the calibration directory (`LEMNOSD_CALIBRATION_DIR`, by default
//! `/var/lib/lemnos/calibration`). The driver owns the calibration and its
//! words (`Sensor::calibration_words`); this module only stores them.
//!
//! ```toml
//! format = "lemnos.calibration"
//! schema_version = 1
//! device = "imu"
//! driver = "bmi088"
//! revision = 42
//! words = [1, 1, 3, 1000000, ...]
//! ```
//!
//! A file is written to a temporary name in the same directory and renamed
//! over the old one, so a power cut leaves the old file or the new one. A
//! file of another format or version, or one whose words the driver refuses,
//! is ignored (the device runs with its factory calibration) and the reason
//! is kept as the device's note.

use crate::devices::Slot;
use lemnos_device::MAX_CALIBRATION_WORDS;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The `format` a calibration file must have.
pub const FORMAT: &str = "lemnos.calibration";
/// The `schema_version` a calibration file must have.
pub const SCHEMA_VERSION: i64 = 1;
/// The fewest milliseconds between two saves of one device's calibration
/// (a revision change saves at once if none was made in the last minute).
pub const SAVE_INTERVAL_MS: u64 = 60_000;
/// The directory calibration files are kept in when none is set.
pub const DEFAULT_DIR: &str = "/var/lib/lemnos/calibration";

/// The calibration directory: `LEMNOSD_CALIBRATION_DIR`, else the default.
pub fn default_dir() -> PathBuf {
    std::env::var_os("LEMNOSD_CALIBRATION_DIR")
        .map_or_else(|| PathBuf::from(DEFAULT_DIR), PathBuf::from)
}

/// A calibration file's contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub driver: String,
    pub revision: u32,
    pub words: Vec<i32>,
}

/// The file for `device` in `dir`.
pub fn path(dir: &Path, device: &str) -> PathBuf {
    dir.join(format!("{device}.toml"))
}

/// The file text for `device`'s calibration.
pub fn encode(device: &str, stored: &Stored) -> String {
    let words: Vec<String> = stored.words.iter().map(i32::to_string).collect();
    format!(
        "format = \"{FORMAT}\"\nschema_version = {SCHEMA_VERSION}\ndevice = \"{device}\"\n\
         driver = \"{}\"\nrevision = {}\nwords = [{}]\n",
        stored.driver,
        stored.revision,
        words.join(", ")
    )
}

/// Reads a calibration file's text, for `device`. The reason it is refused
/// when it is not one.
pub fn decode(text: &str, device: &str) -> Result<Stored, String> {
    let table: toml::Table = text.parse().map_err(|e| format!("not TOML: {e}"))?;
    let format = table.get("format").and_then(toml::Value::as_str);
    if format != Some(FORMAT) {
        return Err(format!("format is {format:?}, not {FORMAT:?}"));
    }
    let version = table
        .get("schema_version")
        .and_then(toml::Value::as_integer);
    if version != Some(SCHEMA_VERSION) {
        return Err(format!(
            "schema_version is {version:?}, this lemnosd reads {SCHEMA_VERSION}"
        ));
    }
    let owner = table.get("device").and_then(toml::Value::as_str);
    if owner != Some(device) {
        return Err(format!("it is for device {owner:?}, not {device:?}"));
    }
    let driver = table
        .get("driver")
        .and_then(toml::Value::as_str)
        .ok_or("no driver")?
        .to_string();
    let revision = table
        .get("revision")
        .and_then(toml::Value::as_integer)
        .and_then(|r| u32::try_from(r).ok())
        .ok_or("revision is not a u32")?;
    let words = table
        .get("words")
        .and_then(toml::Value::as_array)
        .ok_or("no words")?;
    if words.len() > MAX_CALIBRATION_WORDS {
        return Err(format!(
            "{} words, at most {MAX_CALIBRATION_WORDS} are taken",
            words.len()
        ));
    }
    let words = words
        .iter()
        .map(|w| {
            w.as_integer()
                .and_then(|w| i32::try_from(w).ok())
                .ok_or_else(|| "a word is not an i32".to_string())
        })
        .collect::<Result<Vec<i32>, String>>()?;
    Ok(Stored {
        driver,
        revision,
        words,
    })
}

/// Reads `device`'s calibration file: `Ok(None)` when there is none.
pub fn load(dir: &Path, device: &str) -> Result<Option<Stored>, String> {
    let file = path(dir, device);
    match fs::read_to_string(&file) {
        Ok(text) => decode(&text, device)
            .map(Some)
            .map_err(|why| format!("{}: {why}", file.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

/// Writes `device`'s calibration file atomically (a temporary file in `dir`,
/// then a rename over the old one).
pub fn save(dir: &Path, device: &str, stored: &Stored) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let file = path(dir, device);
    let tmp = dir.join(format!(".{device}.toml.{}.tmp", std::process::id()));
    let result = (|| {
        let mut out = fs::File::create(&tmp)?;
        out.write_all(encode(device, stored).as_bytes())?;
        out.sync_all()?;
        fs::rename(&tmp, &file)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Applies `device`'s saved calibration to a freshly built device. A refused
/// file or words leave the device with its factory calibration and set the
/// slot's note. Records the revision the device now has as saved, so the
/// loaded calibration is not written back at once.
#[allow(clippy::print_stderr)]
pub fn restore(slot: &mut Slot, dir: &Path) {
    let id = slot.id().to_string();
    let driver = slot.spec.driver.clone();
    let note = match load(dir, &id) {
        Ok(None) => String::new(),
        Ok(Some(stored)) if stored.driver != driver => {
            format!(
                "calibration: file is for driver {:?}, not {driver:?}",
                stored.driver
            )
        }
        Ok(Some(stored)) => match slot
            .device
            .as_mut()
            .map(|d| d.load_calibration(&stored.words))
        {
            Some(Ok(())) => String::new(),
            Some(Err(kind)) => format!("calibration: words refused: {kind}"),
            None => String::new(),
        },
        Err(why) => format!("calibration: {why}"),
    };
    if !note.is_empty() {
        eprintln!("lemnosd: {id}: {note}");
    }
    slot.calibration_note = note;
    slot.calibration_saved = slot
        .device
        .as_ref()
        .and_then(|d| d.calibration_status())
        .map(|s| s.revision);
    slot.calibration_saved_ms = None;
}

/// Saves `slot`'s calibration when its revision is not the one last saved.
/// With `throttle`, at most once per [`SAVE_INTERVAL_MS`] (a save too soon
/// after the last one waits for the next pass). Returns whether it saved.
#[allow(clippy::print_stderr)]
pub fn persist(slot: &mut Slot, dir: &Path, now_ms: u64, throttle: bool) -> bool {
    let Some(device) = slot.device.as_ref() else {
        return false;
    };
    let Some(status) = device.calibration_status() else {
        return false;
    };
    if slot.calibration_saved == Some(status.revision) {
        return false;
    }
    if throttle
        && slot
            .calibration_saved_ms
            .is_some_and(|at| now_ms.saturating_sub(at) < SAVE_INTERVAL_MS)
    {
        return false;
    }
    let mut words = [0i32; MAX_CALIBRATION_WORDS];
    let count = device.calibration_words(&mut words);
    let stored = Stored {
        driver: slot.spec.driver.clone(),
        revision: status.revision,
        words: words[..count].to_vec(),
    };
    let id = slot.id().to_string();
    slot.calibration_saved_ms = Some(now_ms);
    match save(dir, &id, &stored) {
        Ok(()) => {
            slot.calibration_saved = Some(status.revision);
            true
        }
        Err(e) => {
            // Tried again in a minute.
            eprintln!("lemnosd: {id}: calibration not saved: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory under the temporary directory.
    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lemnosd-cal-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn sample() -> Stored {
        Stored {
            driver: "bmi088".into(),
            revision: 42,
            words: vec![1, 1, 3, 1_000_000, -7, 0],
        }
    }

    #[test]
    fn a_calibration_file_round_trips_through_the_directory() {
        let dir = dir("round-trip");
        save(&dir, "imu", &sample()).unwrap();
        let text = fs::read_to_string(path(&dir, "imu")).unwrap();
        assert!(text.contains("format = \"lemnos.calibration\""));
        assert!(text.contains("schema_version = 1"));
        assert!(text.contains("words = [1, 1, 3, 1000000, -7, 0]"));
        assert_eq!(load(&dir, "imu").unwrap(), Some(sample()));
        // No temporary file is left behind.
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        // A device with no file has no calibration.
        assert_eq!(load(&dir, "magnetometer").unwrap(), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_file_is_refused_with_the_reason() {
        let dir = dir("bad");
        fs::create_dir_all(&dir).unwrap();
        let cases = [
            ("format = \"other\"\nschema_version = 1\n", "format is"),
            (
                "format = \"lemnos.calibration\"\nschema_version = 2\ndevice = \"imu\"\n",
                "schema_version is",
            ),
            (
                "format = \"lemnos.calibration\"\nschema_version = 1\ndevice = \"mag\"\n",
                "it is for device",
            ),
            ("this is not = [toml", "not TOML"),
        ];
        for (text, why) in cases {
            fs::write(path(&dir, "imu"), text).unwrap();
            let error = load(&dir, "imu").unwrap_err();
            assert!(error.contains(why), "{error} should say {why}");
        }
        // Words that are not integers, or too many.
        fs::write(
            path(&dir, "imu"),
            "format = \"lemnos.calibration\"\nschema_version = 1\ndevice = \"imu\"\n\
             driver = \"bmi088\"\nrevision = 1\nwords = [1, \"x\"]\n",
        )
        .unwrap();
        assert!(load(&dir, "imu").unwrap_err().contains("not an i32"));
        let many = vec![0; MAX_CALIBRATION_WORDS + 1];
        let text = encode(
            "imu",
            &Stored {
                driver: "bmi088".into(),
                revision: 1,
                words: many,
            },
        );
        fs::write(path(&dir, "imu"), text).unwrap();
        assert!(load(&dir, "imu").unwrap_err().contains("at most"));
        let _ = fs::remove_dir_all(&dir);
    }
}
