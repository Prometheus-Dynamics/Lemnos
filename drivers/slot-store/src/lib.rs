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
pub mod boot_raw;
pub mod bootcheck;
pub mod bootfat;
pub mod boots;
pub mod disk;
pub mod fallback;
pub mod fatimage;
pub mod firmware;
pub mod json;
pub mod manifest;
pub mod records;
pub mod signing;
pub mod store;
pub mod twocopy;
pub mod ubi;

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
            let dir = std::env::temp_dir().join(format!(
                "lemnos-slot-store-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
