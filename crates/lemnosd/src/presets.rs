//! Look presets: a named set of looks (a look file's text) that is applied
//! as one. They are saved under `<state>/presets/<name>.toml` (the example
//! schemes are in `examples/looks/presets`, docs/looks.md). The active preset
//! (`<state>/presets/active`) sits above the board's looks and below the look
//! files, so a look file still wins a name. None is active until one is applied.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Whether `name` is a valid preset name (letters, digits, `-` and `_`).
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// The saved presets, in `<state>/presets`.
#[derive(Debug, Clone)]
pub struct PresetDir {
    dir: PathBuf,
}

impl PresetDir {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join("presets"),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.toml"))
    }

    /// The text of user preset `name`, if saved.
    pub fn load(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.path(name)).ok()
    }

    /// The names of the saved presets.
    pub fn names(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                name.strip_suffix(".toml").map(str::to_string)
            })
            .filter(|name| valid_name(name))
            .collect();
        names.sort();
        names
    }

    /// Writes user preset `name` (atomically).
    pub fn save(&self, name: &str, text: &str) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path(name);
        let tmp = self.dir.join(format!(".{name}.toml.tmp"));
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)
    }

    /// Deletes user preset `name`; `false` when there was none.
    pub fn delete(&self, name: &str) -> io::Result<bool> {
        match fs::remove_file(self.path(name)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The active preset's name, if one was saved.
    pub fn active(&self) -> Option<String> {
        let text = fs::read_to_string(self.dir.join("active")).ok()?;
        let name = text.trim().to_string();
        valid_name(&name).then_some(name)
    }

    /// Forgets the active preset (none is active).
    pub fn clear_active(&self) -> io::Result<()> {
        match fs::remove_file(self.dir.join("active")) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Records `name` as the active preset.
    pub fn set_active(&self, name: &str) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(".active.tmp");
        fs::write(&tmp, format!("{name}\n"))?;
        fs::rename(&tmp, self.dir.join("active"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_checked() {
        assert!(valid_name("scheme-b"));
        assert!(valid_name("night_2"));
        assert!(!valid_name(""));
        assert!(!valid_name("../etc"));
        assert!(!valid_name("Scheme"));
    }

    #[test]
    fn a_saved_preset_lists_loads_and_deletes() {
        let root = std::env::temp_dir().join(format!("lemnosd-presets-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = PresetDir::new(&root);
        assert!(dir.names().is_empty());
        dir.save("night", "[looks.\"detect.targets\"]\n").unwrap();
        assert_eq!(dir.names(), vec!["night".to_string()]);
        assert_eq!(
            dir.load("night").as_deref(),
            Some("[looks.\"detect.targets\"]\n")
        );
        assert!(dir.delete("night").unwrap());
        assert!(!dir.delete("night").unwrap());
        assert_eq!(dir.active(), None);
        dir.set_active("scheme-a").unwrap();
        assert_eq!(dir.active().as_deref(), Some("scheme-a"));
        let _ = fs::remove_dir_all(&root);
    }
}
