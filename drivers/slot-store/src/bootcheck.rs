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

/// The start check of the board overlay: the live device tree's marker (`lemnos,board` and
/// `lemnos,overlay-sha256` under `/lemnos`, `live`) against the slot's `boot.overlay`. The firmware
/// drops an overlay whose file is missing without a word, so a missing, stale or foreign marker is
/// a named mismatch and the boot does not count as good. `None` when the marker is the slot's.
pub fn overlay_mismatch(
    want: &crate::manifest::Overlay,
    live: Option<(&str, &str)>,
) -> Option<String> {
    match live {
        None => Some(
            "the live device tree has no board overlay marker: the slot's overlay did not load, \
             and the firmware runs the base tree without a word"
                .into(),
        ),
        Some((board, _)) if board != want.board => Some(format!(
            "the live device tree's overlay is for board {board:?}; the slot was built for {:?}",
            want.board
        )),
        Some((_, sha)) if sha != want.sha256 => Some(format!(
            "the live device tree's overlay digest is {sha}; the slot's manifest says {}",
            want.sha256
        )),
        Some(_) => None,
    }
}

/// Checks every listed boot file of a bay set. `dir` is the set's directory (`slots/bayN`), `boot_files`
/// the manifest's list (a path under the set and its SHA-256, 64 lowercase hex digits), and `lengths`
/// each file's stored length from the set's record (a bay file is padded to its fixed size, so only
/// the stored bytes are hashed; a file with no entry is hashed whole). `read` returns a file's bytes,
/// given its path relative to the boot partition's root, or the reason it can't.
///
/// Returns the mismatches in list order; an empty list means every file matches.
pub fn check<F>(
    dir: &str,
    boot_files: &[(String, String)],
    lengths: &[(String, u64)],
    mut read: F,
) -> Vec<Mismatch>
where
    F: FnMut(&str) -> Result<Option<Vec<u8>>, String>,
{
    let mut out = Vec::new();
    for (name, want) in boot_files {
        let path = format!("{dir}/{name}");
        match read(&path) {
            Ok(None) => out.push(Mismatch::Missing(name.clone())),
            Err(why) => out.push(Mismatch::Unreadable(name.clone(), why)),
            Ok(Some(mut bytes)) => {
                if let Some((_, len)) = lengths.iter().find(|(n, _)| n == name) {
                    let len = *len as usize;
                    if bytes.len() < len {
                        out.push(Mismatch::Changed(name.clone()));
                        continue;
                    }
                    bytes.truncate(len);
                }
                if sha256_hex(&bytes) != *want {
                    out.push(Mismatch::Changed(name.clone()));
                }
            }
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
/// `conf` is the file's text, `cmdline` its stored bytes as the manifest hashes them; `Err` names the
/// entry that does not match.
pub fn extlinux_mismatch(
    dir: &str,
    boot_files: &[(String, String)],
    conf: &str,
    cmdline: &[u8],
) -> Result<(), String> {
    let absolute = format!("/{dir}/");
    let listed = |name: &str| boot_files.iter().any(|(n, _)| n == name);
    // A name the manifest lists, named by an entry `key` (`kernel`, `initrd`, `fdt`).
    let file_of = |key: &str, value: &str| -> Result<String, String> {
        let name = value
            .strip_prefix("../")
            .or_else(|| value.strip_prefix(absolute.as_str()))
            .filter(|name| listed(name))
            .ok_or_else(|| format!("extlinux {key} {value:?} isn't one of the set's files"))?;
        Ok(name.to_string())
    };
    let mut kernel = false;
    let mut initrd = false;
    let mut append = None;
    for line in conf.lines() {
        let line = line.trim_start_matches(' ').trim_end_matches(' ');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key.to_ascii_lowercase().as_str() {
            "default" | "timeout" | "label" => {}
            "kernel" => {
                if std::mem::replace(&mut kernel, true) {
                    return Err("extlinux.conf has a second kernel entry".into());
                }
                file_of("kernel", value)?;
            }
            "initrd" => {
                if std::mem::replace(&mut initrd, true) {
                    return Err("extlinux.conf has a second initrd entry".into());
                }
                file_of("initrd", value)?;
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
            _ => {
                return Err(format!(
                    "extlinux.conf has an entry it doesn't know: {key:?}"
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
            "extlinux append {append:?} isn't the set's cmdline.txt {line:?}"
        ));
    }
    Ok(())
}

/// The U-Boot form's check for the set at `dir`, read through `read` as [`check`] reads. The form is
/// U-Boot when the set holds an `extlinux/extlinux.conf` or the boot partition holds the environment
/// locator (`uboot-env.txt`). On that form the set's own conf must pass [`extlinux_mismatch`] against
/// its `cmdline.txt`. `None` when the form is not U-Boot or all matches; otherwise the first reason.
pub fn uboot_check<F>(
    dir: &str,
    boot_files: &[(String, String)],
    lengths: &[(String, u64)],
    mut read: F,
) -> Option<String>
where
    F: FnMut(&str) -> Result<Option<Vec<u8>>, String>,
{
    let own = match read(&format!("{dir}/extlinux/extlinux.conf")) {
        Ok(bytes) => bytes,
        Err(why) => return Some(format!("its extlinux.conf can't be read: {why}")),
    };
    let locator = matches!(read(crate::uboot_env::LOCATOR), Ok(Some(_)));
    if own.is_none() && !locator {
        return None;
    }
    let Some(own) = own else {
        return Some("its extlinux.conf is missing".into());
    };
    let mut cmdline = match read(&format!("{dir}/cmdline.txt")) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Some("its cmdline.txt is missing".into()),
        Err(why) => return Some(format!("its cmdline.txt can't be read: {why}")),
    };
    if let Some((_, len)) = lengths.iter().find(|(n, _)| n == "cmdline.txt") {
        cmdline.truncate(*len as usize);
    }
    let text = match std::str::from_utf8(&own) {
        Ok(text) => text,
        Err(_) => return Some("its extlinux.conf isn't UTF-8".into()),
    };
    extlinux_mismatch(dir, boot_files, text, &cmdline).err()
}

#[cfg(test)]
#[path = "bootcheck_tests.rs"]
mod tests;
