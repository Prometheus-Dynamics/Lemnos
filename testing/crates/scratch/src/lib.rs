//! A test's scratch directory, removed when the guard drops.
//!
//! A test that removes its directory with `remove_dir_all` at the end of its body leaks the
//! directory when it panics first, such as a wait that times out under load. The leftover then
//! fills TMPDIR and fails the gate a second time. [`Scratch`] removes the directory in `Drop`
//! instead, so it goes whether the test passes or unwinds. Keep the guard in a `let` binding for
//! the whole test.

use std::fs;
use std::io;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// A directory (or file) that is removed, with everything in it, when this value drops.
#[derive(Debug)]
pub struct Scratch(PathBuf);

impl Scratch {
    /// A new, empty directory under `std::env::temp_dir()`, named after `name`, the process id
    /// and a counter. A name that is already taken is skipped. Panics, naming the path, when the
    /// directory cannot be made.
    pub fn new(name: &str) -> Scratch {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let pid = std::process::id();
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("{name}-{pid}-{n}"));
            match fs::create_dir(&path) {
                Ok(()) => return Scratch(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("cannot create scratch dir {}: {error}", path.display()),
            }
        }
    }

    /// Guards a directory someone else made, or a path the test will create. Nothing is made
    /// here; the directory is removed on drop if it exists then.
    pub fn adopt(path: PathBuf) -> Scratch {
        Scratch(path)
    }

    /// The guarded directory.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A file the test made is guarded the same way: it is not a directory, so remove it.
        if fs::remove_dir_all(&self.0).is_err() {
            let _ = fs::remove_file(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directory_is_gone_after_drop() {
        let path = {
            let dir = Scratch::new("lemnos-scratch-drop");
            assert!(dir.is_dir());
            fs::write(dir.join("file"), b"x").unwrap();
            dir.path().to_path_buf()
        };
        assert!(!path.exists());
    }

    #[test]
    fn the_directory_is_gone_after_a_caught_panic() {
        let mut path = PathBuf::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let dir = Scratch::new("lemnos-scratch-panic");
            fs::write(dir.join("file"), b"x").unwrap();
            path = dir.path().to_path_buf();
            panic!("the test fails here");
        }));
        assert!(result.is_err());
        assert!(!path.exists());
    }
}
