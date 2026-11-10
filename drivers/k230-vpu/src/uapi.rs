//! The codec block's interface as the SDK kernel's `mvx` driver defines it, beyond standard V4L2:
//! the driver's name, its private pixel formats, buffer flags and controls, and its private
//! requests and their structures.
//!
//! Transcribed (interface facts only, no code copied) from the Kendryte K230 Linux SDK
//! (`github.com/kendryte/k230_linux_sdk`) at commit `fc6cc2151d54cb307be47f81815023f7ffb0eae6`,
//! whose `buildroot-overlay/configs/k230_canmv_defconfig` builds the kernel from
//! `github.com/ruyisdk/linux-xuantie-kernel` at `7d4e1f444f461dbe3833bd99a4640e7b6c2cd529`
//! (none of the SDK's kernel patches touch the codec). In that kernel,
//! `drivers/media/platform/canaan/vpu/`:
//!
//! - `mvx-v4l2-controls.h` (`GPL-2.0`): the pixel formats, the buffer flags, the control ids, the
//!   private requests and their structures.
//! - `mvx_v4l2_vidioc.c`: the driver name `QUERYCAP` answers (`mvx_v4l2_vidioc_querycap`), and
//!   what `VIDIOC_QBUF` reads besides the standard fields (`mvx_v4l2_vidioc_qbuf`).
//! - `mvx_ext_v4l2.c`: the node is one memory-to-memory video device, multi-planar and
//!   single-planar.
//! - `mvx_buffer.h`: `MVX_BUFFER_NPLANES`.
//!
//! # What this crate sends
//!
//! Only standard V4L2 requests, through `lemnos-driver-m2m-decode`: formats, buffer requests,
//! `VIDIOC_QBUF` and `VIDIOC_DQBUF` of dma-bufs on the multi-planar queues, `VIDIOC_S_CTRL`,
//! stream on and off, the stop commands and events. A queued buffer's `flags` and `reserved2`
//! are zero and its planes' `reserved` words are zero, so none of the private meanings below is
//! ever asked for. That matters: on the source queue the kernel reads
//! `vb2_queue.bufs[index]` before the core checks `index` (a `NULL` or out-of-array read for an
//! index the queue does not have), and with [`flags::BUFFER_GENERAL`] set it does so on either
//! queue and copies a header from the plane's `reserved` words. The session queues only indices
//! below the count the kernel granted.
//!
//! The private requests ([`VIDIOC_S_MVX_ROI_REGIONS`] and the rest) are transcribed and pinned,
//! and never issued: issuing one needs a request this crate cannot make without `unsafe`, and
//! none is needed to carry a stream. See the crate's docs for what is left out.
//!
//! Sizes and offsets were checked against gcc on x86_64 and clang for riscv64; the tests pin
//! them.

#[cfg(target_os = "linux")]
use lemnos_linux_sys::iowr;

/// The driver's name, as `VIDIOC_QUERYCAP` reports it.
pub const DRIVER: &str = "mvx";

/// The card name `VIDIOC_QUERYCAP` reports.
pub const CARD: &str = "Linlon Video device";

/// `MVX_BUFFER_NPLANES`: the most memory planes a buffer of this driver has.
pub const MAX_PLANES: usize = 3;

/// A V4L2 four-character code.
const fn fourcc(c: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*c)
}

/// The driver's own pixel formats (`V4L2_PIX_FMT_*` in the header).
pub mod format {
    use super::fourcc;

    /// `V4L2_PIX_FMT_YUV420_AFBC_8`.
    pub const YUV420_AFBC_8: u32 = fourcc(b"Y0A8");
    /// `V4L2_PIX_FMT_YUV420_AFBC_10`.
    pub const YUV420_AFBC_10: u32 = fourcc(b"Y0AA");
    /// `V4L2_PIX_FMT_YUV422_AFBC_8`.
    pub const YUV422_AFBC_8: u32 = fourcc(b"Y2A8");
    /// `V4L2_PIX_FMT_YUV422_AFBC_10`.
    pub const YUV422_AFBC_10: u32 = fourcc(b"Y2AA");
    /// `V4L2_PIX_FMT_AQB1`.
    pub const AQB1: u32 = fourcc(b"Y0AB");
    /// `V4L2_PIX_FMT_Y210`.
    pub const Y210: u32 = fourcc(b"Y210");
    /// `V4L2_PIX_FMT_Y0L2`.
    pub const Y0L2: u32 = fourcc(b"Y0YL");
    /// `V4L2_PIX_FMT_RGB_3P`.
    pub const RGB_3P: u32 = fourcc(b"RGBM");
    /// `V4L2_PIX_FMT_Y10_LE`.
    pub const Y10_LE: u32 = fourcc(b"Y10L");
    /// `V4L2_PIX_FMT_YUV444_10`.
    pub const YUV444_10: u32 = fourcc(b"Y4P3");
    /// `V4L2_PIX_FMT_YUV422_1P_10`.
    pub const YUV422_1P_10: u32 = fourcc(b"Y2P1");
    /// `V4L2_PIX_FMT_YUV420_2P_10`.
    pub const YUV420_2P_10: u32 = fourcc(b"Y0P2");
    /// `V4L2_PIX_FMT_YUV420_I420_10`.
    pub const YUV420_I420_10: u32 = fourcc(b"Y0P3");
    /// `V4L2_PIX_FMT_RV`.
    pub const RV: u32 = fourcc(b"RV00");
    /// `V4L2_PIX_FMT_AVS`.
    pub const AVS: u32 = fourcc(b"AVS1");
    /// `V4L2_PIX_FMT_AVS2`.
    pub const AVS2: u32 = fourcc(b"AVS2");
    /// `V4L2_PIX_FMT_HEVC`, defined by the header when the kernel's is missing.
    pub const HEVC: u32 = fourcc(b"HEVC");
}

/// The driver's buffer flags. On a dequeued buffer they are the kernel's report, handed to the
/// caller as they are; on a queued buffer this crate never sets any of them.
pub mod flags {
    /// `V4L2_BUF_FLAG_MVX_MASK`: the bits the driver gives its own meanings.
    pub const MASK: u32 = 0xfff0_0000;
    /// `V4L2_BUF_FRAME_FLAG_ROTATION_MASK` (a decoded frame).
    pub const FRAME_ROTATION_MASK: u32 = 0x0300_0000;
    /// `V4L2_BUF_FRAME_FLAG_SCALING_MASK` (a decoded frame).
    pub const FRAME_SCALING_MASK: u32 = 0x0c00_0000;
    /// `V4L2_BUF_FLAG_MVX_BUFFER_FRAME_PRESENT`.
    pub const BUFFER_FRAME_PRESENT: u32 = 0x1000_0000;
    /// `V4L2_BUF_FLAG_MVX_BUFFER_NEED_REALLOC`: the frame did not fit the buffer.
    pub const BUFFER_NEED_REALLOC: u32 = 0x2000_0000;
    /// `V4L2_BUF_FLAG_MVX_CODEC_CONFIG` (a bitstream buffer).
    pub const CODEC_CONFIG: u32 = 0xc100_0000;
    /// `V4L2_BUF_FLAG_END_OF_SUB_FRAME` (a bitstream buffer).
    pub const END_OF_SUB_FRAME: u32 = 0xc200_0000;
    /// `V4L2_BUF_FLAG_MVX_DECODE_ONLY` (a bitstream buffer).
    pub const DECODE_ONLY: u32 = 0xc400_0000;
    /// `V4L2_BUF_ENCODE_FLAG_ROTATION_MASK` (a frame to encode).
    pub const ENCODE_ROTATION_MASK: u32 = 0x3000_0000;
    /// `V4L2_BUF_FLAG_MVX_BUFFER_ROI`.
    pub const BUFFER_ROI: u32 = 0x0400_0000;
    /// `V4L2_BUF_FLAG_MVX_BUFFER_GENERAL` (also `..._EPR`): on a queued buffer, the kernel reads a
    /// header from the first plane's `reserved` words. Never set by this crate.
    pub const BUFFER_GENERAL: u32 = 0x0800_0000;
    /// `V4L2_BUF_FLAG_MVX_BUFFER_CHR`.
    pub const BUFFER_CHR: u32 = 0x4000_0000;
    /// `V4L2_BUF_FLAG_MVX_BUFFER_GOP_RESET`.
    pub const BUFFER_GOP_RESET: u32 = 0x8000_0000;
    /// `V4L2_BUF_FLAG_MVX_BUFFER_LTR_RESET`.
    pub const BUFFER_LTR_RESET: u32 = 0x0020_0000;
    /// `V4L2_BUF_FLAG_MVX_BUFFER_ENC_STATS`.
    pub const BUFFER_ENC_STATS: u32 = 0x0040_0000;

    /// `V4L2_BUF_FLAG_MVX_MINIFRAME`, in `reserved2`: the kernel takes a source offset from the
    /// first plane's `reserved[10]`. Never set by this crate.
    pub const RESERVED2_MINIFRAME: u32 = 0x0000_0001;
    /// `V4L2_BUF_FLAG_MVX_OSD_MASK`, in `reserved2`.
    pub const RESERVED2_OSD_MASK: u32 = 0x0000_0006;
    /// `V4L2_BUF_FLAG_MVX_AD_STATS`, in `reserved2`.
    pub const RESERVED2_AD_STATS: u32 = 0x0000_0008;
}

/// The driver's own controls (`enum v4l2_cid_mve_video`): consecutive from
/// [`control::FRAME_RATE`]. Controls are the caller's settings; the claim refuses only the ones
/// named in [`crate::REFUSED_CONTROLS`].
pub mod control {
    /// `V4L2_CTRL_CLASS_CODEC`.
    pub const CLASS_CODEC: u32 = 0x0099_0000;
    /// `V4L2_CID_MVE_VIDEO_FRAME_RATE`, the first.
    pub const FRAME_RATE: u32 = CLASS_CODEC + 0x2000;
    /// `V4L2_CID_MVE_VIDEO_INTBUF_SIZE`.
    pub const INTBUF_SIZE: u32 = FRAME_RATE + 8;
    /// `V4L2_CID_MVE_VIDEO_SECURE_VIDEO`: loads the session's firmware through the secure path.
    pub const SECURE_VIDEO: u32 = FRAME_RATE + 26;
    /// `V4L2_CID_MVE_VIDEO_FW_PROFILING`.
    pub const FW_PROFILING: u32 = FRAME_RATE + 44;
    /// `V4L2_CID_MVE_VIDEO_INTER_MED_BUF_SIZE`.
    pub const INTER_MED_BUF_SIZE: u32 = FRAME_RATE + 51;
    /// `V4L2_CID_MVE_VIDEO_CHANGE_POS`, the last.
    pub const CHANGE_POS: u32 = FRAME_RATE + 74;
}

/// `V4L2_EVENT_MVX_COLOR_DESC` (`V4L2_EVENT_PRIVATE_START`).
pub const EVENT_COLOR_DESC: u32 = 0x0800_0000;

/// `BASE_VIDIOC_PRIVATE`.
pub const BASE_VIDIOC_PRIVATE: u8 = 192;

/// `V4L2_MVX_MAX_FRAME_REGIONS`.
pub const MAX_FRAME_REGIONS: usize = 16;
/// `V4L2_MAX_FRAME_OSD_REGION`.
pub const MAX_FRAME_OSD_REGION: usize = 2;
/// `V4L2_MAX_FRAME_CHANGE_RECTANGLES`.
pub const MAX_FRAME_CHANGE_RECTANGLES: usize = 2;

/// `struct v4l2_mvx_primary`.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Primary {
    /// `x`.
    pub x: u16,
    /// `y`.
    pub y: u16,
}

/// The `display` member of `struct v4l2_mvx_color_desc`.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Display {
    /// `r`.
    pub r: Primary,
    /// `g`.
    pub g: Primary,
    /// `b`.
    pub b: Primary,
    /// `w`.
    pub w: Primary,
    /// `luminance_min`.
    pub luminance_min: u16,
    /// `luminance_max`.
    pub luminance_max: u16,
}

/// The `content` member of `struct v4l2_mvx_color_desc`.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Content {
    /// `luminance_max`.
    pub luminance_max: u16,
    /// `luminance_average`.
    pub luminance_average: u16,
}

/// `struct v4l2_mvx_color_desc` (packed).
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorDesc {
    /// `flags`.
    pub flags: u32,
    /// `range`.
    pub range: u8,
    /// `primaries`.
    pub primaries: u8,
    /// `transfer`.
    pub transfer: u8,
    /// `matrix`.
    pub matrix: u8,
    /// `display`.
    pub display: Display,
    /// `content`.
    pub content: Content,
    /// `video_format`.
    pub video_format: u8,
    /// `aspect_ratio_idc`.
    pub aspect_ratio_idc: u8,
    /// `sar_width`.
    pub sar_width: u16,
    /// `sar_height`.
    pub sar_height: u16,
    /// `num_units_in_tick`.
    pub num_units_in_tick: u32,
    /// `time_scale`.
    pub time_scale: u32,
}

/// `struct v4l2_buffer_param_region`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Region {
    /// `mbx_left`.
    pub mbx_left: u16,
    /// `mbx_right`.
    pub mbx_right: u16,
    /// `mby_top`.
    pub mby_top: u16,
    /// `mby_bottom`.
    pub mby_bottom: u16,
    /// `qp_delta`.
    pub qp_delta: i16,
    /// `prio`.
    pub prio: u16,
    /// `force_intra`.
    pub force_intra: u16,
}

/// `struct v4l2_mvx_roi_regions`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoiRegions {
    /// `pic_index`.
    pub pic_index: u32,
    /// `qp_present`.
    pub qp_present: u8,
    /// `qp`.
    pub qp: u8,
    /// `roi_present`.
    pub roi_present: u8,
    /// `num_roi`.
    pub num_roi: u8,
    /// `roi`.
    pub roi: [Region; MAX_FRAME_REGIONS],
}

/// `struct v4l2_buffer_param_qp`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParamQp {
    /// `qp`.
    pub qp: i32,
    /// `epr_iframe_enable`.
    pub epr_iframe_enable: i32,
}

/// `struct v4l2_sei_user_data`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeiUserData {
    /// `flags`.
    pub flags: u8,
    /// `uuid`.
    pub uuid: [u8; 16],
    /// `user_data`.
    pub user_data: [u8; 256 - 35],
    /// `user_data_len`.
    pub user_data_len: u8,
}

/// `struct v4l2_rate_control`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RateControl {
    /// `rc_type`.
    pub rc_type: u32,
    /// `target_bitrate`.
    pub target_bitrate: u32,
    /// `maximum_bitrate`.
    pub maximum_bitrate: u32,
}

/// `struct v4l2_mvx_dsl_frame`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DslFrame {
    /// `width`.
    pub width: u32,
    /// `height`.
    pub height: u32,
}

/// `struct v4l2_mvx_dsl_ratio`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DslRatio {
    /// `hor`.
    pub hor: u32,
    /// `ver`.
    pub ver: u32,
}

/// `struct v4l2_mvx_long_term_ref`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LongTermRef {
    /// `mode`.
    pub mode: u32,
    /// `period`.
    pub period: u32,
}

/// `struct v4l2_buffer_param_enc_stats`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EncStats {
    /// `mms_buffer_size`.
    pub mms_buffer_size: u32,
    /// `bitcost_buffer_size`.
    pub bitcost_buffer_size: u32,
    /// `qp_buffer_size`.
    pub qp_buffer_size: u32,
    /// `flags`.
    pub flags: u32,
    /// `pic_index_or_mb_size`.
    pub pic_index_or_mb_size: u32,
}

/// `struct v4l2_buffer_param_rectangle`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rectangle {
    /// `x_left`.
    pub x_left: u16,
    /// `x_right`.
    pub x_right: u16,
    /// `y_top`.
    pub y_top: u16,
    /// `y_bottom`.
    pub y_bottom: u16,
}

/// `struct v4l2_mvx_chr_config`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChrConfig {
    /// `pic_index`.
    pub pic_index: u32,
    /// `num_chr`.
    pub num_chr: u32,
    /// `rectangle`.
    pub rectangle: [Rectangle; MAX_FRAME_CHANGE_RECTANGLES],
}

/// `struct v4l2_mvx_huff_table`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HuffTable {
    /// `type`.
    pub table_type: u32,
    /// `dc_luma_code_lenght`.
    pub dc_luma_code_length: [u8; 16],
    /// `ac_luma_code_lenght`.
    pub ac_luma_code_length: [u8; 16],
    /// `dc_chroma_code_lenght`.
    pub dc_chroma_code_length: [u8; 16],
    /// `ac_chroma_code_lenght`.
    pub ac_chroma_code_length: [u8; 16],
    /// `dc_luma_table`.
    pub dc_luma_table: [u8; 162],
    /// `ac_luma_table`.
    pub ac_luma_table: [u8; 162],
    /// `dc_chroma_table`.
    pub dc_chroma_table: [u8; 162],
    /// `ac_chroma_table`.
    pub ac_chroma_table: [u8; 162],
}

/// `struct v4l2_mvx_seamless_target`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeamlessTarget {
    /// `seamless_mode`.
    pub seamless_mode: u32,
    /// `target_width`.
    pub target_width: u32,
    /// `target_height`.
    pub target_height: u32,
    /// `target_stride`.
    pub target_stride: [u32; 3],
    /// `target_size`.
    pub target_size: [u32; 3],
}

/// `struct v4l2_mvx_color_conv_coef`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorConvCoef {
    /// `coef`.
    pub coef: [[i16; 3]; 3],
    /// `offset`.
    pub offset: [u16; 3],
}

/// `struct v4l2_mvx_rgb2yuv_color_conv_coef`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb2YuvColorConvCoef {
    /// `coef`.
    pub coef: [i16; 9],
    /// `luma_range`.
    pub luma_range: [u8; 2],
    /// `chroma_range`.
    pub chroma_range: [u8; 2],
    /// `rgb_range`.
    pub rgb_range: [u8; 2],
}

/// `struct v4l2_mvx_crop_cfg`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CropCfg {
    /// `crop_en`.
    pub crop_en: u8,
    /// `x`.
    pub x: u16,
    /// `y`.
    pub y: u16,
    /// `width`.
    pub width: u16,
    /// `height`.
    pub height: u16,
}

/// `struct v4l2_mvx_osd_cfg`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OsdCfg {
    /// `osd_inside_enable`.
    pub enable: u8,
    /// `osd_inside_alpha_enable`.
    pub alpha_enable: u8,
    /// `osd_inside_convert_color_enable`.
    pub convert_color_enable: u8,
    /// `osd_inside_alpha_value`.
    pub alpha_value: u8,
    /// `osd_inside_convert_color_threshold`.
    pub convert_color_threshold: u8,
    /// `osd_inside_rgb2yuv_mode`.
    pub rgb2yuv_mode: u8,
    /// `osd_inside_start_x`.
    pub start_x: u16,
    /// `osd_inside_start_y`.
    pub start_y: u16,
    /// `reserved`.
    pub reserved: [u16; 3],
}

/// `struct v4l2_osd_config`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OsdConfig {
    /// `pic_index`.
    pub pic_index: u32,
    /// `num_osd`.
    pub num_osd: u32,
    /// `osd_single_cfg`.
    pub osd_single_cfg: [OsdCfg; MAX_FRAME_OSD_REGION],
}

/// `struct v4l2_osd_info`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OsdInfo {
    /// `width_osd`.
    pub width_osd: [u32; MAX_FRAME_OSD_REGION],
    /// `height_osd`.
    pub height_osd: [u32; MAX_FRAME_OSD_REGION],
    /// `inputFormat_osd`.
    pub input_format_osd: [u32; MAX_FRAME_OSD_REGION],
}

/// The private request `BASE_VIDIOC_PRIVATE + n` on `T`, read and written.
#[cfg(target_os = "linux")]
const fn private<T>(n: u8) -> u32 {
    iowr::<T>(b'V', BASE_VIDIOC_PRIVATE + n)
}

/// `VIDIOC_G_MVX_COLORDESC`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_G_MVX_COLORDESC: u32 = private::<ColorDesc>(0);
/// `VIDIOC_S_MVX_ROI_REGIONS`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_ROI_REGIONS: u32 = private::<RoiRegions>(1);
/// `VIDIOC_S_MVX_QP_EPR`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_QP_EPR: u32 = private::<ParamQp>(2);
/// `VIDIOC_S_MVX_COLORDESC`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_COLORDESC: u32 = private::<ColorDesc>(3);
/// `VIDIOC_S_MVX_SEI_USERDATA`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_SEI_USERDATA: u32 = private::<SeiUserData>(4);
/// `VIDIOC_S_MVX_RATE_CONTROL`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_RATE_CONTROL: u32 = private::<RateControl>(5);
/// `VIDIOC_S_MVX_DSL_FRAME`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_DSL_FRAME: u32 = private::<DslFrame>(6);
/// `VIDIOC_S_MVX_DSL_RATIO`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_DSL_RATIO: u32 = private::<DslRatio>(7);
/// `VIDIOC_S_MVX_LONG_TERM_REF`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_LONG_TERM_REF: u32 = private::<LongTermRef>(8);
/// `VIDIOC_S_MVX_DSL_MODE` (an `int`). Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_DSL_MODE: u32 = private::<i32>(9);
/// `VIDIOC_S_MVX_MINI_FRAME_HEIGHT` (an `int`). Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_MINI_FRAME_HEIGHT: u32 = private::<i32>(10);
/// `VIDIOC_S_MVX_STATS_MODE`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_STATS_MODE: u32 = private::<EncStats>(11);
/// `VIDIOC_S_MVX_CHR_CFG`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_CHR_CFG: u32 = private::<ChrConfig>(12);
/// `VIDIOC_S_MVX_HUFF_TABLE`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_HUFF_TABLE: u32 = private::<HuffTable>(13);
/// `VIDIOC_S_MVX_SEAMLESS_TARGET`: sets how large the kernel makes the decoder's frame buffers.
/// Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_SEAMLESS_TARGET: u32 = private::<SeamlessTarget>(14);
/// `VIDIOC_S_MVX_COLOR_CONV_COEF`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_COLOR_CONV_COEF: u32 = private::<ColorConvCoef>(15);
/// `VIDIOC_S_MVX_ENC_SRC_CROP`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_ENC_SRC_CROP: u32 = private::<CropCfg>(16);
/// `VIDIOC_S_MVX_DEC_DST_CROP`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_DEC_DST_CROP: u32 = private::<CropCfg>(17);
/// `VIDIOC_S_MVX_RGB2YUV_COLOR_CONV_COEF`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_RGB2YUV_COLOR_CONV_COEF: u32 = private::<Rgb2YuvColorConvCoef>(18);
/// `VIDIOC_S_MVX_OSD_CONFIG`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_OSD_CONFIG: u32 = private::<OsdConfig>(19);
/// `VIDIOC_S_MVX_OSD_INFO`. Never issued.
#[cfg(target_os = "linux")]
pub const VIDIOC_S_MVX_OSD_INFO: u32 = private::<OsdInfo>(20);

#[cfg(test)]
mod tests;
