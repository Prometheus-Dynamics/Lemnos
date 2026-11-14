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
    let mut file = fs::File::open(node).map_err(|e| format!("{}: {e}", node.display()))?;
    lemnos_slot_store::bootcheck::fat_read(&mut file, path)
}

/// Whether slot `id`'s boot files match its manifest, read raw from `node`. The error names what
/// does not.
///
/// # Errors
///
/// The manifest is missing or not readable, or a boot file does not match.
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
    let found = bootcheck::check(&dir, &manifest.boot_files, &lengths, |path| read(path));
    let uboot = bootcheck::uboot_check(&dir, &manifest.boot_files, &lengths, |path| read(path));
    if found.is_empty() && uboot.is_none() {
        return Ok(());
    }
    let mut list: Vec<String> = found.iter().map(|m| m.to_string()).collect();
    list.extend(uboot);
    Err(list.join("; "))
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

/// Makes the previous slot the default: the old default is marked failed, then the boot record
/// points at the previous, then the state swaps the two.
fn repoint(
    layout: &Layout,
    record: &mut dyn BootRecord,
    state: &State,
    why: &str,
) -> Result<(), String> {
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
