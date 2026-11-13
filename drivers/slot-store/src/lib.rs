//! The board's slot files, shared by the `system-slots` driver, which writes them, and
//! `lemnos-slot-init`, the initramfs program that boots a slot from them.
//!
//! - [`store`]: where each file is, the board's state, each slot's record, atomic replacement.
//! - [`manifest`]: a slot's `slot.json`, schema 3, and the dm-verity table it gives.
//! - [`records`]: the boot records' formats: `tryboot`, `file` and `uboot`.
//! - [`ubi`]: slots on raw NAND: images and the record as UBI volumes.
//! - [`json`]: the small JSON reader both use.
//!
//! Plain `std`, so the initramfs program stays small; the `linux` feature adds the UBI ioctls.

pub mod json;
pub mod manifest;
pub mod records;
pub mod store;
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
