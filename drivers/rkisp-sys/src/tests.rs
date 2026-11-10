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
