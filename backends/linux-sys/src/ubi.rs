//! UBI, the volume layer over raw NAND: making, removing and renaming volumes on a UBI device
//! (`/dev/ubiN`), writing a volume whole (`/dev/ubiN_M`), and a read-only block device over a
//! volume (`/dev/ubiblockN_M`). The interface is the kernel's `mtd/ubi-user.h`.
//!
//! Every call changes the flash, so the tests cover only the structures and request numbers.

use std::fs::File;
use std::io;
use std::os::fd::AsFd;

use crate::ioctl::{ioctl_ptr, iow};

/// `UBI_MAX_VOLUME_NAME`.
pub const MAX_NAME: usize = 127;
/// `UBI_MAX_RNVOL`: volumes one rename request may name.
pub const MAX_RENAME: usize = 32;
/// `UBI_VOL_NUM_AUTO`: the kernel picks the volume number.
pub const VOL_NUM_AUTO: i32 = -1;
/// `UBI_DYNAMIC_VOLUME`.
pub const DYNAMIC_VOLUME: i8 = 3;
/// `UBI_STATIC_VOLUME`: its size is what was last written whole, and its data is checked by CRC.
pub const STATIC_VOLUME: i8 = 4;

/// `struct ubi_mkvol_req`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MkvolReq {
    /// `vol_id`: the number, or [`VOL_NUM_AUTO`]; the kernel writes the number back.
    pub vol_id: i32,
    /// `alignment`.
    pub alignment: i32,
    /// `bytes`: the size to reserve.
    pub bytes: i64,
    /// `vol_type`.
    pub vol_type: i8,
    /// `flags`.
    pub flags: u8,
    /// `name_len`.
    pub name_len: i16,
    /// `padding2`.
    pub padding2: [u8; 4],
    /// `name`, NUL-terminated.
    pub name: [u8; MAX_NAME + 1],
}

/// One entry of `struct ubi_rnvol_req`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RnvolEntry {
    /// `vol_id`.
    pub vol_id: i32,
    /// `name_len`.
    pub name_len: i16,
    /// `padding2`.
    pub padding2: [u8; 2],
    /// `name`, NUL-terminated.
    pub name: [u8; MAX_NAME + 1],
}

/// `struct ubi_rnvol_req`: renames up to [`MAX_RENAME`] volumes at once, atomically. A volume
/// renamed to the name of one that isn't in the request removes that one, in the same step.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RnvolReq {
    /// `count`.
    pub count: i32,
    /// `padding1`.
    pub padding1: [u8; 12],
    /// `ents`.
    pub ents: [RnvolEntry; MAX_RENAME],
}

/// `struct ubi_blkcreate_req`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlkcreateReq {
    /// `padding`.
    pub padding: [u8; 128],
}

crate::ioctl::pod!(MkvolReq, RnvolEntry, RnvolReq, BlkcreateReq);

/// `UBI_IOCMKVOL`, on the device.
pub const IOCMKVOL: u32 = iow::<MkvolReq>(b'o', 0);
/// `UBI_IOCRMVOL`, on the device.
pub const IOCRMVOL: u32 = iow::<i32>(b'o', 1);
/// `UBI_IOCRNVOL`, on the device.
pub const IOCRNVOL: u32 = iow::<RnvolReq>(b'o', 3);
/// `UBI_IOCVOLUP`, on a volume: the next that many bytes written replace it whole.
pub const IOCVOLUP: u32 = iow::<i64>(b'O', 0);
/// `UBI_IOCVOLCRBLK`, on a volume: a read-only block device over it.
pub const IOCVOLCRBLK: u32 = iow::<BlkcreateReq>(b'O', 7);

fn name_bytes(name: &str) -> io::Result<([u8; MAX_NAME + 1], i16)> {
    if name.is_empty() || name.len() > MAX_NAME || name.as_bytes().contains(&0) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut out = [0u8; MAX_NAME + 1];
    out[..name.len()].copy_from_slice(name.as_bytes());
    Ok((out, name.len() as i16))
}

/// The request that makes a volume `name` of `bytes` bytes, static or dynamic.
pub fn mkvol_req(name: &str, bytes: u64, is_static: bool) -> io::Result<MkvolReq> {
    let (name, name_len) = name_bytes(name)?;
    Ok(MkvolReq {
        vol_id: VOL_NUM_AUTO,
        alignment: 1,
        bytes: i64::try_from(bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?,
        vol_type: if is_static {
            STATIC_VOLUME
        } else {
            DYNAMIC_VOLUME
        },
        flags: 0,
        name_len,
        padding2: [0; 4],
        name,
    })
}

/// The request that renames each `(volume number, new name)` at once.
pub fn rnvol_req(renames: &[(i32, &str)]) -> io::Result<RnvolReq> {
    if renames.is_empty() || renames.len() > MAX_RENAME {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut req: RnvolReq = crate::ioctl::zeroed();
    req.count = renames.len() as i32;
    for (entry, (vol_id, name)) in req.ents.iter_mut().zip(renames) {
        let (name, name_len) = name_bytes(name)?;
        entry.vol_id = *vol_id;
        entry.name_len = name_len;
        entry.name = name;
    }
    Ok(req)
}

/// Makes a volume on the UBI device open as `device`. Returns its number.
pub fn make_volume(device: &File, name: &str, bytes: u64, is_static: bool) -> io::Result<i32> {
    let mut req = mkvol_req(name, bytes, is_static)?;
    // SAFETY: UBI_IOCMKVOL reads one `ubi_mkvol_req` and writes the volume number back into its
    // first field; `req` is one, alive for the call.
    unsafe { ioctl_ptr(device.as_fd(), IOCMKVOL, &raw mut req) }?;
    Ok(req.vol_id)
}

/// Removes volume `vol_id` from the UBI device open as `device`.
pub fn remove_volume(device: &File, vol_id: i32) -> io::Result<()> {
    let mut id = vol_id;
    // SAFETY: UBI_IOCRMVOL reads one `__s32`; `id` is one, alive for the call.
    unsafe { ioctl_ptr(device.as_fd(), IOCRMVOL, &raw mut id) }.map(drop)
}

/// Renames volumes on the UBI device open as `device`, all in one atomic step.
pub fn rename_volumes(device: &File, renames: &[(i32, &str)]) -> io::Result<()> {
    let mut req = rnvol_req(renames)?;
    // SAFETY: UBI_IOCRNVOL reads one `ubi_rnvol_req`; `req` is one, alive for the call.
    unsafe { ioctl_ptr(device.as_fd(), IOCRNVOL, &raw mut req) }.map(drop)
}

/// Starts replacing the volume open (for writing) as `volume`: the next `bytes` bytes written
/// to it are its new contents. Until they all are, the volume reads as being updated.
pub fn begin_update(volume: &File, bytes: u64) -> io::Result<()> {
    let mut size =
        i64::try_from(bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: UBI_IOCVOLUP reads one `__s64`; `size` is one, alive for the call.
    unsafe { ioctl_ptr(volume.as_fd(), IOCVOLUP, &raw mut size) }.map(drop)
}

/// Makes the read-only block device over the volume open as `volume` (`ubiblockN_M`).
pub fn create_block(volume: &File) -> io::Result<()> {
    let mut req: BlkcreateReq = crate::ioctl::zeroed();
    // SAFETY: UBI_IOCVOLCRBLK reads one `ubi_blkcreate_req`; `req` is one, alive for the call.
    unsafe { ioctl_ptr(volume.as_fd(), IOCVOLCRBLK, &raw mut req) }.map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_structures_are_the_kernels_size() {
        assert_eq!(size_of::<MkvolReq>(), 152);
        assert_eq!(size_of::<RnvolEntry>(), 136);
        assert_eq!(size_of::<RnvolReq>(), 16 + 32 * 136);
        assert_eq!(size_of::<BlkcreateReq>(), 128);
    }

    #[test]
    fn the_requests_are_the_kernels_numbers() {
        // From mtd/ubi-user.h, as the kernel encodes them on arm64 and x86_64.
        assert_eq!(IOCMKVOL, 0x4098_6f00);
        assert_eq!(IOCRMVOL, 0x4004_6f01);
        assert_eq!(IOCRNVOL, 0x5110_6f03);
        assert_eq!(IOCVOLUP, 0x4008_4f00);
        assert_eq!(IOCVOLCRBLK, 0x4080_4f07);
    }

    #[test]
    fn requests_carry_their_names() {
        let req = mkvol_req("slot-os-2", 8192, true).unwrap();
        assert_eq!(
            (req.vol_id, req.alignment, req.bytes),
            (VOL_NUM_AUTO, 1, 8192)
        );
        assert_eq!((req.vol_type, req.name_len), (STATIC_VOLUME, 9));
        assert_eq!(&req.name[..10], b"slot-os-2\0");
        assert!(mkvol_req("", 1, true).is_err());
        assert!(mkvol_req(&"x".repeat(128), 1, true).is_err());
        let rn = rnvol_req(&[(3, "slot-a"), (4, "b")]).unwrap();
        assert_eq!(rn.count, 2);
        assert_eq!((rn.ents[1].vol_id, rn.ents[1].name_len), (4, 1));
        assert!(rnvol_req(&[]).is_err());
    }
}
