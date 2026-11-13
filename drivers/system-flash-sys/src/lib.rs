//! The kernel's request to read a whole disk's partition table again (`BLKRRPART`), which is the
//! only request this crate makes. It is `_IO(0x12, 95)` from the kernel's `include/uapi/linux/fs.h`;
//! the layout test pins it. It takes no argument.
//!
//! The kernel refuses it while a partition of the disk is in use, and then the table it holds is
//! the old one: the caller reports that, and a restart is what makes the new table authoritative.

#![cfg(target_os = "linux")]

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

/// `BLKRRPART`: `_IO(0x12, 95)`.
pub const RRPART: u32 = 0x125F;

/// Asks the kernel to read the partition table of the whole disk `fd` names again.
pub fn reread_partitions(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: `RRPART` is `_IO(0x12, 95)`, which takes no argument: the kernel reads nothing at the
    // third argument, so the zero passed for it is never dereferenced.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), RRPART as _, 0 as libc::c_long) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `_IO(0x12, 95)` in the asm-generic encoding every Linux architecture here uses.
    #[test]
    fn the_request_is_the_kernels_number() {
        assert_eq!(RRPART, (0x12 << 8) | 95);
    }
}
