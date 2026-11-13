//! The kernel requests this crate makes, and nothing else: the partition re-read on a block device
//! (`BLKRRPART`), and the four NAND requests on an MTD character device. Each is one operation in
//! one `unsafe` block with a `SAFETY` comment, and the layout and number tests pin them.
//!
//! The numbers are from the kernel's `include/uapi/linux/fs.h` (`BLKRRPART` is `_IO(0x12, 95)`)
//! and `include/uapi/mtd/mtd-abi.h`: `MEMGETBADBLOCK` `_IOW('M', 11, loff_t)`, `MEMSETBADBLOCK`
//! `_IOW('M', 14, loff_t)`, `MEMERASE` `_IOW('M', 2, struct erase_info_user)` and `ECCGETSTATS`
//! `_IOR('M', 18, struct mtd_ecc_stats)`. The structs are the kernel's, field for field.
//!
//! The kernel refuses `BLKRRPART` while a partition of the disk is in use, and then the table it
//! holds is the old one: the caller reports that, and a restart is what makes the new table
//! authoritative.

#![cfg(target_os = "linux")]

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

/// `BLKRRPART`: `_IO(0x12, 95)`.
pub const RRPART: u32 = 0x125F;
/// `MEMGETBADBLOCK`: `_IOW('M', 11, loff_t)`. The argument is a pointer to the byte offset.
pub const MEMGETBADBLOCK: u32 = 0x4008_4D0B;
/// `MEMSETBADBLOCK`: `_IOW('M', 14, loff_t)`. The argument is a pointer to the byte offset.
pub const MEMSETBADBLOCK: u32 = 0x4008_4D0E;
/// `MEMERASE`: `_IOW('M', 2, struct erase_info_user)`.
pub const MEMERASE: u32 = 0x4008_4D02;
/// `ECCGETSTATS`: `_IOR('M', 18, struct mtd_ecc_stats)`.
pub const ECCGETSTATS: u32 = 0x8010_4D12;

/// `struct erase_info_user`: the range to erase, in bytes from the start of the device.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EraseInfo {
    /// The first byte of the range.
    pub start: u32,
    /// The range's length in bytes.
    pub length: u32,
}

/// `struct mtd_ecc_stats`: the chip's running ECC counts. `corrected` grows by each bit flip the
/// ECC fixed, and `failed` by each read it could not correct.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EccStats {
    /// Bit flips the ECC corrected.
    pub corrected: u32,
    /// Reads the ECC could not correct.
    pub failed: u32,
    /// Blocks the chip found bad.
    pub badblocks: u32,
    /// Blocks holding the bad block table.
    pub bbtblocks: u32,
}

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

/// Whether the erase block at byte `offset` of the MTD device `fd` is marked bad (1 is bad, 0 good).
pub fn is_bad_block(fd: BorrowedFd<'_>, offset: u64) -> io::Result<bool> {
    let mut at = offset as i64;
    // SAFETY: `MEMGETBADBLOCK` reads one `loff_t` through the pointer, which points at `at`, a live
    // local of that type, for the duration of the call.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), MEMGETBADBLOCK as _, &mut at as *mut i64) };
    match rc {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(io::Error::last_os_error()),
    }
}

/// Marks the erase block at byte `offset` of the MTD device `fd` bad, in the chip's bad block table.
pub fn mark_bad_block(fd: BorrowedFd<'_>, offset: u64) -> io::Result<()> {
    let mut at = offset as i64;
    // SAFETY: `MEMSETBADBLOCK` reads one `loff_t` through the pointer, which points at `at`, a live
    // local of that type, for the duration of the call.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), MEMSETBADBLOCK as _, &mut at as *mut i64) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Erases `length` bytes at byte `start` of the MTD device `fd`. Both are erase-block aligned.
pub fn erase(fd: BorrowedFd<'_>, start: u32, length: u32) -> io::Result<()> {
    let mut info = EraseInfo { start, length };
    // SAFETY: `MEMERASE` reads one `erase_info_user` through the pointer, which points at `info`,
    // a live local of that `repr(C)` type, for the duration of the call.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), MEMERASE as _, &mut info as *mut EraseInfo) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The chip's running ECC counts on the MTD device `fd`.
pub fn ecc_stats(fd: BorrowedFd<'_>) -> io::Result<EccStats> {
    let mut stats = EccStats::default();
    // SAFETY: `ECCGETSTATS` writes one `mtd_ecc_stats` through the pointer, which points at `stats`,
    // a live local of that `repr(C)` type sized for the kernel's 16 bytes.
    let rc = unsafe {
        libc::ioctl(
            fd.as_raw_fd(),
            ECCGETSTATS as _,
            &mut stats as *mut EccStats,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `_IO(0x12, 95)` in the asm-generic encoding every Linux architecture here uses.
    #[test]
    fn the_request_is_the_kernels_number() {
        assert_eq!(RRPART, (0x12 << 8) | 95);
    }

    /// The `_IOC` encoding: direction << 30 | size << 16 | type << 8 | number.
    const fn ioc(dir: u32, ty: u32, nr: u32, size: u32) -> u32 {
        (dir << 30) | (size << 16) | (ty << 8) | nr
    }

    #[test]
    fn the_mtd_requests_are_the_kernels_numbers() {
        const READ: u32 = 2;
        const WRITE: u32 = 1;
        assert_eq!(MEMGETBADBLOCK, ioc(WRITE, b'M' as u32, 11, 8));
        assert_eq!(MEMSETBADBLOCK, ioc(WRITE, b'M' as u32, 14, 8));
        assert_eq!(MEMERASE, ioc(WRITE, b'M' as u32, 2, 8));
        assert_eq!(ECCGETSTATS, ioc(READ, b'M' as u32, 18, 16));
    }

    #[test]
    fn the_mtd_structs_have_the_kernels_sizes() {
        assert_eq!(std::mem::size_of::<EraseInfo>(), 8);
        assert_eq!(std::mem::size_of::<EccStats>(), 16);
    }
}
