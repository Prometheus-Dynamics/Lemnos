//! lemnosd's boot-file check: before a restart Lemnos performs, and on a slow period, the default
//! slot's boot files (kernel, DTBs, overlays, `cmdline.txt`, initramfs) are read from the boot
//! partition and hashed against the slot's signed manifest.
//!
//! Lemnos does not mount the boot partition: the running system has it mounted where the driver's
//! `boot` setting points, and the files are read there. When the default does not match, the
//! previous slot takes its place (the default is marked failed first, so a cut after that still
//! leaves it marked), provided the previous slot matches. When neither matches, the restart is
//! refused and the board stays up; on a slow period the same check only reports.

use std::fs;

use lemnos_slot_store::bootcheck;
use lemnos_slot_store::manifest::Manifest;
use lemnos_slot_store::store::{Layout, Meta, State, valid_id};

use crate::boot::BootRecord;

/// The boot partition's block node as the board's `boot_sources` resolve it, or why it can't.
pub type Node = Result<std::path::PathBuf, String>;

/// A boot file's bytes, read through the caller's reader: `None` when the file is not there.
pub type Read<'a> = &'a mut dyn FnMut(&str) -> Result<Option<Vec<u8>>, String>;

/// Reads a boot file raw from the boot partition's block node (`fat_read`: read-only, nothing
/// mounted). The node is an error when the partition's device is not known: the read is then refused
/// by name, and the check says so.
pub fn read_raw(node: &Node, path: &str) -> Result<Option<Vec<u8>>, String> {
    let node = node.as_ref().map_err(|why| {
        format!("the boot partition's device isn't known, so its files can't be read: {why}")
    })?;
    let mut file = lemnos_slot_store::boot_node::open(node, false)
        .map_err(|e| format!("{}: {e}", node.display()))?;
    lemnos_slot_store::bootcheck::fat_read(&mut file, path)
}

/// Whether slot `id`'s boot files match its manifest, read raw from `node`. The error names what
/// does not.
///
/// # Errors
///
/// The manifest is missing or not readable, or a boot file does not match.
// The driver verifies off the tick ([`prepare`], then [`Prepared::run_raw`] on a thread of its own);
// the read in one go is the tests'.
#[cfg(test)]
pub fn verified(
    layout: &Layout,
    node: &Node,
    record: &dyn BootRecord,
    id: &str,
) -> Result<(), String> {
    verified_with(layout, record, id, &mut |path| read_raw(node, path))
}

/// [`verified`] through `read`.
///
/// # Errors
///
/// As [`verified`].
pub fn verified_with(
    layout: &Layout,
    record: &dyn BootRecord,
    id: &str,
    read: Read<'_>,
) -> Result<(), String> {
    prepare(layout, record, id)?.run(read)
}

/// What a boot-file verify of one slot needs, gathered without reading a boot file: the slot's
/// manifest and its bay (the set its record is in, and each payload's stored length). Small reads only,
/// so it is made on a tick; [`Prepared::run`], which reads and hashes the boot files, runs off it.
#[derive(Clone, Debug)]
pub struct Prepared {
    dir: String,
    boot_files: Vec<(String, String)>,
    lengths: Vec<(String, u64)>,
}

/// The slot's manifest and bay, for [`Prepared::run`].
///
/// # Errors
///
/// The id is not a slot's, its manifest can't be read or parsed, or its boot files have no bay.
pub fn prepare(layout: &Layout, record: &dyn BootRecord, id: &str) -> Result<Prepared, String> {
    if !valid_id(id) {
        return Err(format!("{id:?} isn't a slot id"));
    }
    let text = fs::read_to_string(layout.manifest(id))
        .map_err(|e| format!("{id}'s manifest can't be read: {e}"))?;
    let manifest = Manifest::parse(&text).map_err(|e| format!("{id}'s manifest: {e}"))?;
    // The slot's bay: the set its record is in, and the stored length of each payload.
    let (dir, lengths) = record
        .slot_bay(id)
        .map_err(|e| format!("{id}'s boot files have no bay: {e}"))?;
    Ok(Prepared {
        dir,
        boot_files: manifest.boot_files,
        lengths,
    })
}

impl Prepared {
    /// Reads and hashes the slot's boot files through `read`: `Ok` when every one matches its manifest,
    /// else what does not.
    ///
    /// # Errors
    ///
    /// A boot file does not match.
    pub fn run(&self, read: Read<'_>) -> Result<(), String> {
        let found = bootcheck::check(&self.dir, &self.boot_files, &self.lengths, |path| {
            read(path)
        });
        let uboot = bootcheck::uboot_check(&self.dir, &self.boot_files, &self.lengths, |path| {
            read(path)
        });
        if found.is_empty() && uboot.is_none() {
            return Ok(());
        }
        let mut list: Vec<String> = found.iter().map(|m| m.to_string()).collect();
        list.extend(uboot);
        Err(list.join("; "))
    }

    /// [`Prepared::run`], reading raw from `node`.
    ///
    /// # Errors
    ///
    /// As [`Prepared::run`].
    pub fn run_raw(&self, node: &Node) -> Result<(), String> {
        self.run(&mut |path| read_raw(node, path))
    }
}

/// The check, reading the boot files raw from `node`. Returns a note when it changed the default,
/// `None` when there was nothing to do.
///
/// # Errors
///
/// The default does not match and no verified previous takes its place. With `restart` the
/// error refuses the restart; without it (a slow period) it is only reported.
pub fn check(
    layout: &Layout,
    node: &Node,
    record: &mut dyn BootRecord,
    restart: bool,
) -> Result<Option<String>, String> {
    check_with(layout, record, restart, &mut |path| read_raw(node, path))
}

/// [`check`] through `read`.
///
/// # Errors
///
/// As [`check`].
pub fn check_with(
    layout: &Layout,
    record: &mut dyn BootRecord,
    restart: bool,
    read: Read<'_>,
) -> Result<Option<String>, String> {
    let state = State::load(layout);
    if !valid_id(&state.default) {
        return Ok(None);
    }
    let why = match verified_with(layout, record, &state.default, read) {
        Ok(()) => return Ok(None),
        Err(why) => why,
    };
    if valid_id(&state.previous) && verified_with(layout, record, &state.previous, read).is_ok() {
        repoint(layout, record, &state, &why)?;
        return Ok(Some(format!(
            "{} doesn't match its manifest ({why}): {} is the default now",
            state.default, state.previous
        )));
    }
    let text = format!(
        "{} doesn't match its manifest ({why}), and its previous {:?} doesn't either",
        state.default, state.previous
    );
    Err(if restart {
        format!("{text}: the restart is refused")
    } else {
        text
    })
}

/// What a look at the boot files found, read only: nothing is written by it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// The default matches its manifest, or there is no default to look at, or the records are
    /// another system's.
    Fine,
    /// The default does not match and the previous does: [`check`] moves the default.
    Repoint,
    /// Neither matches: what the check would report.
    Neither(String),
}

/// The slow period's look: [`check_with`]'s reading, without its writes. It reads and hashes every
/// boot file of the default (and of the previous when the default does not match), so it runs off
/// the tick; only a [`Seen::Repoint`] leads to [`check`], on the tick, which reads again before it
/// writes anything.
pub fn look_with(layout: &Layout, record: &dyn BootRecord, read: Read<'_>) -> Seen {
    let state = State::load(layout);
    if !valid_id(&state.default) {
        return Seen::Fine;
    }
    let why = match verified_with(layout, record, &state.default, read) {
        Ok(()) => return Seen::Fine,
        Err(why) => why,
    };
    if valid_id(&state.previous) && verified_with(layout, record, &state.previous, read).is_ok() {
        return Seen::Repoint;
    }
    Seen::Neither(format!(
        "{} doesn't match its manifest ({why}), and its previous {:?} doesn't either",
        state.default, state.previous
    ))
}

/// [`look_with`], reading raw from `node`.
pub fn look(layout: &Layout, node: &Node, record: &dyn BootRecord) -> Seen {
    look_with(layout, record, &mut |path| read_raw(node, path))
}

/// Makes the previous slot the default: the old default is marked failed, then the boot record
/// points at the previous, then the state swaps the two.
fn repoint(
    layout: &Layout,
    record: &mut dyn BootRecord,
    state: &State,
    why: &str,
) -> Result<(), String> {
    // Before anything is marked: a default the loader can't be made to read is not moved.
    crate::boot::loader_reads_record(record)
        .map_err(|e| format!("couldn't point the default at {}: {e}", state.previous))?;
    let mut meta = Meta::load(layout, &state.default);
    meta.set("FAILED", "yes");
    meta.set(
        "FAIL_REASON",
        &format!("its boot files don't match its manifest: {why}"),
    );
    meta.save(layout, &state.default)
        .map_err(|e| format!("couldn't mark {}: {e}", state.default))?;
    record
        .set_default(&state.previous)
        .map_err(|e| format!("couldn't point the default at {}: {e}", state.previous))?;
    record
        .fallback_copy(layout, &state.previous)
        .map_err(|e| format!("couldn't keep {}'s boot files: {e}", state.previous))?;
    let mut next = state.clone();
    next.previous = state.default.clone();
    next.default = state.previous.clone();
    next.save(layout)
        .map_err(|e| format!("couldn't save the state: {e}"))
}

#[cfg(test)]
mod loader_tests {
    use super::*;
    use crate::uboot::UbootEnv;
    use lemnos_slot_store::uboot_env::Place;

    /// A repoint is refused before the old default is marked failed, when the loader can't read the record
    /// (here: its environment has no device, so trial updates are unavailable).
    #[test]
    fn a_repoint_is_refused_before_anything_is_written_when_the_loader_cannot_read_the_record() {
        let dir = std::env::temp_dir().join(format!("lemnos-repoint-{}", std::process::id()));
        // Whatever is at the name is not ours to keep: a file left there would stop the directory.
        if fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
            let _ = fs::remove_dir_all(&dir);
        } else {
            let _ = fs::remove_file(&dir);
        }
        fs::create_dir_all(&dir).unwrap();
        let layout = Layout::new(dir.join("data"), dir.join("boot"));
        let state = State {
            default: "a".into(),
            previous: "b".into(),
            ..State::default()
        };
        state.save(&layout).unwrap();
        let mut record = UbootEnv {
            disk: Err("no disk in this test".into()),
            place: Place::device(dir.join("missing-env").to_str().unwrap(), 1024, 0, 4096).unwrap(),
            restarter: crate::boot::test_restart::stops(),
            boot_node: Err("no boot partition in this test".into()),
            partitions: None,
        };
        let err = repoint(&layout, &mut record, &state, "why").unwrap_err();
        assert!(err.contains("couldn't point the default at b"), "{err}");
        assert!(
            !Meta::load(&layout, "a").failed(),
            "the old default is not marked failed"
        );
        assert_eq!(
            State::load(&layout).default,
            "a",
            "the default did not move"
        );
        assert!(
            !dir.join("missing-env").exists(),
            "nothing was created in the environment"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
