//! The kernel requests this crate makes: the partition re-read on a block device (`BLKRRPART`), the
//! NAND requests on an MTD character device (the bad block table, erase, the ECC counts and the
//! chip's geometry), and the UBI requests the first-flash check uses to format, attach and fill a
//! volume the way the kernel's own UBI does (`ubi`). Each is one operation in one `unsafe` block with
//! a `SAFETY` comment, and the layout and number tests pin them.
//!
//! The numbers are from the kernel's `include/uapi/linux/fs.h` (`BLKRRPART` is `_IO(0x12, 95)`)
//! and `include/uapi/mtd/mtd-abi.h`: `MEMGETBADBLOCK` `_IOW('M', 11, loff_t)`, `MEMSETBADBLOCK`
//! `_IOW('M', 12, loff_t)`, `MEMERASE` `_IOW('M', 2, struct erase_info_user)` and `ECCGETSTATS`
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
/// `MEMSETBADBLOCK`: `_IOW('M', 12, loff_t)`. The argument is a pointer to the byte offset.
pub const MEMSETBADBLOCK: u32 = 0x4008_4D0C;
/// `MEMERASE`: `_IOW('M', 2, struct erase_info_user)`.
pub const MEMERASE: u32 = 0x4008_4D02;
/// `ECCGETSTATS`: `_IOR('M', 18, struct mtd_ecc_stats)`.
pub const ECCGETSTATS: u32 = 0x8010_4D12;
/// `MEMGETINFO`: `_IOR('M', 1, struct mtd_info_user)`.
pub const MEMGETINFO: u32 = 0x8020_4D01;

/// `struct mtd_info_user`: the chip's type, flags and geometry, as the kernel reports them.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MtdInfo {
    /// The MTD type (`MTD_NANDFLASH` is 4).
    pub kind: u8,
    /// The MTD flags (`MTD_WRITEABLE` and the like).
    pub flags: u32,
    /// The device's size in bytes.
    pub size: u32,
    /// The erase block in bytes.
    pub erasesize: u32,
    /// The write unit (page) in bytes.
    pub writesize: u32,
    /// The out-of-band bytes per write unit.
    pub oobsize: u32,
    /// Obsolete; the kernel leaves it zero.
    pub padding: u64,
}

/// Reads the MTD device `fd`'s type and geometry (`MEMGETINFO`).
pub fn mtd_info(fd: BorrowedFd<'_>) -> io::Result<MtdInfo> {
    let mut info = MtdInfo::default();
    // SAFETY: `MEMGETINFO` writes one `mtd_info_user` through the pointer, which points at `info`,
    // a live local of that `repr(C)` type sized for the kernel's 32 bytes.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), MEMGETINFO as _, &mut info as *mut MtdInfo) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

/// The UBI requests, from the kernel's `include/uapi/mtd/ubi-user.h` (`UBI_IOC_MAGIC` 'o', volume
/// requests 'O'). Attaching formats a blank device, as the kernel does on its own.
pub mod ubi {
    use super::*;

    /// `UBI_IOCATT`: `_IOW('o', 64, struct ubi_attach_req)`.
    pub const IOCATT: u32 = 0x4018_6F40;
    /// `UBI_IOCDET`: `_IOW('o', 65, __s32)`.
    pub const IOCDET: u32 = 0x4004_6F41;
    /// `UBI_IOCMKVOL`: `_IOW('o', 0, struct ubi_mkvol_req)`.
    pub const IOCMKVOL: u32 = 0x4098_6F00;
    /// `UBI_IOCVOLUP`: `_IOW('O', 0, __s64)`: the pointer is to the image's size in bytes.
    pub const IOCVOLUP: u32 = 0x4008_4F00;
    /// `UBI_VOL_NUM_AUTO` and `UBI_DEV_NUM_AUTO`: the kernel picks the number.
    pub const AUTO: i32 = -1;
    /// `UBI_DYNAMIC_VOLUME`.
    pub const DYNAMIC: i8 = 3;
    /// `UBI_STATIC_VOLUME`.
    pub const STATIC: i8 = 4;

    /// `struct ubi_attach_req`.
    #[repr(C)]
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
    pub struct AttachReq {
        /// The UBI device number (`AUTO` to let the kernel pick).
        pub ubi_num: i32,
        /// The MTD device to attach.
        pub mtd_num: i32,
        /// The VID header offset (0 for the kernel's default).
        pub vid_hdr_offset: i32,
        /// The most bad blocks expected per 1024 (0 for the default).
        pub max_beb_per1024: i16,
        /// Non-zero to leave fastmap off.
        pub disable_fm: i8,
        /// Non-zero to reserve free blocks for the pools.
        pub need_resv_pool: i8,
        /// The wear-levelling threshold (0 for the default).
        pub wl_threshold: i32,
        /// Reserved; zero.
        pub padding: [i8; 4],
    }

    /// `struct ubi_mkvol_req` (`__packed` in the kernel; its fields are naturally aligned, so the
    /// layout is the same).
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MkvolReq {
        /// The volume number (`AUTO` to let the kernel pick); the kernel writes the one it chose.
        pub vol_id: i32,
        /// The alignment of the volume's logical erase blocks (1 for none).
        pub alignment: i32,
        /// The volume's size in bytes.
        pub bytes: i64,
        /// `DYNAMIC` or `STATIC`.
        pub vol_type: i8,
        /// Volume flags (zero).
        pub flags: u8,
        /// The name's length in bytes.
        pub name_len: i16,
        /// Reserved; zero.
        pub padding2: [i8; 4],
        /// The name, NUL-padded.
        pub name: [u8; 128],
    }

    impl MkvolReq {
        /// A request for a volume of `bytes` bytes named `name` (at most 127 bytes).
        pub fn new(name: &str, bytes: i64, vol_type: i8) -> io::Result<Self> {
            let mut name_bytes = [0u8; 128];
            if name.is_empty() || name.len() > 127 {
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            }
            name_bytes[..name.len()].copy_from_slice(name.as_bytes());
            Ok(Self {
                vol_id: AUTO,
                alignment: 1,
                bytes,
                vol_type,
                flags: 0,
                name_len: name.len() as i16,
                padding2: [0; 4],
                name: name_bytes,
            })
        }
    }

    /// Attaches MTD device `mtd` to a new UBI device through the control node `ctrl`, with the
    /// kernel's defaults except the bad block allowance (20 per 1024), and returns the UBI device number the
    /// kernel gave it.
    pub fn attach(ctrl: BorrowedFd<'_>, mtd: i32) -> io::Result<i32> {
        // The allowance is stated: the kernel's default (20 per 1024) is the chip's, and a larger
        // figure reserves most of a small partition's blocks for bad ones (UBI reserves against the
        // whole chip's size).
        let mut req = AttachReq {
            ubi_num: AUTO,
            mtd_num: mtd,
            max_beb_per1024: 20,
            ..AttachReq::default()
        };
        // SAFETY: `UBI_IOCATT` reads one `ubi_attach_req` through the pointer, which points at `req`,
        // a live local of that `repr(C)` type, and returns the new device number as the result.
        let rc = unsafe { libc::ioctl(ctrl.as_raw_fd(), IOCATT as _, &mut req as *mut AttachReq) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(rc)
    }

    /// Detaches UBI device `ubi_num` through the control node `ctrl`.
    pub fn detach(ctrl: BorrowedFd<'_>, ubi_num: i32) -> io::Result<()> {
        let mut num = ubi_num;
        // SAFETY: `UBI_IOCDET` reads one `__s32` through the pointer, which points at `num`, a live
        // local of that type.
        let rc = unsafe { libc::ioctl(ctrl.as_raw_fd(), IOCDET as _, &mut num as *mut i32) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Creates a volume on the UBI device node `dev` and returns its volume number.
    pub fn mkvol(dev: BorrowedFd<'_>, req: &mut MkvolReq) -> io::Result<i32> {
        // SAFETY: `UBI_IOCMKVOL` reads and writes one `ubi_mkvol_req` through the pointer, which
        // points at `req`, a live borrow of that `repr(C)` type for the duration of the call.
        let rc = unsafe { libc::ioctl(dev.as_raw_fd(), IOCMKVOL as _, req as *mut MkvolReq) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(req.vol_id)
    }

    /// Starts an update of the volume whose node is `vol`: the volume takes `bytes` bytes, which the
    /// caller then writes to it.
    pub fn volume_update(vol: BorrowedFd<'_>, bytes: i64) -> io::Result<()> {
        let mut size = bytes;
        // SAFETY: `UBI_IOCVOLUP` reads one `__s64` through the pointer, which points at `size`, a
        // live local of that type.
        let rc = unsafe { libc::ioctl(vol.as_raw_fd(), IOCVOLUP as _, &mut size as *mut i64) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

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
        assert_eq!(MEMSETBADBLOCK, ioc(WRITE, b'M' as u32, 12, 8));
        assert_eq!(MEMERASE, ioc(WRITE, b'M' as u32, 2, 8));
        assert_eq!(ECCGETSTATS, ioc(READ, b'M' as u32, 18, 16));
    }

    #[test]
    fn the_mtd_structs_have_the_kernels_sizes() {
        assert_eq!(std::mem::size_of::<EraseInfo>(), 8);
        assert_eq!(std::mem::size_of::<EccStats>(), 16);
        assert_eq!(std::mem::size_of::<MtdInfo>(), 32);
        assert_eq!(std::mem::offset_of!(MtdInfo, erasesize), 12);
        assert_eq!(std::mem::offset_of!(MtdInfo, padding), 24);
        assert_eq!(MEMGETINFO, ioc(2, b'M' as u32, 1, 32));
    }

    #[test]
    fn the_ubi_requests_are_the_kernels_numbers_and_sizes() {
        const WRITE: u32 = 1;
        assert_eq!(ubi::IOCATT, ioc(WRITE, b'o' as u32, 64, 24));
        assert_eq!(ubi::IOCDET, ioc(WRITE, b'o' as u32, 65, 4));
        assert_eq!(ubi::IOCMKVOL, ioc(WRITE, b'o' as u32, 0, 152));
        assert_eq!(ubi::IOCVOLUP, ioc(WRITE, b'O' as u32, 0, 8));
        assert_eq!(std::mem::size_of::<ubi::AttachReq>(), 24);
        assert_eq!(std::mem::size_of::<ubi::MkvolReq>(), 152);
        assert_eq!(std::mem::offset_of!(ubi::MkvolReq, name), 24);
    }
}
