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

/// Where a test that writes and flushes a lot puts its scratch: `LEMNOS_TEST_RAM_DIR` when it is set,
/// otherwise `std::env::temp_dir()`.
///
/// The gate sets `LEMNOS_TEST_RAM_DIR` to a directory in RAM (tmpfs) on the machines that run it locally. The
/// board tests write their trees and flush them (`fdatasync`) hundreds of times; on a disk shared with a
/// building gate each flush waits behind the disk's other writes (one test part: 2 s in RAM, 385 s on the
/// disk while a gate built). The flush still runs; in RAM it does not wait. Nothing that uses this asserts
/// what reaches a disk. When the variable is set, the directory must be there: a missing one, or a path
/// that is not a directory, panics naming it, and nothing falls back to the disk. Unset (CI, a developer's
/// `cargo test`), it is the temp directory as before.
///
/// With `LEMNOS_TEST_RAM_LOG` also set, each call appends the calling thread's name (the test's name under
/// libtest and nextest) to that file, so the tests that use the RAM directory can be listed.
pub fn ram_temp_dir() -> PathBuf {
    let Some(dir) = std::env::var_os("LEMNOS_TEST_RAM_DIR") else {
        return std::env::temp_dir();
    };
    let dir = PathBuf::from(dir);
    match fs::metadata(&dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => panic!("LEMNOS_TEST_RAM_DIR={} is not a directory", dir.display()),
        Err(error) => panic!("LEMNOS_TEST_RAM_DIR={}: {error}", dir.display()),
    }
    if let Some(log) = std::env::var_os("LEMNOS_TEST_RAM_LOG") {
        let name = std::thread::current().name().unwrap_or("?").to_string();
        let line = format!("{name}\n");
        let written = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .and_then(|mut f| io::Write::write_all(&mut f, line.as_bytes()));
        if let Err(error) = written {
            panic!(
                "LEMNOS_TEST_RAM_LOG={}: {error}",
                PathBuf::from(log).display()
            );
        }
    }
    dir
}

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
