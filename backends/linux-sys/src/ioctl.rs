//! Linux ioctl request encoding (the asm-generic layout used by arm, arm64,
//! x86, riscv and loongarch) and checked, `EINTR`-retrying ioctl calls.
//! Ported from the earlier stack (see docs/provenance.md).

use std::io;
use std::marker::PhantomData;
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

/// A plain-data uAPI type: integers, integer arrays, raw pointers, or unions and
/// `#[repr(C)]` structures of those.
///
/// # Safety
///
/// Implement only for a type every bit pattern of which is a valid value: the
/// kernel fills these in, and [`zeroed`] and [`from_bytes`] build them from bytes.
pub unsafe trait Pod: Copy {}

// SAFETY: plain integers: every bit pattern is a value.
unsafe impl Pod for i32 {}
// SAFETY: plain integers: every bit pattern is a value.
unsafe impl Pod for u32 {}
// SAFETY: plain integers: every bit pattern is a value.
unsafe impl Pod for i64 {}
// SAFETY: an array of `Pod` values has no other bytes than theirs.
unsafe impl<T: Pod, const N: usize> Pod for [T; N] {}

/// The all-zero value of a uAPI type.
pub fn zeroed<T: Pod>() -> T {
    // SAFETY: `Pod` promises every bit pattern is a valid value, zero included.
    unsafe { std::mem::zeroed() }
}

/// A uAPI value from up to its size in `bytes`, zero past them (for decoders
/// tested on arbitrary input).
pub fn from_bytes<T: Pod>(bytes: &[u8]) -> T {
    let mut value = zeroed::<T>();
    let n = bytes.len().min(size_of::<T>());
    // SAFETY: `n` bytes fit in `bytes` and in `value`, the two do not overlap,
    // and `Pod` promises any bytes are a valid value.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), (&raw mut value).cast::<u8>(), n) };
    value
}

/// An ioctl request paired with its argument type, so a call cannot pass the
/// wrong structure: the safe face of [`ioctl_ptr`]. Only requests whose argument
/// holds no user pointer the kernel follows are made this way; the others have
/// functions of their own that set the pointer themselves.
pub struct Ioctl<T> {
    nr: u32,
    name: &'static str,
    /// Refuses an argument the kernel would read a pointer out of.
    check: Option<fn(&T) -> io::Result<()>>,
    arg: PhantomData<fn(&mut T)>,
}

impl<T> Clone for Ioctl<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Ioctl<T> {}

impl<T> std::fmt::Debug for Ioctl<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

impl<T: Pod> Ioctl<T> {
    /// A request `nr`, called `name` in errors.
    pub(crate) const fn new(nr: u32, name: &'static str) -> Self {
        Self {
            nr,
            name,
            check: None,
            arg: PhantomData,
        }
    }

    /// As [`Self::new`], with `check` run on the argument before every call.
    pub(crate) const fn checked(
        nr: u32,
        name: &'static str,
        check: fn(&T) -> io::Result<()>,
    ) -> Self {
        Self {
            nr,
            name,
            check: Some(check),
            arg: PhantomData,
        }
    }

    /// The request number.
    pub const fn nr(self) -> u32 {
        self.nr
    }

    /// The request's uAPI name, for error messages.
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Issues the request on `fd` with `arg`, retrying on `EINTR`.
    pub fn call(self, fd: BorrowedFd<'_>, arg: &mut T) -> io::Result<libc::c_int> {
        if let Some(check) = self.check {
            check(arg)?;
        }
        // SAFETY: the request was made with `T`, the type its number encodes
        // (the layout tests check both), and `arg` is a live, exclusive `T`.
        // `T` holds no pointer the kernel follows, or `check` refused it.
        unsafe { ioctl_ptr(fd, self.nr, std::ptr::from_mut(arg)) }
    }
}

/// Declares typed requests: `NAME: Type = ior(type_byte, number);`.
macro_rules! ioctls {
    ($($name:ident: $ty:ty = $make:ident($byte:expr, $nr:expr);)*) => {
        $(
            #[doc = concat!("`", stringify!($name), "`.")]
            pub const $name: $crate::ioctl::Ioctl<$ty> = $crate::ioctl::Ioctl::new(
                $crate::ioctl::$make::<$ty>($byte, $nr),
                stringify!($name),
            );
        )*
    };
}
pub(crate) use ioctls;

/// Marks `#[repr(C)]` plain-data uAPI types as [`Pod`].
macro_rules! pod {
    ($($ty:ty),* $(,)?) => {
        $(
            // SAFETY: the type's fields are integers, integer arrays, raw
            // pointers, or unions and structures of those.
            unsafe impl $crate::ioctl::Pod for $ty {}
        )*
    };
}
pub(crate) use pod;

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
