//! Where a test's Unix socket goes: a short directory of its own, whatever TMPDIR is.
//!
//! A Unix socket's path must fit `sun_path`: 107 bytes, and the NUL. A test that puts its socket
//! under TMPDIR or the target directory breaks when those are deep. So a test takes its directory
//! from [`scratch_dir`], which is made under `/tmp` and is unique to this process and call, and
//! takes every socket path from [`socket`], which refuses a path that is too long, by name. Nothing
//! is cut short to fit. The owner removes the directory when the test is done.

use std::fs::DirBuilder;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// The most bytes a Unix socket path may have: `sun_path` holds 108, the last one the NUL.
pub const MAX_SOCKET_PATH: usize = 107;

/// The base every scratch directory is made in. It is short and fixed, so TMPDIR never reaches a
/// socket path.
const BASE: &str = "/tmp";

/// A new, empty directory under [`BASE`], readable only by this user. It is unique to this
/// process and call: a name another process or an earlier run left is skipped. The caller removes
/// it when done. The base is made first when it is missing (a QEMU guest's initramfs has no
/// `/tmp`). Fails when the base cannot be made or written.
pub fn scratch_dir() -> io::Result<PathBuf> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    std::fs::create_dir_all(BASE)?;
    let pid = std::process::id();
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = Path::new(BASE).join(format!("lsock-{pid}-{n}"));
        match DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
}

/// The path of a socket called `name` in `dir`. A path too long for a Unix socket is an error that
/// names it; the name is never shortened to fit.
pub fn socket(dir: &Path, name: &str) -> io::Result<PathBuf> {
    let path = dir.join(name);
    let bytes = path.as_os_str().len();
    if bytes > MAX_SOCKET_PATH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path {} is {bytes} bytes; a Unix socket path holds at most {MAX_SOCKET_PATH}",
                path.display()
            ),
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_socket_is_in_a_short_scratch_directory_whatever_tmpdir_is() {
        let dir = scratch_dir().unwrap();
        assert!(dir.starts_with(BASE));
        let path = socket(&dir, "sock").unwrap();
        assert!(path.as_os_str().len() <= MAX_SOCKET_PATH);
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn each_scratch_directory_is_its_own() {
        let first = scratch_dir().unwrap();
        let second = scratch_dir().unwrap();
        assert_ne!(first, second);
        std::fs::remove_dir(&first).unwrap();
        std::fs::remove_dir(&second).unwrap();
    }

    #[test]
    fn a_name_too_long_for_a_socket_is_refused_by_name() {
        let dir = scratch_dir().unwrap();
        let name = "n".repeat(MAX_SOCKET_PATH);
        let error = socket(&dir, &name).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(
            error.to_string().contains(&name),
            "the refusal names the path"
        );
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn the_refusal_is_at_the_limit_and_not_before() {
        let dir = Path::new("/tmp/x");
        let fits = "f".repeat(MAX_SOCKET_PATH - "/tmp/x/".len());
        assert!(socket(dir, &fits).is_ok());
        let too_long = format!("{fits}f");
        assert!(socket(dir, &too_long).is_err());
    }
}
