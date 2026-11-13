//! Layout tests: the pins against the header values, and the read-back decoding. The builder's
//! tests are in `tests/builder.rs`, where a buffer comes from a claim.

mod pins;

use crate::layout::{Field, Region};
use crate::vepu541_h264::{READ_BACK, ret};

#[test]
fn field_masks_cover_their_width() {
    let f = |width| Field::new(Region::Set, 0, 0, width, false, true, &[]);
    assert_eq!(f(1).mask(), 0x1);
    assert_eq!(f(8).mask(), 0xff);
    assert_eq!(f(27).mask(), 0x07ff_ffff);
    assert_eq!(f(32).mask(), u32::MAX);
}

#[test]
fn the_read_back_window_covers_every_read_back_register() {
    // Every read-back register sits inside the window the job asks for.
    let first = READ_BACK.first;
    let end = first + READ_BACK.count;
    assert_eq!(first, ret::hw_status::REGISTER.word);
    assert_eq!(end, 209);
    assert!(ret::st_bsl::REGISTER.word >= first && ret::st_bsl::REGISTER.word < end);
}

#[test]
fn the_stream_length_reads_from_the_window() {
    // ST_BSL is word 132 of the file (byte 0x210); the window starts at word 7.
    let mut window = vec![0u32; usize::from(READ_BACK.count)];
    window[usize::from(132 - READ_BACK.first)] = 0x0123_4567;
    let got = ret::st_bsl::BS_LGTH.read(Region::Ret, &window, READ_BACK.first);
    assert_eq!(got, Some(0x0123_4567 & 0x07ff_ffff));
}

#[test]
fn a_field_outside_the_window_reads_nothing() {
    let window = [0u32; 4];
    // A write-side field is not a read-back field, and a word before the window is not in it.
    let set_field = crate::vepu541_h264::set::reg001::LKT_NUM;
    assert_eq!(set_field.read(Region::Ret, &window, 0), None);
    assert_eq!(ret::st_bsl::BS_LGTH.read(Region::Ret, &window, 200), None);
}
