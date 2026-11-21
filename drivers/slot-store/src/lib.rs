//! The board's slot files, shared by the `system-slots` driver, which writes them, and
//! `lemnos-slot-init`, the initramfs program that boots a slot from them.
//!
//! - [`store`]: where each file is, the board's state, each slot's record, atomic replacement.
//! - [`manifest`]: a slot's `slot.json`, schema 3, and the dm-verity table it gives.
//! - [`records`]: the boot records' formats: `tryboot`, `file` and `uboot`.
//! - [`ubi`]: slots on raw NAND: images and the record as UBI volumes.
//! - [`boots`]: the boot history, one line per boot.
//! - [`json`]: the small JSON reader both use.
//! - [`twocopy`]: two-copy records, written one sector at a time; [`bootfat`] places their files.
//! - [`signing`]: the signature policy both signed installs apply (slot bundles and first flash).
//! - [`bootfat`] also reads a boot file through its chain, and [`bootcheck`] hashes each slot's
//!   boot files against the manifest (every boot, and lemnosd's check).
//!
//! Plain `std`, so the initramfs program stays small; the `linux` feature adds the UBI ioctls.

pub mod arrival;
pub mod bayfat;
pub mod bays;
pub mod boot_node;
pub mod boot_raw;
pub mod bootadd;
pub mod bootcheck;
pub mod bootfat;
pub mod bootimage;
pub mod boots;
pub mod bundle;
pub mod disk;
pub mod fallback;
pub mod fatadd;
pub mod fatimage;
pub mod firmware;
pub mod goodboot;
pub mod gpt;
pub mod json;
pub mod layout;
pub mod manifest;
pub mod mbr;
pub mod owner;
pub mod records;
pub mod security;
pub mod signing;
pub mod store;
pub mod tar;
pub mod twocopy;
pub mod ubi;
pub mod uboot_env;
pub mod verity;

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A directory for one test, removed after it.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(label: &str) -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            // The time keeps the name apart from one an earlier run with the same process id left.
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir().join(format!(
                "lemnos-slot-store-{label}-{}-{nanos}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            // Whatever is at the name is not ours to keep: clear it, then make the directory.
            if fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
                let _ = fs::remove_dir_all(&dir);
            } else {
                let _ = fs::remove_file(&dir);
            }
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
