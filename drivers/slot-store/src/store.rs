//! The board's slot files, laid out as the board's boot chain lays them out: its update writer,
//! its boot-file store and its initramfs read and write the same files (`docs/design.md` names
//! the reference implementation).
//!
//! On the data partition:
//!
//! ```text
//! slots/<id>.erofs            the root filesystem image, its dm-verity hash tree appended
//!                             (on raw NAND, the UBI volume slot-<id> instead: see crate::ubi)
//! slots/<id>.json             its manifest
//! slots/<id>.boot             "<sha256>  <path>" for each file of its boot directory
//! slots/bootfiles/<sha256>    those files, kept by content
//! board/slots/state.env       DEFAULT, PREVIOUS, TRIAL, STAGED, STATE, ERROR, PROGRESS, STARTED_BY
//! board/slots/state.json      {"schema":1,"default":..,"previous":..,"trial":..}, for the initramfs
//! board/slots/<id>.meta       PINNED, GOOD, INSTALLED, SEQ, FAILED, FAILURES, FAIL_REASON, ...
//! board/slots/booted          the running slot, written by the initramfs
//! board/slots/settings.env    SLOTS_KEEP
//! ```
//!
//! Every file is replaced whole: written beside, synced, renamed, and its directory synced, so a
//! power cut leaves the old file or the new one.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::json::{Json, quoted};
use crate::ubi::Ubi;

/// How many slots housekeeping keeps when nothing says otherwise.
pub const DEFAULT_KEEP: u32 = 3;

/// Where everything is.
#[derive(Debug, Clone)]
pub struct Layout {
    /// The data partition's mount point.
    pub data: PathBuf,
    /// The boot partition's mount point.
    pub boot: PathBuf,
    /// On raw NAND, the UBI device whose volumes hold the images ([`crate::ubi`]); `None` keeps
    /// them as files on the data partition.
    pub ubi: Option<Ubi>,
}

impl Layout {
    /// The images as files.
    pub fn new(data: impl Into<PathBuf>, boot: impl Into<PathBuf>) -> Self {
        Self {
            data: data.into(),
            boot: boot.into(),
            ubi: None,
        }
    }

    /// Slot `id`'s image's size: its file's, or its volume's.
    pub fn image_size(&self, id: &str) -> Option<u64> {
        match &self.ubi {
            None => fs::metadata(self.image(id))
                .ok()
                .filter(fs::Metadata::is_file)
                .map(|m| m.len()),
            Some(ubi) => ubi.volume(&crate::ubi::image_volume(id)).map(|v| v.bytes),
        }
    }

    pub fn images(&self) -> PathBuf {
        self.data.join("slots")
    }

    pub fn bootfiles(&self) -> PathBuf {
        self.images().join("bootfiles")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.data.join("board/slots")
    }

    pub fn image(&self, id: &str) -> PathBuf {
        self.images().join(format!("{id}.erofs"))
    }

    pub fn manifest(&self, id: &str) -> PathBuf {
        self.images().join(format!("{id}.json"))
    }

    pub fn boot_list(&self, id: &str) -> PathBuf {
        self.images().join(format!("{id}.boot"))
    }

    pub fn meta(&self, id: &str) -> PathBuf {
        self.state_dir().join(format!("{id}.meta"))
    }

    pub fn marker(&self, name: &str) -> PathBuf {
        self.state_dir().join(name)
    }

    /// A slot's directory on the boot partition.
    pub fn boot_dir(&self, id: &str) -> PathBuf {
        self.boot.join("slots").join(id)
    }

    /// Whether a slot is installed: its manifest and its image.
    pub fn installed(&self, id: &str) -> bool {
        valid_id(id) && self.manifest(id).is_file() && self.image_size(id).is_some()
    }

    /// Every installed slot, by id.
    pub fn slots(&self) -> Vec<String> {
        let mut ids: Vec<String> = fs::read_dir(self.images())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                name.strip_suffix(".json").map(str::to_string)
            })
            .filter(|id| self.installed(id))
            .collect();
        ids.sort();
        ids
    }
}

/// A slot id as the board names files after it: `[a-z0-9._-]`, at most 64, not starting with
/// a dot.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
}

/// `state.env`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub default: String,
    pub previous: String,
    pub trial: String,
    pub staged: String,
    /// `idle`, `staging`, `staged`, `rebooting`, `confirmed`, `rolled-back`, `cancelled`,
    /// `error`.
    pub state: String,
    pub error: String,
    /// Per mille.
    pub progress: u32,
    pub started_by: String,
}

impl State {
    pub fn load(layout: &Layout) -> Self {
        let mut state = Self {
            state: "idle".into(),
            ..Self::default()
        };
        if let Ok(text) = fs::read_to_string(layout.marker("state.env")) {
            for (key, value) in env_lines(&text) {
                match key.as_str() {
                    "DEFAULT" => state.default = value,
                    "PREVIOUS" => state.previous = value,
                    "TRIAL" => state.trial = value,
                    "STAGED" => state.staged = value,
                    "STATE" => state.state = value,
                    "ERROR" => state.error = value,
                    "PROGRESS" => state.progress = value.parse().unwrap_or(0),
                    "STARTED_BY" => state.started_by = value,
                    _ => {}
                }
            }
        }
        // A board fresh from its image has state.json alone.
        if state.default.is_empty()
            && let Some(json) = fs::read_to_string(layout.marker("state.json"))
                .ok()
                .and_then(|text| Json::parse(&text))
            && let Some(default) = json.get("default").and_then(Json::text)
        {
            state.default = default.to_string();
        }
        state
    }

    pub fn save(&self, layout: &Layout) -> io::Result<()> {
        let dir = layout.state_dir();
        fs::create_dir_all(&dir)?;
        let mut env = String::new();
        for (key, value) in [
            ("DEFAULT", self.default.as_str()),
            ("PREVIOUS", &self.previous),
            ("TRIAL", &self.trial),
            ("STAGED", &self.staged),
            ("STATE", &self.state),
            ("ERROR", &self.error),
            ("PROGRESS", &self.progress.to_string()),
            ("STARTED_BY", &self.started_by),
        ] {
            env.push_str(key);
            env.push('=');
            env.push_str(&shell_quoted(value));
            env.push('\n');
        }
        replace(&dir.join("state.env"), env.as_bytes())?;
        let mut json = String::from("{\"schema\":1");
        for (key, value) in [
            ("default", &self.default),
            ("previous", &self.previous),
            ("trial", &self.trial),
        ] {
            json.push_str(",\"");
            json.push_str(key);
            json.push_str("\":");
            if value.is_empty() {
                json.push_str("null");
            } else {
                quoted(value, &mut json);
            }
        }
        json.push_str("}\n");
        replace(&dir.join("state.json"), json.as_bytes())
    }
}

/// A slot's `<id>.meta`: `KEY=value` lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta(pub Vec<(String, String)>);

impl Meta {
    pub fn load(layout: &Layout, id: &str) -> Self {
        fs::read_to_string(layout.meta(id))
            .map(|text| Self(env_lines(&text)))
            .unwrap_or_default()
    }

    pub fn get(&self, key: &str) -> &str {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map_or("", |(_, v)| v.as_str())
    }

    pub fn set(&mut self, key: &str, value: &str) {
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value.to_string(),
            None => self.0.push((key.to_string(), value.to_string())),
        }
    }

    pub fn save(&self, layout: &Layout, id: &str) -> io::Result<()> {
        fs::create_dir_all(layout.state_dir())?;
        let mut text = String::new();
        for (key, value) in &self.0 {
            text.push_str(key);
            text.push('=');
            text.push_str(&shell_quoted(value));
            text.push('\n');
        }
        replace(&layout.meta(id), text.as_bytes())
    }

    pub fn pinned(&self) -> bool {
        self.get("PINNED") == "yes"
    }

    pub fn failed(&self) -> bool {
        self.get("FAILED") == "yes"
    }

    /// The install order: the board's clock can be far off.
    pub fn seq(&self) -> u64 {
        self.get("SEQ").parse().unwrap_or(0)
    }
}

/// `KEY=value` lines as the shell writes and reads them: a value may be in single quotes, with
/// `'\''` for a quote inside.
fn env_lines(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let value = match value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
                Some(inner) => inner.replace("'\\''", "'"),
                None => value.to_string(),
            };
            Some((key.trim().to_string(), value))
        })
        .collect()
}

fn shell_quoted(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'/'));
    if plain {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Replaces `path` whole: a power cut leaves the old file or the new one.
pub fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?
        .to_string_lossy();
    let dir = path.parent().unwrap_or(Path::new("."));
    let beside = dir.join(format!(".{name}.new"));
    {
        let mut file = File::create(&beside)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&beside, path)?;
    sync_dir(dir)
}

/// Syncs a directory, so a rename in it is on the media. On the boot partition's FAT this also
/// flushes the device's write cache.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Reads a small file as text, `""` when it is missing.
pub fn read_line(path: &Path) -> String {
    fs::read_to_string(path)
        .map(|text| text.trim().to_string())
        .unwrap_or_default()
}

/// The number of slots housekeeping keeps: `settings.env`'s `SLOTS_KEEP`, else `fallback`.
pub fn keep_count(layout: &Layout, fallback: u32) -> u32 {
    fs::read_to_string(layout.marker("settings.env"))
        .ok()
        .and_then(|text| {
            env_lines(&text)
                .into_iter()
                .rev()
                .find(|(k, _)| k == "SLOTS_KEEP")
                .and_then(|(_, v)| v.parse().ok())
        })
        .unwrap_or(fallback)
}

pub fn set_keep(layout: &Layout, keep: u32) -> io::Result<()> {
    fs::create_dir_all(layout.state_dir())?;
    replace(
        &layout.marker("settings.env"),
        format!("SLOTS_KEEP={keep}\n").as_bytes(),
    )
}

/// The next install number (`seq`).
pub fn next_seq(layout: &Layout) -> io::Result<u64> {
    let next = read_line(&layout.marker("seq")).parse::<u64>().unwrap_or(0) + 1;
    fs::create_dir_all(layout.state_dir())?;
    replace(&layout.marker("seq"), format!("{next}\n").as_bytes())?;
    Ok(next)
}

/// Now as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn now_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    utc(secs)
}

/// Seconds since 1970 as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn utc(secs: u64) -> String {
    let days = secs / 86_400;
    let rest = secs % 86_400;
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::TempDir;

    #[test]
    fn ids_are_what_the_board_names_files_after() {
        for good in ["os-2026.2.0-dev.795-v-af5cee85", "a", &"x".repeat(64)] {
            assert!(valid_id(good), "{good}");
        }
        for bad in ["", ".hidden", "A", "a/b", "a b", &"x".repeat(65), ".."] {
            assert!(!valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn state_round_trips_and_writes_state_json_for_the_initramfs() {
        let dir = TempDir::new("state");
        let layout = Layout::new(dir.0.clone(), dir.0.join("boot"));
        let state = State {
            default: "a-1".into(),
            previous: String::new(),
            trial: "b-2".into(),
            staged: "b-2".into(),
            state: "rebooting".into(),
            error: "it's said 'twice'".into(),
            progress: 1000,
            started_by: "lemnos".into(),
        };
        state.save(&layout).unwrap();
        assert_eq!(State::load(&layout), state);
        assert_eq!(
            fs::read_to_string(layout.marker("state.json")).unwrap(),
            "{\"schema\":1,\"default\":\"a-1\",\"previous\":null,\"trial\":\"b-2\"}\n"
        );
        let env = fs::read_to_string(layout.marker("state.env")).unwrap();
        assert!(
            env.contains("ERROR='it'\\''s said '\\''twice'\\'''\n"),
            "{env}"
        );
    }

    #[test]
    fn a_fresh_board_has_its_default_from_state_json() {
        let dir = TempDir::new("fresh");
        let layout = Layout::new(dir.0.clone(), dir.0.join("boot"));
        fs::create_dir_all(layout.state_dir()).unwrap();
        fs::write(
            layout.marker("state.json"),
            "{\"schema\":1,\"default\":\"first\",\"previous\":null,\"trial\":null}",
        )
        .unwrap();
        let state = State::load(&layout);
        assert_eq!(state.default, "first");
        assert_eq!(state.state, "idle");
    }

    #[test]
    fn meta_reads_what_the_shell_wrote() {
        let dir = TempDir::new("meta");
        let layout = Layout::new(dir.0.clone(), dir.0.join("boot"));
        fs::create_dir_all(layout.state_dir()).unwrap();
        fs::write(
            layout.meta("a"),
            "PINNED=yes\nGOOD=untried\nFAIL_REASON='slot a didn'\\''t start'\nSEQ=4\n",
        )
        .unwrap();
        let mut meta = Meta::load(&layout, "a");
        assert!(meta.pinned());
        assert_eq!(meta.get("FAIL_REASON"), "slot a didn't start");
        assert_eq!(meta.seq(), 4);
        meta.set("GOOD", "yes");
        meta.save(&layout, "a").unwrap();
        assert_eq!(Meta::load(&layout, "a"), meta);
    }

    #[test]
    fn times_are_utc() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_794_355_200 + 3_723), "2026-11-11T01:02:03Z");
    }
}
