//! The two mounts the gadget needs: FunctionFS at its mount point, and its removal. A plain
//! `mount(2)` and `umount2(2)`, each one system call, each path a NUL-terminated copy the call
//! owns, so no caller's buffer reaches the kernel.
//!
//! Mounting needs `CAP_SYS_ADMIN`. The gadget's privileged setup is the only caller.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

/// Mounts the FunctionFS instance named `source` at `target` with `options` (`uid=`, `gid=`,
/// `rmode=`, `fmode=`, as `f_fs.c` parses them). The kernel finds the instance by that name: the
/// function's own name, which is `NAME` for `ffs.NAME` in configfs. Any other name is `ENOENT`.
pub fn functionfs(source: &str, target: &Path, options: &str) -> io::Result<()> {
    let target = c_path(target)?;
    let source = CString::new(source).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let kind = c"functionfs";
    let data = CString::new(options).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: each pointer is a NUL-terminated string this call owns for its duration.
    let rc = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            kind.as_ptr(),
            0,
            data.as_ptr().cast(),
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Unmounts whatever is mounted at `target`. Nothing mounted there is `EINVAL`.
pub fn unmount(target: &Path) -> io::Result<()> {
    let target = c_path(target)?;
    // SAFETY: the pointer is a NUL-terminated string this call owns for its duration.
    let rc = unsafe { libc::umount2(target.as_ptr(), 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Unmounts `target` lazily (`MNT_DETACH`): the mount goes at once, and the filesystem lives on
/// until its last open file closes. A FunctionFS instance with a file still open keeps its name
/// taken, so a mount of the same name is `EBUSY` until then. Nothing mounted there is `EINVAL`.
pub fn detach(target: &Path) -> io::Result<()> {
    let target = c_path(target)?;
    // SAFETY: the pointer is a NUL-terminated string this call owns for its duration.
    let rc = unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
