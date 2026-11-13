//! What a descriptor allows, read from the descriptor alone, with no mapping of it.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

/// Whether a writable shared mapping of `fd` can succeed, decided without mapping anything.
///
/// Two things are read. The descriptor's access mode (`F_GETFL`): a descriptor opened read-only
/// never maps writable. And, for a memfd, its seals (`F_GET_SEALS`): `F_SEAL_WRITE` or
/// `F_SEAL_FUTURE_WRITE` forbid a writable mapping. A descriptor that has no seals (a dma-buf, or
/// any file where `F_GET_SEALS` is `EINVAL`) is judged by its access mode alone.
///
/// This is not a probe. It can't see what the mapping would hit later: an address-space or memory
/// limit, a security module's rule, or an exporter that refuses CPU mappings of a dma-buf. Those
/// only show when a mapping is made.
pub fn may_map_writable(fd: BorrowedFd<'_>) -> io::Result<bool> {
    // SAFETY: `F_GETFL` takes no argument and reads the descriptor's flags.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::O_ACCMODE == libc::O_RDONLY {
        return Ok(false);
    }
    // SAFETY: `F_GET_SEALS` takes no argument and reads the seals of a memfd.
    let seals = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) };
    if seals < 0 {
        // Not a memfd, so there are no seals to forbid a writable mapping.
        return match io::Error::last_os_error().raw_os_error() {
            Some(libc::EINVAL) => Ok(true),
            _ => Err(io::Error::last_os_error()),
        };
    }
    Ok(seals & (libc::F_SEAL_WRITE | libc::F_SEAL_FUTURE_WRITE) == 0)
}

/// `F_SEAL_WRITE`: no writable mapping, and no write, of a memfd.
pub const SEAL_WRITE: i32 = libc::F_SEAL_WRITE;
/// `F_SEAL_FUTURE_WRITE`: no new writable mapping of a memfd.
pub const SEAL_FUTURE_WRITE: i32 = libc::F_SEAL_FUTURE_WRITE;

/// A new memfd of `len` bytes that may be sealed later (`MFD_ALLOW_SEALING`, and no seal yet), for
/// a test or a producer that seals its memory once it is done with it.
pub fn sealable_memfd(name: &std::ffi::CStr, len: u64) -> io::Result<OwnedFd> {
    // SAFETY: `name` is a valid NUL-terminated string for the call.
    let fd =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `memfd_create` returned a new descriptor that nothing else owns.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let file = std::fs::File::from(owned);
    file.set_len(len)?;
    Ok(OwnedFd::from(file))
}

/// Adds `seals` to a memfd (`F_ADD_SEALS`). A seal only ever forbids more; it is for tests and
/// for a producer that hands over memory it is done writing.
pub fn add_seals(fd: BorrowedFd<'_>, seals: i32) -> io::Result<()> {
    // SAFETY: `F_ADD_SEALS` takes an integer of seals, and reads nothing else from the caller.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsFd, AsRawFd};

    use super::*;

    fn sealable() -> OwnedFd {
        sealable_memfd(c"descriptor-test", 4096).unwrap()
    }

    #[test]
    fn a_read_write_memfd_may_map_writable_and_a_sealed_one_may_not() {
        let fd = sealable();
        assert!(may_map_writable(fd.as_fd()).unwrap());
        add_seals(fd.as_fd(), SEAL_WRITE).unwrap();
        assert!(!may_map_writable(fd.as_fd()).unwrap());
    }

    #[test]
    fn a_future_write_seal_also_forbids_a_writable_mapping() {
        let fd = sealable();
        add_seals(fd.as_fd(), SEAL_FUTURE_WRITE).unwrap();
        assert!(!may_map_writable(fd.as_fd()).unwrap());
    }

    #[test]
    fn a_descriptor_opened_read_only_may_not_map_writable() {
        let fd = sealable();
        let path = format!("/proc/self/fd/{}", fd.as_raw_fd());
        let read_only = std::fs::File::open(path).unwrap();
        assert!(!may_map_writable(read_only.as_fd()).unwrap());
    }
}
