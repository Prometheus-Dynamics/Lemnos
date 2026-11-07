//! The named looks a light can show, from the lowest to the highest:
//! the built-ins (`lemnos_light`), the board's `[looks.<name>]`, the look
//! files in `LEMNOSD_LOOKS_DIR` (read-only, `/etc/lemnos/looks.d` by default),
//! then those in `LEMNOSD_LOOKS_OVERRIDE_DIR` (writable: `looks save` writes
//! there). Within a directory, files are read in name order and a later one
//! wins a name.
//!
//! Files are watched: the service re-scans them when inotify reports a change
//! in their directories (about once a second where a directory cannot be
//! watched), and on `SIGHUP` or `lemnos-ctl looks reload`. A file that fails to parse is
//! reported and keeps the looks it had (a broken edit never blanks a look).

use crate::presets::{self, PresetDir};
use lemnos_board::{BoardDefinition, looks};
use lemnos_light::{BUILTIN_LOOK_NAMES, Defaults, LookSpec, builtin_look, valid_look_name};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The writable override directory when `LEMNOSD_LOOKS_OVERRIDE_DIR` is unset
/// (`looks save` writes there).
pub const DEFAULT_OVERRIDE_DIR: &str = "/var/lib/lemnos/looks.d";
/// The system's look directory (`LEMNOSD_LOOKS_DIR`).
pub const DEFAULT_LOOKS_DIR: &str = "/etc/lemnos/looks.d";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Dir,
    Writable,
}

struct File {
    path: PathBuf,
    /// The directory's index in [`LookTable::dirs`]: precedence.
    dir: usize,
    /// `(modified, length)` when last read; `None` when it was gone.
    stamp: Option<(SystemTime, u64)>,
    looks: Vec<(String, LookSpec)>,
    error: Option<String>,
}

/// Every look, by name, with where it comes from.
pub struct LookTable {
    board: Vec<(String, LookSpec)>,
    board_errors: Vec<String>,
    /// The directories, lowest precedence first; the last is writable.
    dirs: Vec<(PathBuf, Source)>,
    /// Each directory's modification time at the last full scan: a file added,
    /// removed or renamed in changes it.
    dir_stamps: Vec<Option<SystemTime>>,
    files: Vec<File>,
    /// The overriding looks (the built-ins are not in here): name, and the
    /// origin shown by `looks list`.
    effective: BTreeMap<String, (LookSpec, String)>,
    /// Where user presets are saved (`None`: only the built-ins).
    presets: Option<PresetDir>,
    /// The active preset's name (none when `None`), and its looks (between the
    /// board's looks and the look files).
    active: Option<String>,
    preset: Vec<(String, LookSpec)>,
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

/// `(modified, length)`, the change stamp of a look file.
fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let m = fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

impl LookTable {
    /// The table for `board`'s looks and the look directories: `dir` is read
    /// (and may be missing), `writable` is read last and is where `save`
    /// writes (`None`: saving is refused).
    pub fn new(board: &BoardDefinition, dir: Option<&Path>, writable: Option<&Path>) -> Self {
        let mut board_looks = Vec::new();
        let mut board_errors = Vec::new();
        for (name, table) in &board.looks {
            match looks::from_value("board.toml", name, &toml::Value::Table(table.clone())) {
                Ok(spec) => board_looks.push((name.clone(), spec)),
                Err(errors) => board_errors.extend(errors.iter().map(ToString::to_string)),
            }
        }
        let mut dirs = Vec::new();
        if let Some(dir) = dir {
            dirs.push((dir.to_path_buf(), Source::Dir));
        }
        if let Some(writable) = writable {
            dirs.push((writable.to_path_buf(), Source::Writable));
        }
        let mut table = Self {
            board: board_looks,
            board_errors,
            dirs,
            dir_stamps: Vec::new(),
            files: Vec::new(),
            effective: BTreeMap::new(),
            presets: None,
            active: None,
            preset: Vec::new(),
        };
        table.scan();
        // A scan rebuilds only when a file changed: the board's looks need it now.
        table.rebuild();
        table
    }

    /// The override look `name`, if a board or a file defines one.
    pub fn get(&self, name: &str) -> Option<LookSpec> {
        self.effective.get(name).map(|(spec, _)| *spec)
    }

    /// Whether `name` is a look a client can ask for: a built-in or an
    /// override.
    pub fn knows(&self, name: &str) -> bool {
        self.effective.contains_key(name) || BUILTIN_LOOK_NAMES.contains(&name)
    }

    /// Re-reads the files whose contents changed. Returns whether any look
    /// may have changed (the lights then re-resolve).
    pub fn scan(&mut self) -> bool {
        // The once-a-second check costs no allocation when nothing changed:
        // the directories and every known file stat as they did.
        if self.unchanged() {
            return false;
        }
        self.dir_stamps = self.dirs.iter().map(|(dir, _)| modified(dir)).collect();
        let mut changed = false;
        let mut seen = Vec::new();
        for (index, (dir, _)) in self.dirs.clone().into_iter().enumerate() {
            let mut paths: Vec<PathBuf> = match fs::read_dir(&dir) {
                Ok(entries) => entries
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
                    .filter(|p| p.is_file())
                    .collect(),
                Err(_) => Vec::new(),
            };
            paths.sort();
            for path in paths {
                let stamp = stamp(&path);
                seen.push(path.clone());
                match self.files.iter_mut().find(|f| f.path == path) {
                    Some(file) if file.stamp == stamp && stamp.is_some() => {}
                    Some(file) => {
                        file.stamp = stamp;
                        read_into(file);
                        changed = true;
                    }
                    None => {
                        let mut file = File {
                            path,
                            dir: index,
                            stamp,
                            looks: Vec::new(),
                            error: None,
                        };
                        read_into(&mut file);
                        self.files.push(file);
                        changed = true;
                    }
                }
            }
        }
        let before = self.files.len();
        self.files.retain(|f| seen.contains(&f.path));
        changed |= self.files.len() != before;
        self.files
            .sort_by(|a, b| (a.dir, &a.path).cmp(&(b.dir, &b.path)));
        if changed {
            self.rebuild();
        }
        changed
    }

    /// The look directories, lowest precedence first.
    pub fn dirs(&self) -> impl Iterator<Item = &Path> {
        self.dirs.iter().map(|(dir, _)| dir.as_path())
    }

    /// Whether the directories (or their absence) and the files read stat as
    /// they did at the last scan (a file that was gone or unreadable is always
    /// looked at again).
    fn unchanged(&self) -> bool {
        self.dir_stamps.len() == self.dirs.len()
            && self
                .dirs
                .iter()
                .zip(&self.dir_stamps)
                .all(|((dir, _), stamp)| modified(dir) == *stamp)
            && self
                .files
                .iter()
                .all(|f| f.stamp.is_some() && stamp(&f.path) == f.stamp)
    }

    /// Re-reads every file now, whether or not it changed. Returns the report.
    pub fn reload(&mut self) -> String {
        self.dir_stamps.clear();
        for file in &mut self.files {
            file.stamp = None;
        }
        self.scan();
        self.report()
    }

    fn rebuild(&mut self) {
        let mut effective = BTreeMap::new();
        for (name, spec) in &self.board {
            effective.insert(name.clone(), (*spec, "board.toml".to_string()));
        }
        if let Some(active) = &self.active {
            for (name, spec) in &self.preset {
                effective.insert(name.clone(), (*spec, format!("preset {active}")));
            }
        }
        for file in &self.files {
            for (name, spec) in &file.looks {
                effective.insert(name.clone(), (*spec, file.path.display().to_string()));
            }
        }
        self.effective = effective;
    }

    /// Uses the presets saved in `dir`, and applies the one recorded as active
    /// there (none when none is recorded, or when it is gone).
    pub fn use_presets(&mut self, dir: PresetDir) {
        let saved = dir
            .active()
            .and_then(|name| Some((parse_preset_checked(&name, &dir.load(&name)?).ok()?, name)));
        self.presets = Some(dir);
        match saved {
            Some((looks, name)) => {
                self.preset = looks;
                self.active = Some(name);
            }
            None => {
                self.preset = Vec::new();
                self.active = None;
            }
        }
        self.rebuild();
    }

    /// The name of the active preset, if one is.
    pub fn active_preset(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// Preset `name`'s look file text, from the saved presets.
    fn preset_text_of(&self, name: &str) -> Option<String> {
        self.presets.as_ref()?.load(name)
    }

    /// The presets, with which is active.
    pub fn preset_list(&self) -> String {
        let mut out = String::new();
        if let Some(dir) = &self.presets {
            for name in dir.names() {
                let mark = if self.active.as_deref() == Some(name.as_str()) {
                    "*"
                } else {
                    " "
                };
                out.push_str(&format!("{mark} {name} (saved)\n"));
            }
        }
        out
    }

    /// Preset `name`'s look file text.
    pub fn preset_show(&self, name: &str) -> Result<String, String> {
        self.preset_text_of(name)
            .ok_or_else(|| format!("no preset {name:?} (lemnos-ctl looks preset list)"))
    }

    /// Makes preset `name` the active one: its looks replace the active
    /// preset's, and the choice is saved (when there is a directory).
    pub fn preset_apply(&mut self, name: &str) -> Result<String, String> {
        let text = self.preset_show(name)?;
        let parsed = parse_preset_checked(name, &text)?;
        if let Some(dir) = &self.presets {
            dir.set_active(name)
                .map_err(|e| format!("saving the active preset: {e}"))?;
        }
        self.preset = parsed;
        self.active = Some(name.to_string());
        self.rebuild();
        Ok(format!("applied {name}"))
    }

    /// Saves `text` (a look file of looks, any names) as preset `name`. The
    /// active preset is re-read when it is the one saved.
    pub fn preset_save(&mut self, name: &str, text: &str) -> Result<String, String> {
        if !presets::valid_name(name) {
            return Err(format!("{name:?} is not a valid preset name"));
        }
        parse_preset_checked(name, text)?;
        let dir = self
            .presets
            .as_ref()
            .ok_or_else(|| "no state directory (LEMNOSD_STATE_DIR is off)".to_string())?;
        dir.save(name, text)
            .map_err(|e| format!("saving preset {name}: {e}"))?;
        if self.active.as_deref() == Some(name) {
            self.preset = parse_preset_checked(name, text)?;
            self.rebuild();
        }
        Ok(format!("saved {name}"))
    }

    /// Deletes saved preset `name`. Deleting the active one leaves no preset
    /// active.
    pub fn preset_delete(&mut self, name: &str) -> Result<String, String> {
        let dir = self
            .presets
            .as_ref()
            .ok_or_else(|| "no state directory (LEMNOSD_STATE_DIR is off)".to_string())?;
        if !dir
            .delete(name)
            .map_err(|e| format!("deleting preset {name}: {e}"))?
        {
            return Err(format!("no preset {name:?}"));
        }
        if self.active.as_deref() == Some(name) {
            dir.clear_active()
                .map_err(|e| format!("saving the active preset: {e}"))?;
            self.preset = Vec::new();
            self.active = None;
            self.rebuild();
        }
        Ok(format!("deleted {name}"))
    }

    /// Deletes the look `name` from the writable directory (its override, if
    /// it has one; the look it overrode, if any, shows again).
    pub fn look_delete(&mut self, name: &str) -> Result<String, String> {
        if !valid_look_name(name) {
            return Err(format!("{name:?} is not a valid look name"));
        }
        let dir = self
            .dirs
            .iter()
            .find(|(_, source)| *source == Source::Writable)
            .map(|(dir, _)| dir.clone())
            .ok_or_else(|| {
                "no writable looks directory (LEMNOSD_LOOKS_OVERRIDE_DIR is off)".to_string()
            })?;
        let path = dir.join(format!("{name}.toml"));
        fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.scan();
        Ok(format!("deleted {}", path.display()))
    }

    /// Every look and where it comes from, then what failed to load.
    pub fn list(&self) -> String {
        let mut out = String::new();
        let mut names: Vec<&str> = BUILTIN_LOOK_NAMES.to_vec();
        for name in self.effective.keys() {
            if !names.contains(&name.as_str()) {
                names.push(name.as_str());
            }
        }
        names.sort_unstable();
        for name in names {
            let origin = self
                .effective
                .get(name)
                .map_or("built-in", |(_, origin)| origin.as_str());
            out.push_str(&format!("{name:<24} {origin}\n"));
        }
        out.push_str(&self.failures());
        out
    }

    /// Lines for what did not load (empty when all did).
    fn failures(&self) -> String {
        let mut out = String::new();
        for error in &self.board_errors {
            out.push_str(&format!("error: {error}\n"));
        }
        for file in &self.files {
            if let Some(error) = &file.error {
                out.push_str(&format!("error: {error}\n"));
            }
        }
        out
    }

    /// The report of a reload: what loaded, and what failed.
    pub fn report(&self) -> String {
        let loaded: usize = self.files.iter().map(|f| f.looks.len()).sum();
        let failed = self.failures();
        let mut out = format!(
            "{} look(s) in {} file(s) loaded, {} override(s) in effect\n",
            loaded,
            self.files.len(),
            self.effective.len()
        );
        out.push_str(&failed);
        out
    }

    /// `name` as its file form: the override, else the built-in with the
    /// defaults of a light.
    pub fn show(&self, name: &str, defaults: &Defaults) -> Result<String, String> {
        let spec = self
            .get(name)
            .or_else(|| builtin_look(name, defaults))
            .ok_or_else(|| format!("unknown look {name:?} (lemnos-ctl looks list)"))?;
        Ok(looks::to_toml(name, &spec))
    }

    /// Writes `text` (a look file holding one `[looks.<name>]` table) into
    /// the writable directory as `<name>.toml`, then reads it.
    pub fn save(&mut self, name: &str, text: &str) -> Result<String, String> {
        if !valid_look_name(name) {
            return Err(format!("{name:?} is not a valid look name"));
        }
        let parsed = looks::parse_file("look text", text).map_err(join)?;
        if parsed.len() != 1 || parsed[0].0 != name {
            return Err(format!("the text must define exactly [looks.{name}]"));
        }
        let dir = self
            .dirs
            .iter()
            .find(|(_, source)| *source == Source::Writable)
            .map(|(dir, _)| dir.clone())
            .ok_or_else(|| {
                "no writable looks directory (LEMNOSD_LOOKS_OVERRIDE_DIR is off)".to_string()
            })?;
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(format!("{name}.toml"));
        let tmp = dir.join(format!(".{name}.toml.tmp"));
        let write = || -> std::io::Result<()> {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            fs::rename(&tmp, &path)
        };
        write().map_err(|e| format!("{}: {e}", path.display()))?;
        self.scan();
        Ok(format!("saved {}", path.display()))
    }
}

fn join(errors: Vec<looks::LookError>) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Parses a file into `file`; a failure keeps the looks it had.
fn read_into(file: &mut File) {
    let source = file.path.display().to_string();
    let text = match fs::read_to_string(&file.path) {
        Ok(text) => text,
        Err(e) => {
            file.error = Some(format!("{source}: {e}"));
            return;
        }
    };
    match looks::parse_file(&source, &text) {
        Ok(parsed) => {
            file.looks = parsed;
            file.error = None;
        }
        Err(errors) => file.error = Some(join(errors)),
    }
}

/// A preset's looks, or why its text is not valid.
fn parse_preset_checked(name: &str, text: &str) -> Result<Vec<(String, LookSpec)>, String> {
    looks::parse_file(&format!("preset {name}"), text).map_err(join)
}

#[cfg(test)]
mod preset_tests {
    use super::*;
    use crate::presets::PresetDir;

    /// The example scheme A (a look file's text).
    const SCHEME_A: &str = include_str!("../../../examples/looks/presets/scheme-a.toml");

    fn board() -> BoardDefinition {
        BoardDefinition::from_toml_str(
            "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n",
        )
        .unwrap()
    }

    fn root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("lemnosd-looks-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn a_scan_sees_edits_additions_and_removals_and_nothing_else() {
        let root = root("scan");
        let fill = |color: &str| {
            format!(
                "[looks.\"detect.searching\"]\nlayers = [{{ block = \"fill\", color = \"{color}\" }}]\n"
            )
        };
        let color = |table: &LookTable| match table.get("detect.searching").unwrap().layers[0]
            .unwrap()
            .block
        {
            lemnos_light::Block::Fill { color } => color,
            other => panic!("{other:?}"),
        };
        fs::write(root.join("a.toml"), fill("ffffff")).unwrap();
        let mut table = LookTable::new(&board(), Some(&root), Some(&root.join("missing")));
        assert!(!table.scan(), "nothing changed");
        // Edited in place (the directory's time does not move; the length does, as the
        // modification time may not within a clock tick).
        fs::write(root.join("a.toml"), fill("ff0000") + "# edited\n").unwrap();
        assert!(table.scan());
        assert_eq!(color(&table), lemnos_light::Rgbw::rgb(0xff0000));
        assert!(!table.scan());
        // A later file wins the name; removing it hands the name back.
        fs::write(root.join("b.toml"), fill("00ff00")).unwrap();
        assert!(table.scan());
        assert_eq!(color(&table), lemnos_light::Rgbw::rgb(0x00ff00));
        fs::remove_file(root.join("b.toml")).unwrap();
        assert!(table.scan());
        assert_eq!(color(&table), lemnos_light::Rgbw::rgb(0xff0000));
        // The writable directory appearing is looked at.
        fs::create_dir_all(root.join("missing")).unwrap();
        fs::write(root.join("missing/c.toml"), fill("0000ff")).unwrap();
        assert!(table.scan());
        assert_eq!(color(&table), lemnos_light::Rgbw::rgb(0x0000ff));
        assert!(!table.scan());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn no_preset_is_active_until_one_is_applied() {
        let root = root("none");
        let mut table = LookTable::new(&board(), None, None);
        // The built-ins are the status looks only: a named look is unknown.
        assert!(table.get("detect.targets").is_none());
        assert!(!table.knows("detect.targets"));
        assert!(table.knows("status.ok"));
        table.use_presets(PresetDir::new(&root));
        assert_eq!(table.active_preset(), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_preset_sits_between_the_board_and_the_look_files() {
        let root = root("precedence");
        let files = root.join("files");
        fs::create_dir_all(&files).unwrap();
        // A look file wins over the active preset.
        fs::write(
            files.join("detect.toml"),
            "[looks.\"detect.searching\"]\nlayers = [{ block = \"fill\", color = \"ffffff\" }]\n",
        )
        .unwrap();
        let mut table = LookTable::new(&board(), Some(&files), None);
        table.use_presets(PresetDir::new(&root));
        table.preset_save("scheme-a", SCHEME_A).unwrap();
        table.preset_apply("scheme-a").unwrap();
        // The look file's searching look is the one in force, not scheme A's.
        let searching = table.get("detect.searching").unwrap();
        assert!(matches!(
            searching.layers[0].unwrap().block,
            lemnos_light::Block::Fill { .. }
        ));
        // Scheme A's own look (the preset's) is in force where no file overrides.
        assert_eq!(
            table
                .get("link.waiting")
                .map(|l| l.layers[0].unwrap().block),
            Some(lemnos_light::Block::Comet {
                color: lemnos_light::Rgbw::rgb(0xff5a00),
                period_ms: 2_400,
                tail: 6_000,
                heads: 2,
                base: 180,
                reverse: false,
            })
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn presets_save_apply_persist_and_delete() {
        let root = root("save");
        let mut table = LookTable::new(&board(), None, None);
        table.use_presets(PresetDir::new(&root));
        let night =
            "[looks.\"detect.targets\"]\nlayers = [{ block = \"fill\", color = \"102030\" }]\n";
        assert!(table.preset_save("night", night).is_ok());
        assert!(table.preset_list().contains("night (saved)"));
        table.preset_apply("night").unwrap();
        assert_eq!(table.active_preset(), Some("night"));
        // The choice is remembered by a new table over the same directory.
        let mut again = LookTable::new(&board(), None, None);
        again.use_presets(PresetDir::new(&root));
        assert_eq!(again.active_preset(), Some("night"));
        // Deleting the active preset leaves none active.
        table.preset_delete("night").unwrap();
        assert_eq!(table.active_preset(), None);
        assert!(table.preset_delete("night").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_bad_preset_is_refused_and_nothing_changes() {
        let root = root("bad");
        let mut table = LookTable::new(&board(), None, None);
        table.use_presets(PresetDir::new(&root));
        let before = table.get("detect.targets");
        assert!(
            table
                .preset_save(
                    "broken",
                    "[looks.\"detect.targets\"]\nlayers = [{ block = \"nope\" }]\n"
                )
                .is_err()
        );
        assert_eq!(table.get("detect.targets"), before);
        let _ = fs::remove_dir_all(&root);
    }
}
