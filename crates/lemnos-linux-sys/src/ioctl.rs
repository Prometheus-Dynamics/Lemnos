//! Linux ioctl request encoding (the asm-generic layout used by arm, arm64,
//! x86, riscv and loongarch) and checked, `EINTR`-retrying ioctl calls.
//! Ported from Styx (`styx-kernel/src/bus/ioctl.rs`).

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

const NRBITS: u32 = 8;
const TYPEBITS: u32 = 8;
const SIZEBITS: u32 = 14;
const NRSHIFT: u32 = 0;
const TYPESHIFT: u32 = NRSHIFT + NRBITS;
const SIZESHIFT: u32 = TYPESHIFT + TYPEBITS;
const DIRSHIFT: u32 = SIZESHIFT + SIZEBITS;

const WRITE: u32 = 1;
const READ: u32 = 2;

/// `_IOC(dir, type, nr, size)`.
pub const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
    assert!(size < (1 << SIZEBITS));
    (dir << DIRSHIFT)
        | ((ty as u32) << TYPESHIFT)
        | ((nr as u32) << NRSHIFT)
        | ((size as u32) << SIZESHIFT)
}

/// `_IOR(type, nr, T)`.
pub const fn ior<T>(ty: u8, nr: u8) -> u32 {
    ioc(READ, ty, nr, size_of::<T>())
}

/// `_IOW(type, nr, T)`.
pub const fn iow<T>(ty: u8, nr: u8) -> u32 {
    ioc(WRITE, ty, nr, size_of::<T>())
}

/// `_IOWR(type, nr, T)`.
pub const fn iowr<T>(ty: u8, nr: u8) -> u32 {
    ioc(READ | WRITE, ty, nr, size_of::<T>())
}

/// Issues `ioctl(fd, request, arg)`, retrying on `EINTR`.
///
/// # Safety
///
/// `arg` must be what the kernel expects for `request` on this file: a pointer
/// to a live value of the type encoded in the request, valid for the reads and
/// writes the request performs, including through any pointers it contains.
pub(crate) unsafe fn ioctl_ptr<T>(
    fd: BorrowedFd<'_>,
    request: u32,
    arg: *mut T,
) -> io::Result<libc::c_int> {
    loop {
        // SAFETY: the caller guarantees `arg` matches `request`; `fd` is a live
        // descriptor for the duration of the call.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg) };
        if ret >= 0 {
            return Ok(ret);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Issues an ioctl whose argument is an integer passed by value.
///
/// Only for requests that take their argument by value (e.g. `I2C_SLAVE`), so
/// no memory is accessed through it; the request numbers are fixed by the
/// callers in this crate.
pub(crate) fn ioctl_int(
    fd: BorrowedFd<'_>,
    request: u32,
    arg: libc::c_ulong,
) -> io::Result<libc::c_int> {
    loop {
        // SAFETY: the requests used with this helper take their argument by
        // value, so the kernel reads no memory through it; `fd` is live.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg) };
        if ret >= 0 {
            return Ok(ret);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_kernel_encodings() {
        // Values from the kernel headers on arm64 / x86_64.
        #[repr(C)]
        struct S88([u32; 22]);
        assert_eq!(iowr::<S88>(b'V', 5), 0xc058_5605); // VIDIOC_SUBDEV_S_FMT
        #[repr(C)]
        struct S32([u32; 8]);
        assert_eq!(iow::<S32>(b'V', 90), 0x4020_565a); // VIDIOC_SUBSCRIBE_EVENT
        #[repr(C)]
        struct S68([u32; 17]);
        assert_eq!(ior::<S68>(0xb4, 0x01), 0x8044_b401); // GPIO_GET_CHIPINFO_IOCTL
    }
}
