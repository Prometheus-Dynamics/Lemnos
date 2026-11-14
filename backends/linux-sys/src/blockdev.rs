//! Partition table changes on a whole-disk block device: `BLKPG` with `BLKPG_RESIZE_PARTITION`,
//! which tells the kernel a partition is a new length without a re-read of the table.
//!
//! The numbers and structures are the kernel's `include/uapi/linux/blkpg.h`: `BLKPG` is
//! `_IO(0x12, 105)`, `BLKPG_RESIZE_PARTITION` is 3, and a partition is 152 bytes on a 64-bit
//! target (two `long long`s, an `int` and two 64-byte names, aligned to 8).

use std::io;
use std::os::fd::BorrowedFd;

use crate::ioctl::ioctl_ptr;

/// `BLKPG`: `_IO(0x12, 105)`.
pub const BLKPG: u32 = 0x1269;
/// `BLKPG_RESIZE_PARTITION`: the `op` of a [`BlkpgArg`] that changes a partition's length.
pub const BLKPG_RESIZE_PARTITION: i32 = 3;

/// The kernel's `struct blkpg_partition`.
#[repr(C)]
#[derive(Clone, Copy)]
struct BlkpgPartition {
    start: i64,
    length: i64,
    pno: i32,
    devname: [u8; 64],
    volname: [u8; 64],
}

/// The kernel's `struct blkpg_ioctl_arg`: the operation, and the partition it names by pointer.
#[repr(C)]
struct BlkpgArg {
    op: i32,
    flags: i32,
    datalen: i32,
    data: *mut BlkpgPartition,
}

/// Sets partition `number` of the disk behind `disk` to start at `start_bytes` and be
/// `length_bytes` long. The kernel refuses a partition that is in use in a way it cannot change,
/// and the caller checks the new length in `/sys/class/block` afterwards.
///
/// # Errors
///
/// The kernel's error, as the ioctl returns it.
pub fn resize_partition(
    disk: BorrowedFd<'_>,
    number: i32,
    start_bytes: i64,
    length_bytes: i64,
) -> io::Result<()> {
    let mut partition = BlkpgPartition {
        start: start_bytes,
        length: length_bytes,
        pno: number,
        devname: [0; 64],
        volname: [0; 64],
    };
    let size = i32::try_from(core::mem::size_of::<BlkpgPartition>())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut arg = BlkpgArg {
        op: BLKPG_RESIZE_PARTITION,
        flags: 0,
        datalen: size,
        data: &mut partition,
    };
    // SAFETY: `arg` names `partition`, which lives across the call; the kernel reads `op`,
    // `datalen` and the partition through `data`, and `disk` is an open block device.
    unsafe { ioctl_ptr(disk, BLKPG, &mut arg as *mut BlkpgArg) }.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kernel_structures_have_the_kernels_sizes_on_a_64_bit_target() {
        assert_eq!(BLKPG, 0x1269);
        assert_eq!(core::mem::size_of::<BlkpgPartition>(), 152);
        if cfg!(target_pointer_width = "64") {
            assert_eq!(core::mem::size_of::<BlkpgArg>(), 24);
        }
    }
}
