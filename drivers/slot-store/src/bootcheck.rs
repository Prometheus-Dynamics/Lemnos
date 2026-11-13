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

#[cfg(test)]
#[path = "bootcheck_tests.rs"]
mod tests;
