//! The Linux watchdog's timeout requests on a watchdog device (`/dev/watchdog0`): the two
//! `WDIOC` requests that set and read the timeout, in seconds. They are the only requests this
//! crate makes. Feeding is a write of one byte, which the driver does itself.
//!
//! The request numbers are `_IOWR('W', 6, int)` and `_IOR('W', 7, int)`, from the kernel's
//! `include/uapi/linux/watchdog.h`. The layout test pins them.
//!
//! Nothing here sends the magic close. Setting the timeout does not stop the device.

#![cfg(target_os = "linux")]

use std::ffi::c_int;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

use lemnos_linux_sys::{ior, iowr};

/// `WDIOC_SETTIMEOUT`: takes the timeout in seconds, and the kernel writes back the one it applied.
pub const SET_TIMEOUT: u32 = iowr::<c_int>(b'W', 6);
/// `WDIOC_GETTIMEOUT`: the timeout in seconds the device has.
pub const GET_TIMEOUT: u32 = ior::<c_int>(b'W', 7);

/// Asks the device for `seconds` and returns the timeout it applied, which may be clamped.
pub fn set_timeout(fd: BorrowedFd<'_>, seconds: u32) -> io::Result<u32> {
    let mut value =
        c_int::try_from(seconds).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: `SET_TIMEOUT` is `_IOWR('W', 6, int)`, so the kernel reads and writes one `c_int`
    // at the pointer, and `value` is a live, exclusive `c_int` for that.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), SET_TIMEOUT as _, &mut value as *mut c_int) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    u32::try_from(value).map_err(|_| io::Error::from_raw_os_error(libc::EIO))
}

/// The timeout in seconds the device has now.
pub fn get_timeout(fd: BorrowedFd<'_>) -> io::Result<u32> {
    let mut value: c_int = 0;
    // SAFETY: `GET_TIMEOUT` is `_IOR('W', 7, int)`, so the kernel writes one `c_int` at the
    // pointer, and `value` is a live, exclusive `c_int` for that.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), GET_TIMEOUT as _, &mut value as *mut c_int) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    u32::try_from(value).map_err(|_| io::Error::from_raw_os_error(libc::EIO))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The request numbers of the kernel's `watchdog.h` on LP64 and 32-bit alike: a 4-byte `int`.
    #[test]
    fn the_requests_are_the_kernels_numbers() {
        assert_eq!(SET_TIMEOUT, 0xC004_5706);
        assert_eq!(GET_TIMEOUT, 0x8004_5707);
    }
}
