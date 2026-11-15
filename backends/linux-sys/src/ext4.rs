//! Growing a mounted ext4 filesystem: `EXT4_IOC_RESIZE_FS`, which takes the new block count as a
//! `u64` by pointer.
//!
//! The number is the kernel's `include/uapi/linux/ext4.h`: `_IOW('f', 16, __u64)`, which is
//! `0x40086610`. The kernel grows the filesystem to the device's end only as far as its reserved
//! group descriptor blocks allow; otherwise it refuses, and the caller reports that and tries
//! again later.

use std::io;
use std::os::fd::BorrowedFd;

use crate::ioctl::{ioctl_ptr, iow};

/// `EXT4_IOC_RESIZE_FS`: `_IOW('f', 16, __u64)`.
pub const RESIZE_FS: u32 = iow::<u64>(b'f', 16);

/// Grows the filesystem open at `dir` to `blocks` blocks.
///
/// # Errors
///
/// The kernel's error, as the ioctl returns it (`ENOSPC` when it cannot grow that far).
pub fn resize_fs(dir: BorrowedFd<'_>, blocks: u64) -> io::Result<()> {
    let mut blocks = blocks;
    // SAFETY: `blocks` is a live `u64`, the argument `RESIZE_FS` takes; `dir` is an open file on
    // the filesystem, and the kernel reads the count through the pointer and nothing else.
    unsafe { ioctl_ptr(dir, RESIZE_FS, &mut blocks as *mut u64) }.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resize_request_is_the_kernels_number() {
        assert_eq!(RESIZE_FS, 0x4008_6610);
    }
}
