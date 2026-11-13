use crate::isp35::{__BindgenBitfieldUnit, isp39_rawaf_mean_aehgl};
use crate::*;

#[test]
fn the_buffer_sizes_are_the_kernels() {
    // The numbers `layout.rs` checks against the C compiler, as the driver sees them.
    assert_eq!(ISP21_PARAMS_BYTES, 10475);
    assert_eq!(ISP21_STATS_BYTES, 13700);
    assert_eq!(ISP30_PARAMS_BYTES, 10921);
    assert_eq!(ISP30_STATS_BYTES, 16172);
    assert_eq!(ISP35_PARAMS_BYTES, 13065);
    assert_eq!(ISP35_STATS_BYTES, 14542);
}

#[test]
fn an_isp35_bitfield_lands_where_the_c_compiler_puts_it() {
    // `__u32 average:12; __u32 highlight:20;`: on a little-endian target the first field takes
    // the low bits of the word.
    let word: u32 = (0x12345 << 12) | 0xabc;
    let unit = isp39_rawaf_mean_aehgl::new_bitfield_1(0xabc, 0x12345);
    assert_eq!(unit, __BindgenBitfieldUnit::new(word.to_le_bytes()));
    let read = isp39_rawaf_mean_aehgl {
        _bitfield_1: __BindgenBitfieldUnit::new(word.to_le_bytes()),
    };
    assert_eq!((read.average(), read.highlight()), (0xabc, 0x12345));
}

#[test]
fn the_statistics_wrappers_put_their_header_where_the_kernel_does() {
    use core::mem::offset_of;
    // ISP21 and ISP30 lead with the header; ISP35 ends with it.
    assert_eq!(offset_of!(isp21::rkisp_isp21_stat_buffer, frame_id), 4);
    assert_eq!(offset_of!(isp3x::rkisp3x_isp_stat_buffer, frame_id), 4);
    assert_eq!(
        offset_of!(isp35::rkisp35_stat_buffer, frame_id),
        size_of::<isp35::isp35_stat>() + 4
    );
}

#[test]
fn the_isp35_module_bits_are_distinct_and_all_named() {
    use crate::module35::*;
    let all = [
        DPCC, BLS, SDG, LSC, AWB_GAIN, BDM, CCM, GOC, CPROC, IE, RAWAF, RAWAE0, RAWAE1, RAWAE2,
        RAWAE3, RAWAWB, RAWHIST0, RAWHIST1, RAWHIST2, RAWHIST3, HDRMGE, RAWNR, GIC, DHAZ, LUT3D,
        LDCH, GAIN, DEBAYER, BAYNR, BAY3D, YNR, CNR, SHARP, DRC, CAC, CSM, CGC, RGBIR, HSV, ENH,
        HIST, AI, AIAWB, AWBSYNC, BAY3D_L2, FORCE,
    ];
    // Every bit of the ISP35 table is one bit, and no two names share one.
    let mut seen = 0u64;
    for bit in all {
        assert_eq!(bit.count_ones(), 1);
        assert_eq!(seen & bit, 0);
        seen |= bit;
    }
    assert_eq!(all.len(), 46);
    assert_eq!(seen, ALL);
    assert_eq!(super::module::ALL & RGBIR, 0);
}

#[test]
fn the_byte_views_are_packed_so_every_byte_is_a_field_byte() {
    use crate::bytes::Bytes;
    use core::mem::align_of;
    assert_eq!(align_of::<isp21::isp21_isp_params_cfg>(), 1);
    assert_eq!(align_of::<isp21::rkisp_isp21_stat_buffer>(), 1);
    assert_eq!(align_of::<isp3x::isp3x_isp_params_cfg>(), 1);
    assert_eq!(align_of::<isp3x::rkisp3x_isp_stat_buffer>(), 1);
    assert_eq!(align_of::<isp35::isp35_isp_params_cfg>(), 1);
    assert_eq!(align_of::<isp35::rkisp35_stat_buffer>(), 1);
    assert_eq!(
        <isp35::isp35_isp_params_cfg as Bytes>::SIZE,
        ISP35_PARAMS_BYTES
    );
    assert_eq!(
        <isp35::rkisp35_stat_buffer as Bytes>::SIZE,
        ISP35_STATS_BYTES
    );
    assert_eq!(
        <isp3x::isp3x_isp_params_cfg as Bytes>::SIZE,
        ISP30_PARAMS_BYTES
    );
    assert_eq!(
        <isp21::isp21_isp_params_cfg as Bytes>::SIZE,
        ISP21_PARAMS_BYTES
    );
}

#[test]
fn a_caller_writes_module_bits_at_the_kernels_offsets_through_bytes() {
    use crate::bytes::Bytes;
    use crate::module35;
    use core::mem::offset_of;
    let zero = [0u8; ISP35_PARAMS_BYTES];
    let mut params = <isp35::isp35_isp_params_cfg as Bytes>::from_bytes(&zero).unwrap();
    let mask = module35::LSC | module35::HSV;
    // The bytes at the field's offset are the mask, little-endian, and nothing else moves.
    params.as_bytes_mut()[offset_of!(isp35::isp35_isp_params_cfg, module_ens)..][..8]
        .copy_from_slice(&mask.to_le_bytes());
    let back = <isp35::isp35_isp_params_cfg as Bytes>::from_bytes(params.as_bytes()).unwrap();
    assert_eq!({ back.module_ens }, mask);
    assert_eq!({ back.module_en_update }, 0);
    assert_eq!({ back.module_cfg_update }, 0);
    assert_eq!(params.as_bytes().len(), ISP35_PARAMS_BYTES);
    assert!(<isp35::isp35_isp_params_cfg as Bytes>::from_bytes(&[0; 10]).is_none());
}
