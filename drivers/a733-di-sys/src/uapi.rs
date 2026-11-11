//! The deinterlacer's character device as the vendor kernel defines it: the `DI_IOC_*` requests
//! and their structures.
//!
//! Transcribed (interface facts only, no code copied) from the Radxa `allwinner-bsp` tree at
//! commit `a842bb8dc74608ba13975c97c31b9518f1976c01` (branch `cubie-aiot-v1.5.0`), the driver for
//! this chip's block (`DI301`, built for `CONFIG_ARCH_SUN60IW2` in `drivers/di/drv_div3x/Makefile`):
//!
//! - `drivers/di/drv_div3x/sunxi_di.h` (`GPL-2.0-or-later`): the requests, the structures and the
//!   mode values.
//! - `drivers/di/drv_div3x/di_fops.c`, `di_client.c`, `lowlevel_v3x/di301/di301.c` and
//!   `lowlevel_v3x/di301/di301_reg.h`: what the kernel does with each field (below).
//! - `drivers/di/common/di_utils.c`: the formats the block takes.
//! - `drivers/di/drv_div3x/di_driver.c`: the node's name.
//!
//! The layout is the one built without `CONFIG_SUNXI_DI_SINGEL_FILE` (its default). With that
//! option the kernel's `di_fb` and `di_process_fb_arg` are larger, and since the kernel dispatches
//! by request number alone and copies only the size the number encodes, a request built here would
//! be read past its end. The board check confirms the option is off before anything runs.
//!
//! Sizes and offsets were checked against gcc on x86_64 and clang for aarch64; the tests pin them.

#[cfg(target_os = "linux")]
use lemnos_linux_sys::{ioc, ior, iow, iowr};

/// The device node.
pub const DEVICE: &str = "/dev/deinterlace";

/// The request type byte, `DI_IOC_MAGIC`.
pub const MAGIC: u8 = b'D';

/// `DI_IOC_GET_VERSION`.
#[cfg(target_os = "linux")]
pub const GET_VERSION: u32 = ior::<Version>(MAGIC, 0x0);
/// `DI_IOC_RESET`: forgets the checked settings and the frames.
#[cfg(target_os = "linux")]
pub const RESET: u32 = ioc(0, MAGIC, 0x1, 0);
/// `DI_IOC_CHECK_PARA`: checks the settings and picks the mode; needs a reset before it is asked
/// again.
#[cfg(target_os = "linux")]
pub const CHECK_PARA: u32 = ioc(0, MAGIC, 0x2, 0);
/// `DI_IOC_SET_TIMEOUT`.
#[cfg(target_os = "linux")]
pub const SET_TIMEOUT: u32 = iow::<TimeoutNs>(MAGIC, 0x3);
/// `DI_IOC_SET_VIDEO_SIZE`.
#[cfg(target_os = "linux")]
pub const SET_VIDEO_SIZE: u32 = iow::<Size>(MAGIC, 0x4);
/// `DI_IOC_SET_DIT_MODE`.
#[cfg(target_os = "linux")]
pub const SET_DIT_MODE: u32 = iow::<DitMode>(MAGIC, 0x5);
/// `DI_IOC_SET_TNR_MODE`.
#[cfg(target_os = "linux")]
pub const SET_TNR_MODE: u32 = iow::<TnrMode>(MAGIC, 0x6);
/// `DI_IOC_SET_FMD_ENABLE`.
#[cfg(target_os = "linux")]
pub const SET_FMD_ENABLE: u32 = iow::<FmdEnable>(MAGIC, 0x7);
/// `DI_IOC_PROCESS_FB`: runs one job and waits for it. Answers 1
/// (`FB_PROCESS_ERROR_INTERLACE_TYPE`) when the motion detector found the input progressive.
#[cfg(target_os = "linux")]
pub const PROCESS_FB: u32 = iow::<ProcessFbArg>(MAGIC, 0x8);
/// `DI_IOC_SET_VIDEO_CROP`: the output window, also copied to every other crop.
#[cfg(target_os = "linux")]
pub const SET_VIDEO_CROP: u32 = iow::<Rect>(MAGIC, 0x9);
/// `DI_IOC_MEM_REQUEST`: allocates kernel memory and answers its physical address. Never issued.
#[cfg(target_os = "linux")]
pub const MEM_REQUEST: u32 = iowr::<MemArg>(MAGIC, 0x10);
/// `DI_IOC_MEM_RELEASE`. Never issued.
#[cfg(target_os = "linux")]
pub const MEM_RELEASE: u32 = iowr::<MemArg>(MAGIC, 0x11);
/// `DI_IOC_SET_DEMO_CROP`.
#[cfg(target_os = "linux")]
pub const SET_DEMO_CROP: u32 = iow::<DemoCropArg>(MAGIC, 0x12);
/// `DI_IOC_GET_TNRPARA`.
#[cfg(target_os = "linux")]
pub const GET_TNRPARA: u32 = ior::<TnrParam>(MAGIC, 0x13);
/// `DI_IOC_SET_TNRPARA`.
#[cfg(target_os = "linux")]
pub const SET_TNRPARA: u32 = iow::<TnrParam>(MAGIC, 0x14);
/// `DI_IOC_SET_MEDIAID`.
#[cfg(target_os = "linux")]
pub const SET_MEDIAID: u32 = ioc(0, MAGIC, 0x15, 0);

/// `DI_DIT_INTP_MODE_*`.
pub mod intp {
    /// `DI_DIT_INTP_MODE_INVALID`: no deinterlacing (noise reduction only).
    pub const INVALID: u32 = 0;
    /// `DI_DIT_INTP_MODE_WEAVE`.
    pub const WEAVE: u32 = 1;
    /// `DI_DIT_INTP_MODE_BOB`.
    pub const BOB: u32 = 2;
    /// `DI_DIT_INTP_MODE_MOTION`.
    pub const MOTION: u32 = 3;
}

/// `DI_DIT_OUT_*FRAME`.
pub mod out {
    /// `DI_DIT_OUT_0FRAME`.
    pub const FRAMES_0: u32 = 0;
    /// `DI_DIT_OUT_1FRAME`.
    pub const FRAMES_1: u32 = 1;
    /// `DI_DIT_OUT_2FRAME`.
    pub const FRAMES_2: u32 = 2;
}

/// `DI_TNR_MODE_*` and `DI_TNR_LEVEL_*`.
pub mod tnr {
    /// `DI_TNR_MODE_INVALID`: off.
    pub const MODE_INVALID: u32 = 0;
    /// `DI_TNR_MODE_ADAPTIVE`.
    pub const MODE_ADAPTIVE: u32 = 1;
    /// `DI_TNR_MODE_FIX`.
    pub const MODE_FIX: u32 = 2;
    /// `DI_TNR_LEVEL_HIGH`.
    pub const LEVEL_HIGH: u32 = 0;
    /// `DI_TNR_LEVEL_MIDDLE`.
    pub const LEVEL_MIDDLE: u32 = 1;
    /// `DI_TNR_LEVEL_LOW`.
    pub const LEVEL_LOW: u32 = 2;
}

/// `FB_PROCESS_ERROR_INTERLACE_TYPE`: what `DI_IOC_PROCESS_FB` answers for progressive input.
pub const PROCESSED_PROGRESSIVE: i32 = 1;

/// The DRM four-character codes of the formats the block takes (`di_format_get_planar_num`).
pub mod fourcc {
    const fn code(c: &[u8; 4]) -> u32 {
        u32::from_le_bytes(*c)
    }
    /// `DRM_FORMAT_YUV420`.
    pub const YUV420: u32 = code(b"YU12");
    /// `DRM_FORMAT_YVU420`.
    pub const YVU420: u32 = code(b"YV12");
    /// `DRM_FORMAT_YUV422`.
    pub const YUV422: u32 = code(b"YU16");
    /// `DRM_FORMAT_YVU422`.
    pub const YVU422: u32 = code(b"YV16");
    /// `DRM_FORMAT_NV12`.
    pub const NV12: u32 = code(b"NV12");
    /// `DRM_FORMAT_NV21`.
    pub const NV21: u32 = code(b"NV21");
    /// `DRM_FORMAT_NV16`.
    pub const NV16: u32 = code(b"NV16");
    /// `DRM_FORMAT_NV61`.
    pub const NV61: u32 = code(b"NV61");
}

/// `struct di_version`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Version {
    /// `version_major`.
    pub major: u32,
    /// `version_minor`.
    pub minor: u32,
    /// `version_patchlevel`.
    pub patchlevel: u32,
    /// `ip_version`.
    pub ip_version: u32,
}

/// `struct di_timeout_ns`. Zero leaves a timeout as it was (3 s each when the file opens).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimeoutNs {
    /// `wait4start`.
    pub wait4start: u64,
    /// `wait4finish`.
    pub wait4finish: u64,
}

/// `struct di_dit_mode`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DitMode {
    /// An [`intp`] value.
    pub intp_mode: u32,
    /// An [`out`] value.
    pub out_frame_mode: u32,
}

/// `struct di_tnr_mode`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TnrMode {
    /// A [`tnr`] mode.
    pub mode: u32,
    /// A [`tnr`] level.
    pub level: u32,
}

/// `struct di_fmd_enable`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FmdEnable {
    /// 1 enables film-mode detection.
    pub en: u32,
}

/// `struct di_size`, in pixels.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size {
    /// `width`.
    pub width: u32,
    /// `height`.
    pub height: u32,
}

/// `struct di_rect`: `right` and `bottom` are one past the window.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    /// `left`.
    pub left: u32,
    /// `top`.
    pub top: u32,
    /// `right`.
    pub right: u32,
    /// `bottom`.
    pub bottom: u32,
}

/// `struct di_buf`. With a dma-buf the addresses are byte offsets into it, and the kernel adds
/// the buffer's bus address to `y_addr` always and to `cb_addr` and `cr_addr` only when they are
/// nonzero. Strides are in bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Buf {
    /// `y_addr`.
    pub y_addr: u64,
    /// `cb_addr`.
    pub cb_addr: u64,
    /// `cr_addr`.
    pub cr_addr: u64,
    /// `ystride`.
    pub ystride: u32,
    /// `cstride`.
    pub cstride: u32,
}

/// `struct di_fb`. A negative `dma_buf_fd` makes the addresses physical; this crate never builds
/// one.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fb {
    /// A [`fourcc`] code.
    pub format: u32,
    /// The dma-buf.
    pub dma_buf_fd: i32,
    /// Where the planes are.
    pub buf: Buf,
    /// The image's size; the block itself uses the video size.
    pub size: Size,
}

/// `struct di_process_fb_arg`: one job.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessFbArg {
    /// `is_interlace`.
    pub is_interlace: u8,
    /// `is_pulldown`.
    pub is_pulldown: u8,
    /// `top_field_first`.
    pub top_field_first: u8,
    /// `base_field`: 0 the top field, 1 the bottom (by the header's comment).
    pub base_field: u8,
    /// `in_fb0`.
    pub in_fb0: Fb,
    /// `in_fb1`.
    pub in_fb1: Fb,
    /// `in_fb2`.
    pub in_fb2: Fb,
    /// `out_dit_fb0`.
    pub out_dit_fb0: Fb,
    /// `out_dit_fb1`.
    pub out_dit_fb1: Fb,
    /// `out_tnr_fb0`.
    pub out_tnr_fb0: Fb,
}

/// `struct di_mem_arg`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemArg {
    /// `size`.
    pub size: u32,
    /// `handle`.
    pub handle: u32,
    /// `phys_addr`.
    pub phys_addr: u64,
}

/// `struct di_demo_crop_arg`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DemoCropArg {
    /// `dit_demo`.
    pub dit_demo: Rect,
    /// `tnr_demo`.
    pub tnr_demo: Rect,
}

/// `TNR_REG_COUNT`.
pub const TNR_REG_COUNT: usize = 28;

/// `struct tnr_module_param_pqd`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TnrParam {
    /// `id`.
    pub id: i32,
    /// `value`.
    pub value: [u32; TNR_REG_COUNT],
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn the_layouts_match_the_c_compiler() {
        assert_eq!(size_of::<Version>(), 16);
        assert_eq!(size_of::<TimeoutNs>(), 16);
        assert_eq!(size_of::<DitMode>(), 8);
        assert_eq!(size_of::<TnrMode>(), 8);
        assert_eq!(size_of::<FmdEnable>(), 4);
        assert_eq!(size_of::<Size>(), 8);
        assert_eq!(size_of::<Rect>(), 16);
        assert_eq!(size_of::<Buf>(), 32);
        assert_eq!(align_of::<Buf>(), 8);
        assert_eq!(offset_of!(Buf, cb_addr), 8);
        assert_eq!(offset_of!(Buf, cr_addr), 16);
        assert_eq!(offset_of!(Buf, ystride), 24);
        assert_eq!(offset_of!(Buf, cstride), 28);
        assert_eq!(size_of::<Fb>(), 48);
        assert_eq!(offset_of!(Fb, dma_buf_fd), 4);
        assert_eq!(offset_of!(Fb, buf), 8);
        assert_eq!(offset_of!(Fb, size), 40);
        assert_eq!(size_of::<ProcessFbArg>(), 296);
        assert_eq!(offset_of!(ProcessFbArg, base_field), 3);
        assert_eq!(offset_of!(ProcessFbArg, in_fb0), 8);
        assert_eq!(offset_of!(ProcessFbArg, in_fb1), 56);
        assert_eq!(offset_of!(ProcessFbArg, in_fb2), 104);
        assert_eq!(offset_of!(ProcessFbArg, out_dit_fb0), 152);
        assert_eq!(offset_of!(ProcessFbArg, out_dit_fb1), 200);
        assert_eq!(offset_of!(ProcessFbArg, out_tnr_fb0), 248);
        assert_eq!(size_of::<MemArg>(), 16);
        assert_eq!(size_of::<DemoCropArg>(), 32);
        assert_eq!(size_of::<TnrParam>(), 116);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_request_numbers_match_the_c_compiler() {
        assert_eq!(GET_VERSION, 0x8010_4400);
        assert_eq!(RESET, 0x4401);
        assert_eq!(CHECK_PARA, 0x4402);
        assert_eq!(SET_TIMEOUT, 0x4010_4403);
        assert_eq!(SET_VIDEO_SIZE, 0x4008_4404);
        assert_eq!(SET_DIT_MODE, 0x4008_4405);
        assert_eq!(SET_TNR_MODE, 0x4008_4406);
        assert_eq!(SET_FMD_ENABLE, 0x4004_4407);
        assert_eq!(PROCESS_FB, 0x4128_4408);
        assert_eq!(SET_VIDEO_CROP, 0x4010_4409);
        assert_eq!(MEM_REQUEST, 0xc010_4410);
        assert_eq!(MEM_RELEASE, 0xc010_4411);
        assert_eq!(SET_DEMO_CROP, 0x4020_4412);
        assert_eq!(GET_TNRPARA, 0x8074_4413);
        assert_eq!(SET_TNRPARA, 0x4074_4414);
        assert_eq!(SET_MEDIAID, 0x4415);
    }

    #[test]
    fn the_formats_are_the_drm_codes() {
        assert_eq!(fourcc::NV12, 0x3231_564e);
        assert_eq!(fourcc::NV21, 0x3132_564e);
        assert_eq!(fourcc::NV16, 0x3631_564e);
        assert_eq!(fourcc::NV61, 0x3136_564e);
        assert_eq!(fourcc::YUV420, 0x3231_5559);
        assert_eq!(fourcc::YVU420, 0x3231_5659);
        assert_eq!(fourcc::YUV422, 0x3631_5559);
        assert_eq!(fourcc::YVU422, 0x3631_5659);
        assert_eq!(
            (intp::INVALID, intp::WEAVE, intp::BOB, intp::MOTION),
            (0, 1, 2, 3)
        );
        assert_eq!((out::FRAMES_0, out::FRAMES_1, out::FRAMES_2), (0, 1, 2));
        assert_eq!(
            (tnr::MODE_INVALID, tnr::MODE_ADAPTIVE, tnr::MODE_FIX),
            (0, 1, 2)
        );
        assert_eq!(
            (tnr::LEVEL_HIGH, tnr::LEVEL_MIDDLE, tnr::LEVEL_LOW),
            (0, 1, 2)
        );
    }
}
