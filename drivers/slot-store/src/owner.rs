//! Who owns the slots state: the one rule for giving the daemon's dirs (state, images, boot files) to its user,
//! run as root before lemnosd starts (slot-init on a board; the QEMU route-A check stands it in).
//!
//! A dir is given only as itself, never what it holds (the daemon replaces a file by renaming over
//! it, which needs the dir alone), and never through a link: a path that is not a plain directory is
//! refused by name. A dir already the user's is left as it is, so a normal start changes nothing.
//! The caller decides whether the dirs are ours to give at all (the boot records' owner); this module
//! never looks.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// What happened to one dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Given {
    /// Changed to the user and group.
    Given,
    /// Already the user's: nothing done.
    Already,
}

/// Makes `dir` if missing and gives it to `uid:gid` through `chown` (an `lchown`, which the caller
/// supplies so a test can record it).
///
/// # Errors
///
/// The text to log: the dir can't be made or read, is not a plain directory, or the chown failed.
pub fn give_dir(
    dir: &Path,
    uid: u32,
    gid: u32,
    chown: impl FnOnce(&Path, u32, u32) -> Result<(), String>,
) -> Result<Given, String> {
    let _ = fs::create_dir_all(dir);
    let meta = fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.file_type().is_dir() {
        return Err(format!(
            "{} is not a plain directory: it is not given away",
            dir.display()
        ));
    }
    if (meta.uid(), meta.gid()) == (uid, gid) {
        return Ok(Given::Already);
    }
    chown(dir, uid, gid).map(|()| Given::Given)
}

/// Gives each of the layout's [`daemon_dirs`](crate::store::Layout::daemon_dirs) to `uid:gid`, in
/// order, by [`give_dir`]: each dir's result, so the caller logs every one. A parent is given before
/// the dir under it, and one dir's failure does not stop the others.
pub fn give_daemon_dirs(
    layout: &crate::store::Layout,
    uid: u32,
    gid: u32,
    chown: impl Fn(&Path, u32, u32) -> Result<(), String>,
) -> Vec<(std::path::PathBuf, Result<Given, String>)> {
    layout
        .daemon_dirs()
        .into_iter()
        .map(|dir| {
            let given = give_dir(&dir, uid, gid, &chown);
            (dir, given)
        })
        .collect()
}

/// The board's own chown: `lchown(2)`, which never follows a link.
///
/// # Errors
///
/// The call's error, as text.
pub fn lchown(dir: &Path, uid: u32, gid: u32) -> Result<(), String> {
    std::os::unix::fs::lchown(dir, Some(uid), Some(gid)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Each dir the daemon is given sits under a writable `/data` grant of the packaged unit
    /// (`packaging/systemd/grants.toml`, `access = "rw"`): owning a dir the unit's sandbox keeps
    /// read-only would not let the daemon write it either, and the two lists are kept together here.
    #[test]
    fn every_given_dir_is_under_a_writable_data_grant_of_the_unit() {
        let table = include_str!("../../../packaging/systemd/grants.toml");
        let writable: Vec<&str> = table
            .lines()
            .filter(|l| l.contains("access = \"rw\""))
            .filter_map(|l| l.split("path = \"").nth(1)?.split('"').next())
            .filter(|p| p.starts_with("/data/"))
            .collect();
        assert!(!writable.is_empty(), "the table has no writable /data rows");
        let layout = crate::store::Layout::new("/data", "/boot");
        for dir in layout.daemon_dirs() {
            let dir = dir.display().to_string();
            assert!(
                writable
                    .iter()
                    .any(|grant| dir == *grant || dir.starts_with(&format!("{grant}/"))),
                "{dir} is given to the daemon but no writable /data grant covers it: {writable:?}"
            );
        }
    }

    /// The boot-file store is given with the state and image dirs, after the images dir it sits in.
    #[test]
    fn the_daemon_dirs_are_given_in_order_with_the_boot_file_store_last() {
        let root = std::env::temp_dir().join(format!("owner-order-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let layout = crate::store::Layout::new(root.join("data"), root.join("boot"));
        let seen = RefCell::new(Vec::new());
        let given = give_daemon_dirs(&layout, 990, 990, |d, _, _| {
            seen.borrow_mut().push(
                d.strip_prefix(root.join("data"))
                    .unwrap()
                    .display()
                    .to_string(),
            );
            Ok(())
        });
        assert!(given.iter().all(|(_, g)| g.is_ok()), "{given:?}");
        assert_eq!(*seen.borrow(), ["board/slots", "slots", "slots/bootfiles"]);
        let _ = fs::remove_dir_all(&root);
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("slot-owner-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    #[test]
    fn a_missing_dir_is_made_and_given() {
        let root = scratch("missing");
        let dir = root.join("board/slots");
        let seen = RefCell::new(Vec::new());
        let got = give_dir(&dir, 990, 991, |d, u, g| {
            seen.borrow_mut().push((d.to_path_buf(), u, g));
            Ok(())
        });
        assert_eq!(got, Ok(Given::Given));
        assert!(dir.is_dir());
        assert_eq!(seen.into_inner(), [(dir, 990, 991)]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_dir_already_the_users_is_left() {
        let root = scratch("already");
        let meta = fs::metadata(&root).expect("meta");
        let got = give_dir(&root, meta.uid(), meta.gid(), |_, _, _| {
            Err("must not be called".into())
        });
        assert_eq!(got, Ok(Given::Already));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_link_is_refused_by_name_and_not_followed() {
        let root = scratch("link");
        let real = root.join("real");
        fs::create_dir_all(&real).expect("real");
        let link = root.join("slots");
        std::os::unix::fs::symlink(&real, &link).expect("link");
        let got = give_dir(&link, 1, 1, |_, _, _| Err("must not be called".into()));
        assert!(got.is_err_and(|e| e.contains("is not a plain directory")));
        let _ = fs::remove_dir_all(root);
    }
}
