//! Layout pins: every word and bit field against the values gcc and clang give for the vendor
//! header. The builder's tests are in `tests/builder.rs`, where a buffer comes from a claim.

mod pins;

use crate::layout::Field;
use crate::vpu720::{reg001_enc_strt, reg032_sw_src_fmt};

#[test]
fn field_masks_cover_their_width() {
    let f = |width| Field::new(0, 0, width, true, &[]);
    assert_eq!(f(1).mask(), 0x1);
    assert_eq!(f(8).mask(), 0xff);
    assert_eq!(f(13).mask(), 0x1fff);
    assert_eq!(f(32).mask(), u32::MAX);
}

#[test]
fn the_header_lists_its_encodings() {
    // vepu_cmd: 0 to 7 are listed. src_fmt: 0xb is the gap in the list.
    assert_eq!(reg001_enc_strt::VEPU_CMD.values, &[0, 1, 2, 3, 4, 5, 6, 7]);
    assert!(!reg032_sw_src_fmt::SRC_FMT.values.contains(&0xb));
    assert!(reg032_sw_src_fmt::SRC_FMT.values.contains(&0xd));
}
