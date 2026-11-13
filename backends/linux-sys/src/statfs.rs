//! How much room a filesystem has.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The bytes an unprivileged writer may still use on the filesystem that holds `path`.
// The fields are narrower than 64 bits on some 32-bit targets.
#[allow(clippy::useless_conversion)]
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `statvfs` is plain data; all zeros is a valid value of it.
    let mut stat: libc::statvfs = unsafe { core::mem::zeroed() };
    // SAFETY: `path` is a NUL-terminated string and `stat` a writable `statvfs`, both alive for
    // the call.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::from(stat.f_bavail).saturating_mul(u64::from(stat.f_frsize)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_has_room_and_a_missing_one_says_so() {
        assert!(free_bytes(&std::env::temp_dir()).unwrap() > 0);
        let missing = free_bytes(Path::new("/nonexistent/lemnos")).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    }
}
