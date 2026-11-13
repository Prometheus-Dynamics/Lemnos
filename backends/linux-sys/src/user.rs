//! The user database entry the gadget's privileged setup needs: a user's uid and primary group,
//! so the FunctionFS mount and the gadget's `UDC` attribute can be given to the daemon's user.

use std::ffi::CString;
use std::io;

/// The uid and primary gid of the user `name`, or `None` when there is no such user.
pub fn user_ids(name: &str) -> io::Result<Option<(u32, u32)>> {
    let name = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: `name` is NUL-terminated and lives across the call. The entry it returns is read
    // below, before any other call into the user database.
    let entry = unsafe { libc::getpwnam(name.as_ptr()) };
    if entry.is_null() {
        return Ok(None);
    }
    // SAFETY: `entry` is non-null and points at the entry `getpwnam` returned; only its uid is
    // read here.
    let uid = unsafe { (*entry).pw_uid };
    // SAFETY: as above; only its primary gid is read here.
    let gid = unsafe { (*entry).pw_gid };
    Ok(Some((uid, gid)))
}

/// Whether the effective uid is root.
pub fn is_root() -> bool {
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    unsafe { libc::geteuid() == 0 }
}
