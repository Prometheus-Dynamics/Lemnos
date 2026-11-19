//! A directory under TMPDIR that is removed when the guard drops, so a test leaves nothing behind
//! whether it passes or panics. Each test binary includes this file with `#[path]`.

use std::path::{Path, PathBuf};

/// The directory the guard owns. It derefs to `Path`; it does not convert to `PathBuf` by value,
/// so a call that takes the guard by value cannot remove the directory early.
pub struct Scratch(pub PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
