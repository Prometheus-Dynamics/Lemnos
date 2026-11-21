//! Whether the slot's root can start lemnosd, checked before `switch_root`.
//!
//! slot-init arms the hardware watchdog, and after its exec lemnosd's `hw-watchdog` driver is the
//! watchdog's only feeder (`docs/slots.md`, "The watchdog chain"). A root that can't start lemnosd
//! therefore always ends in a watchdog reset some seconds later, with nothing in the log saying why.
//! This check names what is missing instead, and the slot is refused like any other bad slot.
//!
//! The root is read as files under the new root's mount point. Its symbolic links name paths inside
//! that root (`/usr/lib/...` means `<root>/usr/lib/...`), so every path is resolved here component by
//! component, inside the root, never through the initramfs's own tree.

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

/// The daemon's program, as the units start it.
pub const DAEMON: &str = "usr/bin/lemnosd";
/// The user and the group lemnosd drops to (`--user lemnos --group lemnos` in
/// `packaging/systemd/lemnosd.service` and `packaging/init/S50lemnosd`; a test ties them).
pub const USER: &str = "lemnos";
pub const GROUP: &str = "lemnos";
/// The systemd unit's name, and where a unit file or its enable links may be.
pub const UNIT: &str = "lemnosd.service";
const UNIT_DIRS: [&str; 2] = ["etc/systemd/system", "usr/lib/systemd/system"];
/// The BusyBox init script.
pub const INIT_SCRIPT: &str = "etc/init.d/S50lemnosd";

/// Symbolic links followed in one path before it is refused as a loop.
const MAX_LINKS: u32 = 16;

/// Every reason the root can't start lemnosd, joined; `Ok` with how it starts when it can.
pub fn check(root: &Path) -> Result<&'static str, String> {
    let mut missing = Vec::new();
    match resolve(root, DAEMON) {
        Some(path) if is_executable_file(&path) => {}
        Some(_) => missing.push(format!("/{DAEMON} is not an executable file")),
        None => missing.push(format!("no /{DAEMON}")),
    }
    let started = match started_by(root) {
        Ok(how) => Some(how),
        Err(why) => {
            missing.push(why);
            None
        }
    };
    if !names(root, "etc/passwd", USER) {
        missing.push(format!("no user {USER} in /etc/passwd"));
    }
    if !names(root, "etc/group", GROUP) {
        missing.push(format!("no group {GROUP} in /etc/group"));
    }
    match (missing.is_empty(), started) {
        (true, Some(how)) => Ok(how),
        _ => Err(missing.join("; ")),
    }
}

/// How the root starts lemnosd: an enabled systemd unit or the BusyBox init script.
fn started_by(root: &Path) -> Result<&'static str, String> {
    if is_masked(root) {
        return Err(format!(
            "{UNIT} is masked (/etc/systemd/system/{UNIT} links to /dev/null)"
        ));
    }
    let unit = UNIT_DIRS
        .iter()
        .any(|dir| resolve(root, &format!("{dir}/{UNIT}")).is_some_and(|p| p.is_file()));
    if unit && enabled(root) {
        return Ok("systemd unit enabled");
    }
    if resolve(root, INIT_SCRIPT).is_some_and(|p| is_executable_file(&p)) {
        return Ok("init script");
    }
    Err(if unit {
        format!("{UNIT} is not enabled (no *.wants/{UNIT} link) and there is no /{INIT_SCRIPT}")
    } else {
        format!("no {UNIT} and no /{INIT_SCRIPT}")
    })
}

/// A `*.wants/lemnosd.service` link in a unit directory whose target is a unit file.
fn enabled(root: &Path) -> bool {
    UNIT_DIRS.iter().any(|dir| {
        let Some(dir) = resolve(root, dir) else {
            return false;
        };
        let Ok(entries) = fs::read_dir(&dir) else {
            return false;
        };
        entries.flatten().any(|entry| {
            let name = entry.file_name();
            let wants = name.to_str().is_some_and(|n| n.ends_with(".wants"));
            let inside = Path::new(&dir).join(&name).join(UNIT);
            wants
                && inside
                    .strip_prefix(root)
                    .ok()
                    .and_then(|rel| resolve(root, rel.to_str()?))
                    .is_some_and(|p| p.is_file())
        })
    })
}

fn is_masked(root: &Path) -> bool {
    let path = root.join("etc/systemd/system").join(UNIT);
    match fs::read_link(&path) {
        Ok(target) => target == Path::new("/dev/null"),
        Err(_) => fs::metadata(&path).is_ok_and(|m| m.file_type().is_char_device()),
    }
}

/// The ids lemnosd drops to, from the root's `/etc/passwd` ([`USER`]'s uid) and `/etc/group`
/// ([`GROUP`]'s gid). `None` when either line is missing or malformed.
#[must_use]
pub fn ids(root: &Path) -> Option<(u32, u32)> {
    let field = |file: &str, name: &str| -> Option<u32> {
        let text = fs::read_to_string(resolve(root, file)?).ok()?;
        text.lines().find_map(|line| {
            let mut parts = line.split(':');
            (parts.next() == Some(name))
                .then(|| parts.nth(1)?.parse().ok())
                .flatten()
        })
    };
    Some((field("etc/passwd", USER)?, field("etc/group", GROUP)?))
}

/// Whether a `name:...` line opens the file (passwd and group alike).
fn names(root: &Path, file: &str, name: &str) -> bool {
    let Some(path) = resolve(root, file) else {
        return false;
    };
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    text.lines()
        .any(|line| line.split(':').next() == Some(name) && line.contains(':'))
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `rel` (relative to the root) as a path that exists, with every symbolic link on the way followed
/// inside the root: an absolute target starts again at the root, `..` never climbs above it. `None`
/// when a component is missing, or after [`MAX_LINKS`] links.
pub fn resolve(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut links = 0;
    let mut done: Vec<std::ffi::OsString> = Vec::new();
    let mut todo: Vec<std::ffi::OsString> = components(Path::new(rel)).into_iter().rev().collect();
    while let Some(part) = todo.pop() {
        if part == ".." {
            done.pop();
            continue;
        }
        let here: PathBuf = std::iter::once(root.as_os_str().to_owned())
            .chain(done.iter().cloned())
            .chain(std::iter::once(part.clone()))
            .collect();
        let meta = fs::symlink_metadata(&here).ok()?;
        if !meta.file_type().is_symlink() {
            done.push(part);
            continue;
        }
        links += 1;
        if links > MAX_LINKS {
            return None;
        }
        let target = fs::read_link(&here).ok()?;
        if target.is_absolute() {
            done.clear();
        }
        todo.extend(components(&target).into_iter().rev());
    }
    let path: PathBuf = std::iter::once(root.as_os_str().to_owned())
        .chain(done)
        .collect();
    Some(path)
}

/// The normal and `..` components of a path; the root and `.` are dropped.
fn components(path: &Path) -> Vec<std::ffi::OsString> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_owned()),
            Component::ParentDir => Some("..".into()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests;
