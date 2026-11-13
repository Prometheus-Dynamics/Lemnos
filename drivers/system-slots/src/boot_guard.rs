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
use std::io;
use std::path::Path;

use lemnos_slot_store::bootcheck;
use lemnos_slot_store::manifest::Manifest;
use lemnos_slot_store::store::{Layout, Meta, State, valid_id};

use crate::boot::BootRecord;

/// Reads a boot file of the mounted boot partition: `None` when it is not there.
fn read_mounted(root: &Path, path: &str) -> Result<Option<Vec<u8>>, String> {
    match fs::read(root.join(path)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Whether slot `id`'s boot files match its manifest. The error names what does not.
///
/// # Errors
///
/// The manifest is missing or not readable, or a boot file does not match.
pub fn verified(layout: &Layout, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err(format!("{id:?} isn't a slot id"));
    }
    let text = fs::read_to_string(layout.manifest(id))
        .map_err(|e| format!("{id}'s manifest can't be read: {e}"))?;
    let manifest = Manifest::parse(&text).map_err(|e| format!("{id}'s manifest: {e}"))?;
    let found = bootcheck::check(id, &manifest.boot_files, |path| {
        read_mounted(&layout.boot, path)
    });
    if found.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = found.iter().map(|m| m.to_string()).collect();
    Err(list.join("; "))
}

/// The check. Returns a note when it changed the default, `None` when there was nothing to do.
///
/// # Errors
///
/// The default does not match and no verified previous takes its place. With `restart` the
/// error refuses the restart; without it (a slow period) it is only reported.
pub fn check(
    layout: &Layout,
    record: &mut dyn BootRecord,
    restart: bool,
) -> Result<Option<String>, String> {
    let state = State::load(layout);
    if !valid_id(&state.default) {
        return Ok(None);
    }
    let why = match verified(layout, &state.default) {
        Ok(()) => return Ok(None),
        Err(why) => why,
    };
    if valid_id(&state.previous) && verified(layout, &state.previous).is_ok() {
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
