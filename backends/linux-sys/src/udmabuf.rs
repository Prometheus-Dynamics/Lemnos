//! `/dev/udmabuf`: a dma-buf over the pages of a sealed memfd, and the memfd itself.

use crate::ioctl::{ioctl_ptr, iow};
use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

/// `struct udmabuf_create`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Create {
    memfd: u32,
    flags: u32,
    offset: u64,
    size: u64,
}

/// `F_SEAL_SHRINK`: the memfd's size may not go down (what udmabuf needs).
pub const SEAL_SHRINK: i32 = libc::F_SEAL_SHRINK;

/// `UDMABUF_FLAGS_CLOEXEC`.
const FLAGS_CLOEXEC: u32 = 0x01;

/// `UDMABUF_CREATE`.
const CREATE: u32 = iow::<Create>(b'u', 0x42);

/// A dma-buf over `size` bytes of `memfd` from `offset`, close-on-exec, made by the udmabuf
/// device open as `dev`. The kernel wants `offset` and `size` page-aligned and the memfd
/// sealed against shrinking (`F_SEAL_SHRINK`), and refuses otherwise (`EINVAL`).
pub fn create(
    dev: BorrowedFd<'_>,
    memfd: BorrowedFd<'_>,
    offset: u64,
    size: u64,
) -> io::Result<OwnedFd> {
    let mut data = Create {
        memfd: memfd.as_raw_fd() as u32,
        flags: FLAGS_CLOEXEC,
        offset,
        size,
    };
    // SAFETY: `CREATE` takes a `udmabuf_create`, which `data` is; the kernel only reads it.
    let fd = unsafe { ioctl_ptr(dev, CREATE, &raw mut data)? };
    // SAFETY: a successful `UDMABUF_CREATE` returns a new dma-buf descriptor, which no other
    // owner has.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// A new memfd named `name` (for `/proc`), close-on-exec, that takes seals.
pub fn memfd_create(name: &CStr) -> io::Result<OwnedFd> {
    // SAFETY: `name` is a valid C string for the call; the flags take no pointers.
    let fd =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new descriptor from `memfd_create`, owned by no one else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The memfd's seals (`F_GET_SEALS`): a set of `libc::F_SEAL_*`.
pub fn seals(memfd: BorrowedFd<'_>) -> io::Result<i32> {
    // SAFETY: `F_GET_SEALS` takes no argument.
    let seals = unsafe { libc::fcntl(memfd.as_raw_fd(), libc::F_GET_SEALS) };
    if seals < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(seals)
}

/// Adds `seals` (`libc::F_SEAL_*`) to the memfd (`F_ADD_SEALS`).
pub fn add_seals(memfd: BorrowedFd<'_>, seals: i32) -> io::Result<()> {
    // SAFETY: `F_ADD_SEALS` takes an integer seal set.
    if unsafe { libc::fcntl(memfd.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;

    #[test]
    fn layout_and_request_match_the_kernel_header() {
        assert_eq!(std::mem::size_of::<Create>(), 24);
        assert_eq!(std::mem::offset_of!(Create, flags), 4);
        assert_eq!(std::mem::offset_of!(Create, offset), 8);
        assert_eq!(std::mem::offset_of!(Create, size), 16);
        assert_eq!(CREATE, 0x4018_7542); // UDMABUF_CREATE
    }

    /// Opens `/dev/udmabuf`, or says by name why it could not: the real device is the only object
    /// that decides whether a memfd is a valid source. `None` is a stated skip, printed as one.
    #[allow(clippy::print_stdout)] // the skip is printed by name, so a log shows it was not run
    fn udmabuf_device() -> Option<OwnedFd> {
        let path = c"/dev/udmabuf";
        // SAFETY: `path` is a valid C string; the flags take no pointers.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            println!(
                "SKIPPED: /dev/udmabuf is not openable ({}): the real-device refusal did not run",
                io::Error::last_os_error()
            );
            return None;
        }
        // SAFETY: a new descriptor from `open`, owned by no one else.
        Some(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// A memfd of one page, sealed or not as the test asks.
    fn one_page_memfd(seal: bool) -> OwnedFd {
        let fd = memfd_create(c"lemnos-udmabuf").unwrap();
        // SAFETY: `fd` is a memfd this test owns; the size is one page.
        assert_eq!(unsafe { libc::ftruncate(fd.as_raw_fd(), 4096) }, 0);
        if seal {
            add_seals(std::os::fd::AsFd::as_fd(&fd), libc::F_SEAL_SHRINK).unwrap();
        }
        fd
    }

    /// The real kernel refuses an unsealed memfd as a dma-buf source (`EINVAL`), and takes the
    /// same memfd once it is sealed against shrinking. The second half is the control: the
    /// refusal is the seal's, not the device's or the size's. A stand-in that seals for the caller
    /// would pass where the real device fails; this test fails then.
    #[test]
    fn udmabuf_refuses_an_unsealed_memfd_and_takes_a_sealed_one() {
        let Some(dev) = udmabuf_device() else {
            return;
        };
        let unsealed = one_page_memfd(false);
        let err = create(
            std::os::fd::AsFd::as_fd(&dev),
            std::os::fd::AsFd::as_fd(&unsealed),
            0,
            4096,
        )
        .expect_err("the kernel must refuse a memfd without F_SEAL_SHRINK");
        assert_eq!(err.raw_os_error(), Some(libc::EINVAL), "{err}");

        let sealed = one_page_memfd(true);
        let buf = create(
            std::os::fd::AsFd::as_fd(&dev),
            std::os::fd::AsFd::as_fd(&sealed),
            0,
            4096,
        )
        .expect("the same memfd, sealed against shrinking, is a dma-buf");
        drop(buf);
    }

    #[test]
    fn a_memfd_takes_and_reports_seals() {
        let fd = memfd_create(c"lemnos-test").unwrap();
        assert_eq!(
            seals(std::os::fd::AsFd::as_fd(&fd)).unwrap() & libc::F_SEAL_SHRINK,
            0
        );
        add_seals(std::os::fd::AsFd::as_fd(&fd), libc::F_SEAL_SHRINK).unwrap();
        assert_ne!(
            seals(std::os::fd::AsFd::as_fd(&fd)).unwrap() & libc::F_SEAL_SHRINK,
            0
        );
    }
}
