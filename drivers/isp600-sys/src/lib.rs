//! The ISP600 driver's unsafe layer, and nothing else: the vendor kernel's requests on the ISP and
//! statistics subdevices of its `vin` camera driver whose arguments carry a pointer the kernel
//! follows, with their layouts and request numbers.
//!
//! - [`load`] hands the kernel one load image: the block's register image and tables, which the
//!   kernel copies during the call and gives the block at its next parameter load.
//! - [`configure_statistics`] and [`enable_statistics`] set the statistics subdevice going.
//! - [`request_statistics`] copies the oldest finished statistics out.
//!
//! Each takes memory of the exact size the kernel copies, as a fixed-size array, and builds the
//! request's pointer from it itself: no caller's number becomes an address or a length here. The
//! kernel reads or writes that memory only during the call (`copy_from_user` and `copy_to_user`
//! inside the request's handler), so a borrow for the call is enough.
//!
//! # Source
//!
//! Interface facts only, no code copied, from the vendor kernel source in `radxa/allwinner-bsp`
//! on GitHub, branch `cubie-aiot-v1.4.6` at `2045a3ca2a01f088c0314dc924bda59d154e363e` (GPL-2.0;
//! the uAPI header is `GPL-2.0-or-later WITH Linux-syscall-note`):
//!
//! - `include/uapi/media/sunxi_camera_v2.h`: `struct isp_table_reg_map`, `struct
//!   vin_isp_stat_data`, `struct vin_isp_h3a_config`, `struct vin_isp_stat_event_status` and the
//!   request numbers `VIDIOC_VIN_ISP_LOAD_REG`, `VIDIOC_VIN_ISP_H3A_CFG`,
//!   `VIDIOC_VIN_ISP_STAT_REQ`, `VIDIOC_VIN_ISP_STAT_EN` (`BASE_VIDIOC_PRIVATE` is 192, from
//!   `videodev2.h`).
//! - `drivers/vin/vin-isp/sunxi_isp.c`: `__isp_set_load_reg` refuses a size past
//!   `ISP_LOAD_DRAM_SIZE` and copies `size` bytes from `addr` during the call.
//! - `drivers/vin/vin-stat/vin_h3a.c`: `isp_stat_buf_get` (the `ISP_600` branch) refuses a
//!   `buf_size` short of the statistics buffer plus `ISP_SAVE_LOAD_STATISTIC_SIZE`, copies the
//!   statistics buffer and then the save-load statistics after it during the call, and reports the
//!   statistics buffer's size alone in `buf_size`; `-EBUSY` when no buffer is finished;
//!   `isp_stat_config` answers `ISP_STAT_TOTAL_SIZE`; `h3a_ioctl` reads `STAT_EN`'s argument as
//!   one byte.
//! - `drivers/vin/vin-isp/isp600/isp600_reg_cfg.h`: `ISP_LOAD_DRAM_SIZE` (`0x7700` when the chip
//!   is not `CONFIG_ARCH_SUN55IW6`), `ISP_STAT_TOTAL_SIZE` (`0x6a00`),
//!   `ISP_SAVE_LOAD_STATISTIC_SIZE` (`0x3300`).
//!
//! `layout-check/run.sh <allwinner-bsp checkout>` compiles a C program against that header and
//! diffs the sizes, offsets and numbers it prints against `src/layout.rs`.
//!
//! Hardware status: untested on board. The check is `testing/board-runs/E13.md`.

#![cfg_attr(not(any(test, target_os = "linux")), no_std)]

#[cfg(target_os = "linux")]
mod call;
mod layout;

#[cfg(target_os = "linux")]
pub use call::{Received, configure_statistics, enable_statistics, load, request_statistics};

use core::ffi::c_void;
use core::mem::size_of;

/// Bytes of a load image (`ISP_LOAD_DRAM_SIZE`): what [`load`] hands the kernel.
pub const LOAD_BYTES: usize = 0x7700;
/// Bytes of the statistics buffer the block saves each frame (`ISP_STAT_TOTAL_SIZE`).
pub const SAVE_BYTES: usize = 0x6a00;
/// Bytes of the save-load statistics the kernel copies after it
/// (`ISP_SAVE_LOAD_STATISTIC_SIZE`).
pub const SAVE_LOAD_STATISTICS_BYTES: usize = 0x3300;
/// Bytes [`request_statistics`] fills: the saved buffer, then the save-load statistics.
pub const STATISTICS_BYTES: usize = SAVE_BYTES + SAVE_LOAD_STATISTICS_BYTES;

/// `struct isp_table_reg_map`: the argument of `VIDIOC_VIN_ISP_LOAD_REG`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TableRegMap {
    /// `addr`: the image the kernel copies from.
    pub addr: *mut c_void,
    /// `size`: bytes to copy, at most [`LOAD_BYTES`].
    pub size: u32,
}

/// `struct vin_isp_stat_data`: the argument of `VIDIOC_VIN_ISP_STAT_REQ`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StatData {
    /// `buf`: where the kernel copies the statistics.
    pub buf: *mut c_void,
    /// `buf_size`: bytes at `buf` on the way in; the saved buffer's bytes on the way out.
    pub buf_size: u32,
    /// `frame_number`: the frame the statistics are of, as the kernel counts them.
    pub frame_number: u32,
    /// `config_counter`: not set by this kernel.
    pub config_counter: u32,
}

/// `struct vin_isp_h3a_config`: the argument of `VIDIOC_VIN_ISP_H3A_CFG`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct H3aConfig {
    /// `buf_size`: set by the kernel to the statistics buffer's size.
    pub buf_size: u32,
    /// `config_counter`: not read by this kernel.
    pub config_counter: u32,
}

/// `struct vin_isp_stat_event_status`: the payload of the statistics event. Pinned for callers
/// that subscribe; the driver reads statistics by request instead.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatEventStatus {
    /// `frame_number`.
    pub frame_number: u32,
    /// `config_counter`.
    pub config_counter: u16,
    /// `buf_err`: no statistics buffer was free for the frame.
    pub buf_err: u8,
}

/// `BASE_VIDIOC_PRIVATE` (`videodev2.h`).
pub const BASE_VIDIOC_PRIVATE: u32 = 192;

/// `_IOWR(type, nr, size)` exactly as the C macro builds it: the fields are or-ed in unmasked, so
/// a number past 255 carries into the type byte, as `BASE_VIDIOC_PRIVATE + 70` does.
const fn iowr(ty: u8, nr: u32, size: usize) -> u32 {
    (3 << 30) | ((size as u32) << 16) | ((ty as u32) << 8) | nr
}

/// `VIDIOC_VIN_ISP_H3A_CFG`.
pub const VIDIOC_VIN_ISP_H3A_CFG: u32 =
    iowr(b'V', BASE_VIDIOC_PRIVATE + 31, size_of::<H3aConfig>());
/// `VIDIOC_VIN_ISP_STAT_REQ`.
pub const VIDIOC_VIN_ISP_STAT_REQ: u32 =
    iowr(b'V', BASE_VIDIOC_PRIVATE + 32, size_of::<StatData>());
/// `VIDIOC_VIN_ISP_STAT_EN`, whose argument is declared `unsigned int`.
pub const VIDIOC_VIN_ISP_STAT_EN: u32 = iowr(b'V', BASE_VIDIOC_PRIVATE + 33, size_of::<u32>());
/// `VIDIOC_VIN_ISP_LOAD_REG`.
pub const VIDIOC_VIN_ISP_LOAD_REG: u32 =
    iowr(b'V', BASE_VIDIOC_PRIVATE + 70, size_of::<TableRegMap>());

/// `V4L2_EVENT_VIN_H3A`: statistics finished, on the statistics subdevice.
pub const V4L2_EVENT_VIN_H3A: u32 = 0x0800_0000 | 0x100 | 0x1;

#[cfg(test)]
mod tests;
