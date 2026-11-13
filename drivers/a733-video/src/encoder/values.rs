//! The values a caller names, as `vencoder.h` numbers them. Each module is one C enum (or the
//! macros of one kind), in the header's order; the numbers are the header's.

/// `VENC_CODEC_TYPE`.
pub mod codec {
    /// `VENC_CODEC_H264`.
    pub const H264: u32 = 0;
    /// `VENC_CODEC_JPEG`.
    pub const JPEG: u32 = 1;
    /// `VENC_CODEC_H264_VER2`.
    pub const H264_VER2: u32 = 2;
    /// `VENC_CODEC_H265`.
    pub const H265: u32 = 3;
    /// `VENC_CODEC_VP8`.
    pub const VP8: u32 = 4;
}

/// `VENC_RC_MODE`: the rate control. `QPMAP` is the mode the QP map applies to.
pub mod rc_mode {
    /// `AW_CBR`.
    pub const CBR: u32 = 0;
    /// `AW_VBR`.
    pub const VBR: u32 = 1;
    /// `AW_AVBR`.
    pub const AVBR: u32 = 2;
    /// `AW_QPMAP`.
    pub const QPMAP: u32 = 3;
    /// `AW_FIXQP`.
    pub const FIXQP: u32 = 4;
}

/// `VENC_PIXEL_FMT`.
pub mod pixel {
    /// `VENC_PIXEL_YUV420SP`.
    pub const YUV420SP: u32 = 0;
    /// `VENC_PIXEL_YVU420SP`.
    pub const YVU420SP: u32 = 1;
    /// `VENC_PIXEL_YUV420P`.
    pub const YUV420P: u32 = 2;
    /// `VENC_PIXEL_YVU420P`.
    pub const YVU420P: u32 = 3;
    /// `VENC_PIXEL_YUV422_BEGIN`, the first 4:2:2 format.
    pub const YUV422_BEGIN: u32 = 4;
    /// `VENC_PIXEL_YUV422SP`.
    pub const YUV422SP: u32 = 5;
    /// `VENC_PIXEL_YVU422SP`.
    pub const YVU422SP: u32 = 6;
    /// `VENC_PIXEL_YUV422P`.
    pub const YUV422P: u32 = 7;
    /// `VENC_PIXEL_YVU422P`.
    pub const YVU422P: u32 = 8;
    /// `VENC_PIXEL_YUYV422`.
    pub const YUYV422: u32 = 9;
    /// `VENC_PIXEL_UYVY422`.
    pub const UYVY422: u32 = 10;
    /// `VENC_PIXEL_YVYU422`.
    pub const YVYU422: u32 = 11;
    /// `VENC_PIXEL_VYUY422`.
    pub const VYUY422: u32 = 12;
    /// `VENC_PIXEL_YUV422_END`, one past the 4:2:2 formats.
    pub const YUV422_END: u32 = 13;
    /// `VENC_PIXEL_ARGB`.
    pub const ARGB: u32 = 14;
    /// `VENC_PIXEL_RGBA`.
    pub const RGBA: u32 = 15;
    /// `VENC_PIXEL_ABGR`.
    pub const ABGR: u32 = 16;
    /// `VENC_PIXEL_BGRA`.
    pub const BGRA: u32 = 17;
    /// `VENC_PIXEL_TILE_32X32`.
    pub const TILE_32X32: u32 = 18;
    /// `VENC_PIXEL_TILE_128X32`.
    pub const TILE_128X32: u32 = 19;
    /// `VENC_PIXEL_AFBC_AW`.
    pub const AFBC_AW: u32 = 20;
    /// `VENC_PIXEL_LBC_AW`.
    pub const LBC_AW: u32 = 21;
}

/// `VENC_H264PROFILETYPE`: the profile numbers, which are the H.264 `profile_idc` values.
pub mod h264_profile {
    /// `VENC_H264ProfileBaseline`.
    pub const BASELINE: u32 = 66;
    /// `VENC_H264ProfileMain`.
    pub const MAIN: u32 = 77;
    /// `VENC_H264ProfileHigh`.
    pub const HIGH: u32 = 100;
}

/// `VENC_H264LEVELTYPE`: the level numbers, which are ten times the H.264 level.
pub mod h264_level {
    /// `VENC_H264LevelDefault`: the encoder chooses.
    pub const DEFAULT: u32 = 0;
    /// `VENC_H264Level1`.
    pub const L1: u32 = 10;
    /// `VENC_H264Level11`.
    pub const L1_1: u32 = 11;
    /// `VENC_H264Level12`.
    pub const L1_2: u32 = 12;
    /// `VENC_H264Level13`.
    pub const L1_3: u32 = 13;
    /// `VENC_H264Level2`.
    pub const L2: u32 = 20;
    /// `VENC_H264Level21`.
    pub const L2_1: u32 = 21;
    /// `VENC_H264Level22`.
    pub const L2_2: u32 = 22;
    /// `VENC_H264Level3`.
    pub const L3: u32 = 30;
    /// `VENC_H264Level31`.
    pub const L3_1: u32 = 31;
    /// `VENC_H264Level32`.
    pub const L3_2: u32 = 32;
    /// `VENC_H264Level4`.
    pub const L4: u32 = 40;
    /// `VENC_H264Level41`.
    pub const L4_1: u32 = 41;
    /// `VENC_H264Level42`.
    pub const L4_2: u32 = 42;
    /// `VENC_H264Level5`.
    pub const L5: u32 = 50;
    /// `VENC_H264Level51`.
    pub const L5_1: u32 = 51;
    /// `VENC_H264Level52`.
    pub const L5_2: u32 = 52;
}

/// The `VENC_BUFFERFLAG_*` macros: the flags on a bitstream buffer.
pub mod buffer_flag {
    /// `VENC_BUFFERFLAG_KEYFRAME`.
    pub const KEYFRAME: u32 = 0x1;
    /// `VENC_BUFFERFLAG_EOS`.
    pub const EOS: u32 = 0x2;
    /// `VENC_BUFFERFLAG_THUMB`.
    pub const THUMB: u32 = 0x4;
}

/// `VENC_PICTURE_SLICE_TYPE`: one bit each, and zero for none.
pub mod slice {
    /// `PICTURE_SLICE_INVALID`.
    pub const INVALID: u32 = 0x0;
    /// `PICTURE_B_SLICE`.
    pub const B: u32 = 1 << 0;
    /// `PICTURE_P_SLICE`.
    pub const P: u32 = 1 << 1;
    /// `PICTURE_I_SLICE`.
    pub const I: u32 = 1 << 2;
    /// `PICTURE_IDR_SLICE`.
    pub const IDR: u32 = 1 << 3;
}
