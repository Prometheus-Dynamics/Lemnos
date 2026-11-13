//! The records and values against the vendor header. The bit positions were read off the header's
//! bit fields by compiling a probe with gcc and clang (x86_64, both agree); the enum numbers by
//! printing the header's enumerators. Nothing here is taken from a running engine.

use super::ctu_info::CtuInfo;
use super::qp_map::{self, CTU_BYTES, MbQp, pack_ctu, pack_map, unpack_ctu};
use super::{Error, values};

#[test]
fn the_qp_byte_has_the_headers_bit_fields() {
    let qp = |qp, skip, enable| MbQp { qp, skip, enable }.to_byte();
    assert_eq!(qp(63, false, false), Ok(0x3f));
    assert_eq!(qp(0, true, false), Ok(0x40));
    assert_eq!(qp(0, false, true), Ok(0x80));
    assert_eq!(qp(0, true, true), Ok(0xc0));
    assert_eq!(qp(1, false, false), Ok(0x01));
}

#[test]
fn every_byte_is_an_entry_and_back() {
    for byte in 0..=u8::MAX {
        assert_eq!(MbQp::from_byte(byte).to_byte(), Ok(byte));
    }
}

#[test]
fn a_qp_above_six_bits_is_refused() {
    let mb = |qp| MbQp {
        qp,
        ..MbQp::default()
    };
    assert_eq!(mb(64).to_byte(), Err(Error::QpOutOfRange(64)));
    assert_eq!(mb(u8::MAX).to_byte(), Err(Error::QpOutOfRange(u8::MAX)));
    assert_eq!(mb(qp_map::MAX_QP).to_byte(), Ok(qp_map::MAX_QP));
}

#[test]
fn a_ctu_word_puts_the_first_macroblock_lowest() {
    let mbs = [
        MbQp {
            qp: 1,
            ..MbQp::default()
        },
        MbQp {
            qp: 2,
            ..MbQp::default()
        },
        MbQp {
            qp: 3,
            ..MbQp::default()
        },
        MbQp {
            enable: true,
            ..MbQp::default()
        },
    ];
    assert_eq!(pack_ctu(&mbs), Ok(0x8003_0201));
    assert_eq!(unpack_ctu(0x8003_0201), mbs);
    assert_eq!(CTU_BYTES, 4);
}

#[test]
fn the_map_is_the_bytes_of_its_ctus_in_order() {
    let a = [
        MbQp {
            qp: 10,
            skip: true,
            enable: false,
        },
        MbQp {
            qp: 11,
            skip: false,
            enable: false,
        },
        MbQp {
            qp: 12,
            skip: false,
            enable: true,
        },
        MbQp {
            qp: 13,
            skip: true,
            enable: true,
        },
    ];
    let b = [MbQp {
        qp: 63,
        ..MbQp::default()
    }; CTU_BYTES];
    let mut out = [0u8; 8];
    assert_eq!(pack_map(&[a, b], &mut out), Ok(8));
    assert_eq!(out, [0x4a, 0x0b, 0x8c, 0xcd, 0x3f, 0x3f, 0x3f, 0x3f]);
}

#[test]
fn a_short_buffer_is_refused_and_untouched() {
    let ctus = [[MbQp::default(); CTU_BYTES]; 2];
    let mut out = [0xaau8; 7];
    assert_eq!(
        pack_map(&ctus, &mut out),
        Err(Error::TooShort { need: 8, have: 7 })
    );
    assert_eq!(out, [0xaa; 7]);
}

#[test]
fn a_bad_value_anywhere_writes_nothing() {
    let good = [MbQp::default(); CTU_BYTES];
    let mut bad = good;
    bad[2].qp = 64;
    let mut out = [0xaau8; 8];
    assert_eq!(
        pack_map(&[good, bad], &mut out),
        Err(Error::QpOutOfRange(64))
    );
    assert_eq!(out, [0xaa; 8]);
}

#[cfg(target_os = "linux")]
#[test]
fn packing_the_map_allocates_nothing() {
    use lemnos_measure::test_alloc::allocations;
    let ctus = [[MbQp {
        qp: 5,
        skip: true,
        enable: true,
    }; CTU_BYTES]; 16];
    let mut out = [0u8; 64];
    let (result, count) = allocations(|| pack_map(&ctus, &mut out));
    assert_eq!(result, Ok(64));
    assert_eq!(count, 0);
}

#[test]
fn the_ctu_info_word_has_the_headers_bit_fields() {
    let info = |f: fn(&mut CtuInfo)| {
        let mut i = CtuInfo::default();
        f(&mut i);
        i.to_word()
    };
    assert_eq!(info(|i| i.is_move = true), Ok(0x1));
    assert_eq!(info(|i| i.is_expand = true), Ok(0x2));
    assert_eq!(info(|i| i.en_mad = true), Ok(0x4));
    assert_eq!(info(|i| i.mad = 0xff), Ok(0xff00));
    assert_eq!(info(|i| i.mse = 0xffff), Ok(0xffff_0000));
}

#[test]
fn the_ctu_info_reserved_bits_are_refused_and_read_back() {
    let info = CtuInfo {
        reserved: 1,
        ..CtuInfo::default()
    };
    assert_eq!(info.to_word(), Err(Error::Reserved(1)));
    assert_eq!(CtuInfo::from_word(0xf8).reserved, 0x1f);
    assert_eq!(CtuInfo::from_word(0xf8).mad, 0);
}

#[test]
fn the_ctu_info_word_round_trips() {
    let info = CtuInfo {
        is_move: true,
        is_expand: false,
        en_mad: true,
        reserved: 0,
        mad: 0x5a,
        mse: 0x1234,
    };
    let word = info.to_word().unwrap();
    assert_eq!(word, 0x1234_5a05);
    assert_eq!(CtuInfo::from_word(word), info);
}

#[test]
fn the_codec_numbers_are_the_headers() {
    use values::codec::*;
    assert_eq!([H264, JPEG, H264_VER2, H265, VP8], [0, 1, 2, 3, 4]);
}

#[test]
fn the_rate_control_numbers_are_the_headers() {
    use values::rc_mode::*;
    assert_eq!([CBR, VBR, AVBR, QPMAP, FIXQP], [0, 1, 2, 3, 4]);
}

#[test]
fn the_pixel_numbers_are_the_headers() {
    use values::pixel::*;
    assert_eq!(
        [YUV420SP, YVU420SP, YUV420P, YVU420P, YUV422_BEGIN],
        [0, 1, 2, 3, 4]
    );
    assert_eq!([YUV422_END, ARGB, TILE_32X32], [13, 14, 18]);
    assert_eq!(LBC_AW, 21);
}

#[test]
fn the_h264_profile_and_level_numbers_are_the_headers() {
    use values::{h264_level, h264_profile};
    assert_eq!(
        [
            h264_profile::BASELINE,
            h264_profile::MAIN,
            h264_profile::HIGH
        ],
        [66, 77, 100]
    );
    assert_eq!(
        [h264_level::DEFAULT, h264_level::L1, h264_level::L3_1],
        [0, 10, 31]
    );
    assert_eq!([h264_level::L4_2, h264_level::L5_2], [42, 52]);
}

#[test]
fn the_buffer_flags_and_slice_types_are_the_headers() {
    use values::{buffer_flag, slice};
    assert_eq!(
        [buffer_flag::KEYFRAME, buffer_flag::EOS, buffer_flag::THUMB],
        [1, 2, 4]
    );
    assert_eq!(
        [slice::INVALID, slice::B, slice::P, slice::I, slice::IDR],
        [0, 1, 2, 4, 8]
    );
}
