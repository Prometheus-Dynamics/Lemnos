//! The rkcif driver's unsafe layer, and nothing else: the vendor kernel's request that sets how
//! the receiver's capture node lays raw samples out in memory, with its number and values.
//!
//! The vendor driver writes 10- and 12-bit raw samples either packed end to end (its default
//! on the newer chips, "compact") or one per 16-bit word. The word layout with the sample in
//! the low bits is exactly what the kernel uAPI's own formats name (`V4L2_PIX_FMT_SRGGB10`,
//! `V4L2_PIX_FMT_Y12` and the rest: "expanded to 16 bits"), so a caller that asks for one of
//! those and sets [`MemoryMode::WordLowAlign`] first gets what the format says.
//!
//! [`set_memory_mode`] and [`memory_mode`] each pass the kernel one `int` the call owns: no
//! caller's number becomes an address or a length here.
//!
//! # Source
//!
//! Interface facts only, no code copied, from the vendor kernel source in
//! `rockchip-linux/kernel` on GitHub, branch `develop-6.1` (GPL-2.0; the uAPI header is
//! `GPL-2.0+ WITH Linux-syscall-note`), read 2026-11-11:
//!
//! - `include/uapi/linux/rkcif-config.h`: `RKCIF_CMD_GET_CSI_MEMORY_MODE` is
//!   `_IOR('V', BASE_VIDIOC_PRIVATE + 0, int)`, `RKCIF_CMD_SET_CSI_MEMORY_MODE` is
//!   `_IOW('V', BASE_VIDIOC_PRIVATE + 1, int)`, and `enum cif_csi_lvds_memory` is
//!   `CSI_LVDS_MEM_COMPACT = 0`, `CSI_LVDS_MEM_WORD_LOW_ALIGN = 1`,
//!   `CSI_LVDS_MEM_WORD_HIGH_ALIGN = 2` (`BASE_VIDIOC_PRIVATE` is 192, from `videodev2.h`).
//! - `drivers/media/platform/rockchip/cif/capture.c`, `rkcif_ioctl_default`: the set request
//!   clears `is_compact` for either word mode and sets `is_high_align` for the high one; it
//!   refuses compact with `-EINVAL` on chips that don't have it (older than RV1126 on the MIPI
//!   path), and does not work the format out again, so it is made before `VIDIOC_S_FMT`.
//!   `rkcif_set_fmt`: without compact, a raw line is `ALIGN(width * bpp / 8, 8)` bytes, where
//!   `bpp` is 16 for every 10-, 12-, 14- and 16-bit format (`out_fmts`, `bpp = { 16 }`, and
//!   `rkcif_align_bits_per_pixel` rounding to multiples of 8, `CIF_RAW_STORED_BIT_WIDTH_RV1126`
//!   in `regs.h`); with compact it is `ALIGN(width * raw_bpp / 8, 256)`.
//!
//! Hardware status: untested on board. The check is `testing/board-runs/H5a.md`.

#![cfg_attr(not(any(test, target_os = "linux")), no_std)]

#[cfg(target_os = "linux")]
mod call;

#[cfg(target_os = "linux")]
pub use call::{memory_mode, set_memory_mode};

/// `BASE_VIDIOC_PRIVATE` (`videodev2.h`).
pub const BASE_VIDIOC_PRIVATE: u8 = 192;

/// `_IOC_WRITE` and `_IOC_READ` of the generic request encoding.
const WRITE: u32 = 1;
const READ: u32 = 2;

/// `_IOC(dir, 'V', nr, sizeof(int))`.
const fn ioc_int(dir: u32, nr: u8) -> u32 {
    (dir << 30) | ((core::mem::size_of::<i32>() as u32) << 16) | ((b'V' as u32) << 8) | nr as u32
}

/// `RKCIF_CMD_GET_CSI_MEMORY_MODE`: `_IOR('V', BASE_VIDIOC_PRIVATE + 0, int)`.
pub const RKCIF_CMD_GET_CSI_MEMORY_MODE: u32 = ioc_int(READ, BASE_VIDIOC_PRIVATE);
/// `RKCIF_CMD_SET_CSI_MEMORY_MODE`: `_IOW('V', BASE_VIDIOC_PRIVATE + 1, int)`.
pub const RKCIF_CMD_SET_CSI_MEMORY_MODE: u32 = ioc_int(WRITE, BASE_VIDIOC_PRIVATE + 1);

/// How the capture node lays raw samples out (`enum cif_csi_lvds_memory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum MemoryMode {
    /// `CSI_LVDS_MEM_COMPACT`: samples packed end to end, lines aligned to 256 bytes.
    Compact = 0,
    /// `CSI_LVDS_MEM_WORD_LOW_ALIGN`: one sample per 16-bit little-endian word, in its low
    /// bits: the kernel uAPI's 10- to 16-bit raw formats.
    WordLowAlign = 1,
    /// `CSI_LVDS_MEM_WORD_HIGH_ALIGN`: one sample per 16-bit word, in its high bits.
    WordHighAlign = 2,
}

impl MemoryMode {
    /// The mode the kernel answered with, if it is one.
    #[must_use]
    pub const fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            0 => Some(Self::Compact),
            1 => Some(Self::WordLowAlign),
            2 => Some(Self::WordHighAlign),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_numbers_are_the_headers() {
        // `_IOR('V', 192, int)` and `_IOW('V', 193, int)` on the generic encoding.
        assert_eq!(RKCIF_CMD_GET_CSI_MEMORY_MODE, 0x8004_56c0);
        assert_eq!(RKCIF_CMD_SET_CSI_MEMORY_MODE, 0x4004_56c1);
    }

    #[test]
    fn the_modes_are_the_enums_values() {
        assert_eq!(MemoryMode::Compact as i32, 0);
        assert_eq!(MemoryMode::WordLowAlign as i32, 1);
        assert_eq!(MemoryMode::WordHighAlign as i32, 2);
        for mode in [
            MemoryMode::Compact,
            MemoryMode::WordLowAlign,
            MemoryMode::WordHighAlign,
        ] {
            assert_eq!(MemoryMode::from_raw(mode as i32), Some(mode));
        }
        assert_eq!(MemoryMode::from_raw(3), None);
        assert_eq!(MemoryMode::from_raw(-1), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_node_that_isnt_the_receivers_refuses_and_nothing_is_written() {
        use std::os::fd::AsFd;
        // An ordinary file has no such request: the kernel says so, through the call.
        let file = std::fs::File::open("/dev/null").unwrap();
        let error = set_memory_mode(file.as_fd(), MemoryMode::WordLowAlign).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTTY));
        assert_eq!(
            memory_mode(file.as_fd()).unwrap_err().raw_os_error(),
            Some(libc::ENOTTY)
        );
    }
}
