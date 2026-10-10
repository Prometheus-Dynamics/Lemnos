//! The named looks a light can show, from the lowest to the highest:
//! the built-ins (`lemnos_light`), the board's `[looks.<name>]`, the look
//! files in `LEMNOSD_LOOKS_DIR` (read-only, `/etc/lemnos/looks.d` by default),
//! then those in `LEMNOSD_LOOKS_OVERRIDE_DIR` (writable: `looks save` writes
//! there). Within a directory, files are read in name order and a later one
//! wins a name.
//!
//! Files are watched: the service re-scans them about once a second, and on
//! `SIGHUP` or `lemnos-ctl looks reload`. A file that fails to parse is
//! reported and keeps the looks it had (a broken edit never blanks a look).

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
    files: Vec<File>,
    /// The overriding looks (the built-ins are not in here): name, and the
    /// origin shown by `looks list`.
    effective: BTreeMap<String, (LookSpec, String)>,
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
            files: Vec::new(),
            effective: BTreeMap::new(),
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
                let stamp = fs::metadata(&path)
                    .ok()
                    .and_then(|m| Some((m.modified().ok()?, m.len())));
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

    /// Re-reads every file now, whether or not it changed. Returns the report.
    pub fn reload(&mut self) -> String {
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
        for file in &self.files {
            for (name, spec) in &file.looks {
                effective.insert(name.clone(), (*spec, file.path.display().to_string()));
            }
        }
        self.effective = effective;
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
