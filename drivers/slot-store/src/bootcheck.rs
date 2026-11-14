//! The every-boot check: each boot file of a slot, the command line included, read as it sits on
//! the boot partition and hashed, against the signed manifest's `boot.files`.
//!
//! The manifest's list is the bundle's files, and the bundle's `cmdline.txt` is copied to the boot
//! partition byte for byte (nothing is appended at install), so one list covers every file the
//! loader reads for the slot. A file that is missing, unreadable, or whose hash differs is a
//! [`Mismatch`]; the caller decides what that means (the default falls back, a trial fails).
//!
//! The reader is a parameter: slot-init reads the boot partition with [`crate::bootfat::read_file`]
//! (no mount) and lemnosd reads it the same way; tests read real files in a directory.

use std::fmt;

use sha2::{Digest, Sha256};

use crate::bootfat::{self, ReadAt, Refusal};

/// The largest boot file the check reads (a kernel, a DTB, an initramfs, a command line).
pub const LIMIT: u64 = 256 << 20;

/// SHA-256 of `bytes`, as 64 lowercase hex digits.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The reader over a FAT boot partition, read without a mount: a file that is not there is `None`,
/// anything else the reader refused is its reason.
///
/// # Errors
///
/// The reason the FAT reader refused the file, as text.
pub fn fat_read<D: ReadAt>(dev: &mut D, path: &str) -> Result<Option<Vec<u8>>, String> {
    match bootfat::read_file(dev, path, LIMIT) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(Refusal::Missing) => Ok(None),
        Err(refusal) => Err(refusal.to_string()),
    }
}

/// Where a slot's boot files are on the boot partition, relative to its root.
pub fn slot_dir(id: &str) -> String {
    format!("slots/{id}")
}

/// One boot file that does not match its manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// The file is not on the boot partition.
    Missing(String),
    /// The file could not be read: the reader's reason.
    Unreadable(String, String),
    /// The file is there and its hash differs from the manifest's.
    Changed(String),
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mismatch::Missing(name) => write!(f, "{name} is missing from the boot partition"),
            Mismatch::Unreadable(name, why) => write!(f, "{name} can't be read: {why}"),
            Mismatch::Changed(name) => write!(f, "{name} doesn't match its manifest hash"),
        }
    }
}

/// Checks every listed boot file of slot `id`. `boot_files` is the manifest's list: a path under
/// the slot's boot directory and its SHA-256 (64 lowercase hex digits). `read` returns a file's
/// bytes, given its path relative to the boot partition's root, or the reason it can't.
///
/// Returns the mismatches in list order; an empty list means every file matches.
pub fn check<F>(id: &str, boot_files: &[(String, String)], mut read: F) -> Vec<Mismatch>
where
    F: FnMut(&str) -> Result<Option<Vec<u8>>, String>,
{
    let dir = slot_dir(id);
    let mut out = Vec::new();
    for (name, want) in boot_files {
        let path = format!("{dir}/{name}");
        match read(&path) {
            Ok(None) => out.push(Mismatch::Missing(name.clone())),
            Err(why) => out.push(Mismatch::Unreadable(name.clone(), why)),
            Ok(Some(bytes)) if sha256_hex(&bytes) != *want => {
                out.push(Mismatch::Changed(name.clone()));
            }
            Ok(Some(_)) => {}
        }
    }
    out
}

/// The U-Boot form: the slot's generated `extlinux/extlinux.conf` must start the slot's own hashed
/// files and no other. Its `kernel` and `initrd` entries must name files the manifest lists, and
/// the slot's `board.dtb` must be one of them: the label has no `fdt` line, since the boot script
/// loads that device tree and names the boot source in it. An `fdt` or `fdtdir` line is refused by
/// name. Its `append` must equal the slot's `cmdline.txt` byte for byte, without the
/// line break the installer trims (`install.rs` passes the trimmed line to `extlinux`). Any other
/// entry is refused: a loader line this check does not know is not checked, so it is not accepted.
///
/// `conf` is the file's text, `cmdline` its bytes as the manifest hashes them; `Err` names the entry
/// that does not match.
pub fn extlinux_mismatch(
    id: &str,
    boot_files: &[(String, String)],
    conf: &str,
    cmdline: &[u8],
) -> Result<(), String> {
    let prefix = format!("/slots/{id}/");
    let listed = |name: &str| boot_files.iter().any(|(n, _)| n == name);
    // A name the manifest lists, named by an entry `key` (`kernel`, `initrd`, `fdt`).
    let file_of = |key: &str, value: &str| -> Result<String, String> {
        let name = value
            .strip_prefix(&prefix)
            .filter(|name| listed(name))
            .ok_or_else(|| format!("extlinux {key} {value:?} isn't one of the slot's files"))?;
        Ok(name.to_string())
    };
    let mut kernel = false;
    let mut initrd = false;
    let mut append = None;
    for line in conf.lines() {
        let line = line.trim_start_matches(' ');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "default" | "timeout" | "label" => {}
            "kernel" => {
                if std::mem::replace(&mut kernel, true) {
                    return Err("extlinux.conf has a second kernel entry".into());
                }
                file_of(key, value)?;
            }
            "initrd" => {
                if std::mem::replace(&mut initrd, true) {
                    return Err("extlinux.conf has a second initrd entry".into());
                }
                file_of(key, value)?;
            }
            "fdt" | "fdtdir" => {
                return Err(format!(
                    "extlinux.conf has an {key} line ({value:?}): the label has none, the boot script loads board.dtb"
                ));
            }
            "append" => {
                if append.is_some() {
                    return Err("extlinux.conf has a second append entry".into());
                }
                append = Some(value.to_string());
            }
            other => {
                return Err(format!(
                    "extlinux.conf has an entry it doesn't know: {other:?}"
                ));
            }
        }
    }
    if !kernel {
        return Err("extlinux.conf has no kernel entry".into());
    }
    if !initrd {
        return Err("extlinux.conf has no initrd entry".into());
    }
    if !listed("board.dtb") {
        return Err("the slot has no board.dtb (the device tree the boot script loads)".into());
    }
    let Some(append) = append else {
        return Err("extlinux.conf has no append entry".into());
    };
    let line = std::str::from_utf8(cmdline)
        .map_err(|_| "cmdline.txt isn't UTF-8, so no append can equal it".to_string())?;
    let line = line.trim_end_matches(['\r', '\n']);
    if append != line {
        return Err(format!(
            "extlinux append {append:?} isn't the slot's cmdline.txt {line:?}"
        ));
    }
    Ok(())
}

/// The U-Boot form's check for slot `id`, read through `read` as [`check`] reads. The form is
/// U-Boot when the slot has an `extlinux/extlinux.conf`, the partition has a `boot-record.env`, or
/// the root `extlinux/extlinux.conf` exists. On that form the slot's own conf must pass
/// [`extlinux_mismatch`] against its `cmdline.txt`; and when the slot is the default, the root
/// copy U-Boot reads must equal it byte for byte. `None` when the form is not U-Boot or all matches;
/// otherwise the first reason.
pub fn uboot_check<F>(
    id: &str,
    boot_files: &[(String, String)],
    is_default: bool,
    mut read: F,
) -> Option<String>
where
    F: FnMut(&str) -> Result<Option<Vec<u8>>, String>,
{
    let slot_conf = format!("{}/extlinux/extlinux.conf", slot_dir(id));
    let own = match read(&slot_conf) {
        Ok(bytes) => bytes,
        Err(why) => return Some(format!("its extlinux.conf can't be read: {why}")),
    };
    let record = read("boot-record.env");
    let root = read("extlinux/extlinux.conf");
    let uboot = own.is_some() || matches!(record, Ok(Some(_))) || matches!(root, Ok(Some(_)));
    if !uboot {
        return None;
    }
    let Some(own) = own else {
        return Some("its extlinux.conf is missing".into());
    };
    let cmdline = match read(&format!("{}/cmdline.txt", slot_dir(id))) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Some("its cmdline.txt is missing".into()),
        Err(why) => return Some(format!("its cmdline.txt can't be read: {why}")),
    };
    let text = match std::str::from_utf8(&own) {
        Ok(text) => text,
        Err(_) => return Some("its extlinux.conf isn't UTF-8".into()),
    };
    if let Err(why) = extlinux_mismatch(id, boot_files, text, &cmdline) {
        return Some(why);
    }
    if is_default {
        match root {
            Ok(Some(bytes)) if bytes == own => {}
            Ok(Some(_)) => {
                return Some("the root extlinux/extlinux.conf isn't the default's".into());
            }
            Ok(None) => return Some("the root extlinux/extlinux.conf is missing".into()),
            Err(why) => return Some(format!("the root extlinux/extlinux.conf: {why}")),
        }
    }
    None
}

#[cfg(test)]
#[path = "bootcheck_tests.rs"]
mod tests;
