//! The layouts, request numbers and values, as gcc on x86_64 and clang for riscv64 and riscv32
//! compute them from the header's declarations.

use std::mem::{align_of, offset_of, size_of};

use super::*;

#[test]
fn the_layouts_match_the_c_compiler() {
    assert_eq!(size_of::<Primary>(), 4);
    assert_eq!(size_of::<ColorDesc>(), 46);
    assert_eq!(align_of::<ColorDesc>(), 1);
    assert_eq!(offset_of!(ColorDesc, range), 4);
    assert_eq!(offset_of!(ColorDesc, display), 8);
    assert_eq!(offset_of!(ColorDesc, content), 28);
    assert_eq!(offset_of!(ColorDesc, video_format), 32);
    assert_eq!(offset_of!(ColorDesc, sar_width), 34);
    assert_eq!(offset_of!(ColorDesc, num_units_in_tick), 38);
    assert_eq!(offset_of!(ColorDesc, time_scale), 42);

    assert_eq!(size_of::<Region>(), 14);
    assert_eq!(size_of::<RoiRegions>(), 232);
    assert_eq!(offset_of!(RoiRegions, num_roi), 7);
    assert_eq!(offset_of!(RoiRegions, roi), 8);
    assert_eq!(size_of::<ParamQp>(), 8);
    assert_eq!(size_of::<SeiUserData>(), 239);
    assert_eq!(offset_of!(SeiUserData, user_data_len), 238);
    assert_eq!(size_of::<RateControl>(), 12);
    assert_eq!(size_of::<DslFrame>(), 8);
    assert_eq!(size_of::<DslRatio>(), 8);
    assert_eq!(size_of::<LongTermRef>(), 8);
    assert_eq!(size_of::<EncStats>(), 20);
    assert_eq!(size_of::<Rectangle>(), 8);
    assert_eq!(size_of::<ChrConfig>(), 24);
    assert_eq!(offset_of!(ChrConfig, rectangle), 8);
    assert_eq!(size_of::<HuffTable>(), 716);
    assert_eq!(offset_of!(HuffTable, dc_luma_table), 68);
    assert_eq!(offset_of!(HuffTable, ac_chroma_table), 554);
    assert_eq!(size_of::<SeamlessTarget>(), 36);
    assert_eq!(offset_of!(SeamlessTarget, target_size), 24);
    assert_eq!(size_of::<ColorConvCoef>(), 24);
    assert_eq!(offset_of!(ColorConvCoef, offset), 18);
    assert_eq!(size_of::<Rgb2YuvColorConvCoef>(), 24);
    assert_eq!(offset_of!(Rgb2YuvColorConvCoef, rgb_range), 22);
    assert_eq!(size_of::<CropCfg>(), 10);
    assert_eq!(offset_of!(CropCfg, x), 2);
    assert_eq!(offset_of!(CropCfg, height), 8);
    assert_eq!(size_of::<OsdCfg>(), 16);
    assert_eq!(offset_of!(OsdCfg, start_x), 6);
    assert_eq!(offset_of!(OsdCfg, reserved), 10);
    assert_eq!(size_of::<OsdConfig>(), 40);
    assert_eq!(offset_of!(OsdConfig, osd_single_cfg), 8);
    assert_eq!(size_of::<OsdInfo>(), 24);
}

#[cfg(target_os = "linux")]
#[test]
fn the_request_numbers_match_the_c_compiler() {
    assert_eq!(VIDIOC_G_MVX_COLORDESC, 0xc02e_56c0);
    assert_eq!(VIDIOC_S_MVX_ROI_REGIONS, 0xc0e8_56c1);
    assert_eq!(VIDIOC_S_MVX_QP_EPR, 0xc008_56c2);
    assert_eq!(VIDIOC_S_MVX_COLORDESC, 0xc02e_56c3);
    assert_eq!(VIDIOC_S_MVX_SEI_USERDATA, 0xc0ef_56c4);
    assert_eq!(VIDIOC_S_MVX_RATE_CONTROL, 0xc00c_56c5);
    assert_eq!(VIDIOC_S_MVX_DSL_FRAME, 0xc008_56c6);
    assert_eq!(VIDIOC_S_MVX_DSL_RATIO, 0xc008_56c7);
    assert_eq!(VIDIOC_S_MVX_LONG_TERM_REF, 0xc008_56c8);
    assert_eq!(VIDIOC_S_MVX_DSL_MODE, 0xc004_56c9);
    assert_eq!(VIDIOC_S_MVX_MINI_FRAME_HEIGHT, 0xc004_56ca);
    assert_eq!(VIDIOC_S_MVX_STATS_MODE, 0xc014_56cb);
    assert_eq!(VIDIOC_S_MVX_CHR_CFG, 0xc018_56cc);
    assert_eq!(VIDIOC_S_MVX_HUFF_TABLE, 0xc2cc_56cd);
    assert_eq!(VIDIOC_S_MVX_SEAMLESS_TARGET, 0xc024_56ce);
    assert_eq!(VIDIOC_S_MVX_COLOR_CONV_COEF, 0xc018_56cf);
    assert_eq!(VIDIOC_S_MVX_ENC_SRC_CROP, 0xc00a_56d0);
    assert_eq!(VIDIOC_S_MVX_DEC_DST_CROP, 0xc00a_56d1);
    assert_eq!(VIDIOC_S_MVX_RGB2YUV_COLOR_CONV_COEF, 0xc018_56d2);
    assert_eq!(VIDIOC_S_MVX_OSD_CONFIG, 0xc028_56d3);
    assert_eq!(VIDIOC_S_MVX_OSD_INFO, 0xc018_56d4);
}

#[test]
fn the_control_ids_match_the_c_compiler() {
    assert_eq!(control::FRAME_RATE, 0x0099_2000);
    assert_eq!(control::INTBUF_SIZE, 0x0099_2008);
    assert_eq!(control::SECURE_VIDEO, 0x0099_201a);
    assert_eq!(control::FW_PROFILING, 0x0099_202c);
    assert_eq!(control::INTER_MED_BUF_SIZE, 0x0099_2033);
    assert_eq!(control::CHANGE_POS, 0x0099_204a);
}

#[test]
fn the_formats_and_flags_are_the_headers() {
    assert_eq!(format::YUV420_AFBC_8, 0x3841_3059);
    assert_eq!(format::Y210, 0x3031_3259);
    assert_eq!(format::RGB_3P, 0x4d42_4752);
    assert_eq!(format::HEVC, 0x4356_4548);
    assert_eq!(format::AVS2, 0x3253_5641);
    // The private bits all sit in the driver's mask.
    for flag in [
        flags::FRAME_ROTATION_MASK,
        flags::FRAME_SCALING_MASK,
        flags::BUFFER_FRAME_PRESENT,
        flags::BUFFER_NEED_REALLOC,
        flags::CODEC_CONFIG,
        flags::END_OF_SUB_FRAME,
        flags::DECODE_ONLY,
        flags::ENCODE_ROTATION_MASK,
        flags::BUFFER_ROI,
        flags::BUFFER_GENERAL,
        flags::BUFFER_CHR,
        flags::BUFFER_GOP_RESET,
        flags::BUFFER_LTR_RESET,
        flags::BUFFER_ENC_STATS,
    ] {
        assert_eq!(flag & !flags::MASK, 0, "{flag:#x}");
    }
    assert_eq!(EVENT_COLOR_DESC, 0x0800_0000);
    assert_eq!(MAX_PLANES, 3);
}
