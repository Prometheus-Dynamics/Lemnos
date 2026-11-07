//! Change notification for the files the service reads (the update status,
//! the look files): one inotify instance in the service's poll set, so a
//! file that does not change costs no wake-up and no read.
//!
//! Directories are watched, not files: writers replace files by renaming
//! over them. A directory that does not exist yet is waited for through its
//! nearest existing ancestor, and one that goes away is waited for again.
//! When inotify is not there (or a watch cannot be set) the caller falls back
//! to polling.

use lemnos_linux_sys::inotify::{self, mask};
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};

/// What changed, as bits.
pub(crate) const UPDATE: u8 = 1;
pub(crate) const LOOKS: u8 = 2;
const ALL: u8 = UPDATE | LOOKS;

/// A watched directory's events: its entries written, created, removed or
/// renamed, and the directory itself going away.
const DIR_EVENTS: u32 = mask::CREATE
    | mask::DELETE
    | mask::MOVED_FROM
    | mask::MOVED_TO
    | mask::CLOSE_WRITE
    | mask::MODIFY
    | mask::DELETE_SELF
    | mask::MOVE_SELF
    | mask::MASK_ADD;
/// An ancestor's events while a directory below it is missing.
const ANCESTOR_EVENTS: u32 =
    mask::CREATE | mask::MOVED_TO | mask::DELETE_SELF | mask::MOVE_SELF | mask::MASK_ADD;

struct Dir {
    path: PathBuf,
    topic: u8,
    /// The directory's watch, while it exists.
    wd: Option<i32>,
    /// The nearest existing ancestor's watch while the directory is missing,
    /// and the length of the ancestor's path within `path`.
    ancestor: Option<(i32, usize)>,
}

pub(crate) struct FileWatch {
    inotify: Option<File>,
    dirs: Vec<Dir>,
}

impl FileWatch {
    /// An inotify instance (none when the kernel refuses one: then every
    /// [`Self::watch`] says to poll).
    pub fn new() -> Self {
        Self {
            inotify: inotify::init().ok().map(File::from),
            dirs: Vec::new(),
        }
    }

    /// Watches `dir` (or waits for it) for `topic`. `false`: it cannot be
    /// watched, and the caller polls instead.
    pub fn watch(&mut self, dir: &Path, topic: u8) -> bool {
        let Some(fd) = self.inotify.as_ref().map(AsFd::as_fd) else {
            return false;
        };
        let mut entry = Dir {
            path: dir.to_path_buf(),
            topic,
            wd: None,
            ancestor: None,
        };
        arm(fd, &mut entry);
        let armed = entry.wd.is_some() || entry.ancestor.is_some();
        if armed {
            self.dirs.push(entry);
        }
        armed
    }

    /// The descriptor to poll for readability, when anything is watched.
    pub fn fd(&self) -> Option<BorrowedFd<'_>> {
        self.inotify
            .as_ref()
            .filter(|_| !self.dirs.is_empty())
            .map(AsFd::as_fd)
    }

    /// Reads the events waiting; returns the topics whose files may have
    /// changed. Allocates nothing unless a directory has to be watched anew.
    pub fn drain(&mut self) -> u8 {
        let Some(file) = self.inotify.as_ref() else {
            return 0;
        };
        let mut changed = 0;
        let mut rearm = false;
        let mut buf = [0u8; 4096];
        loop {
            let n = match (&*file).read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            inotify::for_each_event(&buf[..n], |wd, bits, name| {
                if bits & mask::Q_OVERFLOW != 0 {
                    changed = ALL;
                    rearm = true;
                    return;
                }
                for dir in &mut self.dirs {
                    if dir.wd == Some(wd) {
                        changed |= dir.topic;
                        if bits & (mask::IGNORED | mask::DELETE_SELF | mask::MOVE_SELF) != 0 {
                            dir.wd = None;
                            rearm = true;
                        }
                    }
                    // Only the entry on the way down to the directory, or
                    // the ancestor itself going away.
                    if let Some((ancestor, len)) = dir.ancestor
                        && ancestor == wd
                        && (name.is_none_or(|name| next_component(&dir.path, len) == Some(name))
                            || bits & (mask::IGNORED | mask::DELETE_SELF | mask::MOVE_SELF) != 0)
                    {
                        rearm = true;
                    }
                }
            });
        }
        if rearm {
            let fd = file.as_fd();
            for dir in &mut self.dirs {
                if dir.wd.is_none() {
                    arm(fd, dir);
                    if dir.wd.is_some() {
                        changed |= dir.topic;
                    }
                }
            }
        }
        changed
    }
}

/// Watches `dir`, or else its nearest existing ancestor.
fn arm(fd: BorrowedFd<'_>, dir: &mut Dir) {
    if let Ok(wd) = inotify::add_watch(fd, &dir.path, DIR_EVENTS) {
        dir.wd = Some(wd);
        dir.ancestor = None;
        return;
    }
    dir.ancestor = dir
        .path
        .ancestors()
        .skip(1)
        .filter(|a| !a.as_os_str().is_empty())
        .find_map(|a| {
            inotify::add_watch(fd, a, ANCESTOR_EVENTS)
                .ok()
                .map(|wd| (wd, a.as_os_str().len()))
        });
}

/// The component of `path` after its first `len` bytes (an ancestor's path).
fn next_component(path: &Path, len: usize) -> Option<&std::ffi::OsStr> {
    use std::os::unix::ffi::OsStrExt;
    let rest = path.as_os_str().as_bytes().get(len..)?;
    let rest = rest.strip_prefix(b"/").unwrap_or(rest);
    let end = rest.iter().position(|b| *b == b'/').unwrap_or(rest.len());
    (end > 0).then(|| std::ffi::OsStr::from_bytes(&rest[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("lemnosd-watch-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn the_component_toward_a_missing_directory() {
        let path = Path::new("/var/lib/lemnos/looks.d");
        assert_eq!(
            next_component(path, "/var/lib".len()),
            Some("lemnos".as_ref())
        );
        assert_eq!(next_component(path, "/".len()), Some("var".as_ref()));
        assert_eq!(next_component(path, path.as_os_str().len()), None);
    }

    #[test]
    fn unrelated_entries_beside_a_missing_directory_wake_nothing_up() {
        let root = root("beside");
        let mut watch = FileWatch::new();
        assert!(watch.watch(&root.join("var/looks.d"), LOOKS));
        fs::write(root.join("other"), "").unwrap();
        assert_eq!(watch.drain(), 0);
        fs::create_dir_all(root.join("var/looks.d")).unwrap();
        assert_eq!(watch.drain(), LOOKS);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rename_into_a_watched_directory_is_seen() {
        let root = root("rename");
        let mut watch = FileWatch::new();
        assert!(watch.watch(&root, UPDATE));
        assert_eq!(watch.drain(), 0, "nothing yet");
        fs::write(root.join("status.tmp"), "{}").unwrap();
        fs::rename(root.join("status.tmp"), root.join("status.json")).unwrap();
        assert_eq!(watch.drain(), UPDATE);
        assert_eq!(watch.drain(), 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_directory_is_waited_for_and_watched_once_made() {
        let root = root("missing");
        let dir = root.join("a/looks.d");
        let mut watch = FileWatch::new();
        assert!(watch.watch(&root, UPDATE));
        assert!(watch.watch(&dir, LOOKS));
        fs::create_dir_all(&dir).unwrap();
        // The parent appears first, then the directory (maybe in one batch).
        let mut seen = watch.drain();
        seen |= watch.drain();
        assert_eq!(seen & LOOKS, LOOKS, "the new directory counts as a change");
        fs::write(dir.join("x.toml"), "").unwrap();
        assert_eq!(watch.drain() & LOOKS, LOOKS);
        // Removed and made again: still watched.
        fs::remove_dir_all(root.join("a")).unwrap();
        let _ = watch.drain();
        fs::create_dir_all(&dir).unwrap();
        let _ = watch.drain();
        fs::write(dir.join("y.toml"), "").unwrap();
        assert_eq!(watch.drain() & LOOKS, LOOKS);
        let _ = fs::remove_dir_all(&root);
    }
}
