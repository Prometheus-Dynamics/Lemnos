//! Host tests of the command streams. Each stream is compared word for word with what the
//! vendor's `VGLite/vg_lite.c` submits for the same call, for small cases: the arrays below are
//! the words that library built for a recording kernel (its `vg_lite_kernel` ioctl replaced by a
//! recorder, the rest of its user library unchanged, compiled for an x86-64 host with no FMA).
//! The fixtures put each buffer at the address the recorder handed out, in allocation order.
//!
//! The buffers are host stand-ins ([`Scratch`]): the stream checks them as it checks kernel
//! memory, and `Context::run` refuses them, so they never reach a GPU.
//!
//! The arrays and the oracle that recorded them are the dev branch's
//! (`crates/lemnos-drivers-vglite/src/tests.rs` and `stroke_tests.rs`), carried over unchanged.
//!
//! The K230's own build may contract multiply-adds in the step set-up (GCC's default is
//! `-ffp-contract=fast`), which could move a step by one unit in the last place; that is not
//! measured here. Words that come from reading the C rather than from the recording are checked
//! separately in the last tests.

use std::io;

use lemnos_driver_vglite_sys::{Layout, Scratch, Stream, TargetFormat, Tessellation};
use lemnos_measure::test_alloc::allocations;

use crate::command::{self, Bound, Op};
use crate::plan::{
    self, BLEND_DST_OVER, BLEND_NONE, BLEND_SRC_OVER, Filter, Format, IDENTITY, Image, Sampling,
    Transform,
};
use crate::stroke::{Cap, Join, Outliner, Subpath};

fn tess() -> Tessellation {
    Tessellation {
        physical: 0x4000_0000,
        size: 0x1_0000,
        count_size: 0x1000,
        tess_w_h: 0,
    }
}

/// A host buffer for a fixture at `address`; large enough for every fixture here.
fn buffer(address: u32) -> Scratch {
    Scratch::new(address, 0x1000)
}

fn bound(
    buffer: &Scratch,
    format: Format,
    width: u32,
    height: u32,
    stride: u32,
) -> Bound<'_, Scratch> {
    Bound {
        image: Image {
            format,
            width,
            height,
            stride,
        },
        buffer,
    }
}

/// The recorder's first allocation: a 16x4 RGBA target at 0x8000_0000, stride 64.
fn target_buffer() -> Scratch {
    buffer(0x8000_0000)
}

/// The recorder's third allocation: a 16x4 RGBA source at 0x8020_0000, stride 64.
fn source_buffer() -> Scratch {
    buffer(0x8020_0000)
}

/// Encodes `op` into a fresh stream, finishes it, and returns its words. Asserts it draws.
fn words(op: Op<'_>, target: Bound<'_, Scratch>, source: Option<Bound<'_, Scratch>>) -> Vec<u32> {
    let mut stream = Stream::new(1 << 17, tess());
    assert!(
        command::encode(op, &mut stream, target, source).expect("encodes"),
        "draws"
    );
    stream.finish().expect("finishes");
    stream.words().to_vec()
}

#[test]
fn init_is_the_vendor_set_up_then_its_flush() {
    let mut stream = Stream::new(1 << 17, tess());
    command::init(&mut stream).unwrap();
    stream.finish().unwrap();
    assert_eq!(stream.words().to_vec(), INIT);
}

#[test]
fn clear_of_a_rectangle_is_a_rectangle_primitive() {
    let t = target_buffer();
    let target = bound(&t, Format::Rgba8888, 16, 4, 64);
    let got = words(
        Op::Clear {
            rect: (1, 1, 3, 2),
            color: 0xFF11_2233,
        },
        target,
        None,
    );
    assert_eq!(got, CLEAR_RECT);
}

#[test]
fn clear_across_the_full_width_from_the_corner_is_the_pixel_engine() {
    let t = target_buffer();
    let target = bound(&t, Format::Rgba8888, 16, 4, 64);
    let got = words(
        Op::Clear {
            rect: (0, 0, 16, 4),
            color: 0xFF11_2233,
        },
        target,
        None,
    );
    assert_eq!(got, CLEAR_FULL);
}

#[test]
fn a8_clear_writes_the_colour_word_unchanged() {
    let m = buffer(0x8010_0000);
    let mask = bound(&m, Format::A8, 16, 4, 16);
    let got = words(
        Op::Clear {
            rect: (0, 0, 16, 4),
            color: 0x0000_00FF,
        },
        mask,
        None,
    );
    assert_eq!(got, CLEAR_A8);
}

#[test]
fn identity_blit_with_point_sampling() {
    let (t, s) = (target_buffer(), source_buffer());
    let got = words(
        Op::Blit {
            matrix: IDENTITY,
            blend: BLEND_NONE,
            sampling: Sampling::Point,
        },
        bound(&t, Format::Rgba8888, 16, 4, 64),
        Some(bound(&s, Format::Rgba8888, 16, 4, 64)),
    );
    assert_eq!(got, BLIT_IDENTITY);
}

#[test]
fn crop_is_a_translated_point_blit() {
    let (t, s) = (target_buffer(), source_buffer());
    let got = words(
        Op::Blit {
            matrix: plan::crop((2, 1, 4, 2)),
            blend: BLEND_NONE,
            sampling: Sampling::Point,
        },
        bound(&t, Format::Rgba8888, 16, 4, 64),
        Some(bound(&s, Format::Rgba8888, 16, 4, 64)),
    );
    assert_eq!(got, BLIT_CROP);
}

#[test]
fn horizontal_flip_is_a_mirror_matrix() {
    let (t, s) = (target_buffer(), source_buffer());
    let got = words(
        Op::Blit {
            matrix: plan::transform(Transform::FlipHorizontal, 16, 4),
            blend: BLEND_NONE,
            sampling: Sampling::Point,
        },
        bound(&t, Format::Rgba8888, 16, 4, 64),
        Some(bound(&s, Format::Rgba8888, 16, 4, 64)),
    );
    assert_eq!(got, BLIT_FLIP_H);
}

#[test]
fn quarter_turn_sets_the_transparency_bit_of_a_rotated_blit() {
    let (t, s) = (buffer(0x8030_0000), source_buffer());
    let got = words(
        Op::Blit {
            matrix: plan::transform(Transform::Rotate90, 16, 4),
            blend: BLEND_NONE,
            sampling: Sampling::Point,
        },
        bound(&t, Format::Rgba8888, 4, 16, 64),
        Some(bound(&s, Format::Rgba8888, 16, 4, 64)),
    );
    assert_eq!(got, BLIT_ROT90);
    // The configuration word carries 0x8000 (`transparency_mode`) for a rotated NONE blit.
    assert_eq!(got[11], 0x1000_9001);
}

#[test]
fn bilinear_half_scale_resize_uses_the_half_phase_steps() {
    let (t, s) = (buffer(0x8040_0000), source_buffer());
    let src = Image::packed(Format::Rgba8888, 16, 4);
    let dst = Image::packed(Format::Rgba8888, 8, 2);
    let (matrix, sampling) = plan::resize(src, dst, Filter::Bilinear).unwrap();
    assert_eq!(sampling, Sampling::BiLinear);
    let got = words(
        Op::Blit {
            matrix,
            blend: BLEND_NONE,
            sampling,
        },
        bound(&t, Format::Rgba8888, 8, 2, 64),
        Some(bound(&s, Format::Rgba8888, 16, 4, 64)),
    );
    assert_eq!(got, RESIZE_HALF);
}

#[test]
fn src_over_and_dst_over_set_the_blend_and_read_split_bits() {
    let (t, s) = (target_buffer(), source_buffer());
    let over = words(
        Op::Blit {
            matrix: IDENTITY,
            blend: BLEND_SRC_OVER,
            sampling: Sampling::Point,
        },
        bound(&t, Format::Rgba8888, 16, 4, 64),
        Some(bound(&s, Format::Rgba8888, 16, 4, 64)),
    );
    assert_eq!(over, SRC_OVER_STREAM);
    let under = words(
        Op::Blit {
            matrix: IDENTITY,
            blend: BLEND_DST_OVER,
            sampling: Sampling::Point,
        },
        bound(&t, Format::Rgba8888, 16, 4, 64),
        Some(bound(&s, Format::Rgba8888, 16, 4, 64)),
    );
    assert_eq!(under, DST_OVER_STREAM);
}

#[test]
fn palette_loads_the_colour_table_then_blits_the_indices() {
    let (t, ix) = (target_buffer(), buffer(0x8050_0000));
    let colours: [u32; 256] = std::array::from_fn(|i| 0xFF00_0000 | i as u32);
    let got = words(
        Op::Palette { colours: &colours },
        bound(&t, Format::Rgba8888, 16, 4, 64),
        Some(bound(&ix, Format::Index8, 16, 4, 16)),
    );
    assert_eq!(got, PALETTE);
}

#[test]
fn the_colour_table_is_one_run_padded_to_an_even_count() {
    // Read from `push_clut`: VG_LITE_STATES(256, 0x0B00), 256 entries, then one NOP.
    assert_eq!(PALETTE[0], 0x3100_0B00);
    assert_eq!(PALETTE[257], 0x8000_0000);
    assert_eq!(PALETTE[258], 0x3001_0A10);
}

// The checks below were derived by reading the C, not from the recording.

#[test]
fn target_word_of_rgba_is_its_code_with_the_premultiply_bit() {
    // `convert_target_format(RGBA8888)` is 0x23; `set_render_target` ORs `premultiply_dst`
    // 0x100 in and leaves the mirror bit clear (`VG_LITE_ORIENTATION_TOP_BOTTOM` is 0).
    assert_eq!(CLEAR_RECT[1], 0x0000_0123);
    assert_eq!(CLEAR_RECT[0], 0x3001_0A10);
}

#[test]
fn rectangle_packs_two_sixteen_bit_fields_per_word() {
    // `push_rectangle`: DATA(1), a zero word, then x | y << 16 and width | height << 16.
    assert_eq!(
        &CLEAR_RECT[16..20],
        &[0x4000_0001, 0, 0x0001_0001, 0x0002_0003]
    );
}

#[test]
fn every_stream_ends_with_the_target_flush_and_the_end_command() {
    let tail = [0x3001_0A1B, 1, 0x1000_0007, 0, 0x2000_0007, 0, 0, 0];
    for stream in [&INIT[..], &CLEAR_RECT[..], &BLIT_IDENTITY[..], &PALETTE[..]] {
        assert_eq!(&stream[stream.len() - 8..], &tail);
    }
}

#[test]
fn a_draw_outside_the_target_draws_nothing() {
    let (t, s) = (target_buffer(), source_buffer());
    let shifted = plan::Matrix {
        m: [[1.0, 0.0, 100.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    };
    let mut stream = Stream::new(1 << 17, tess());
    let drew = command::encode(
        Op::Blit {
            matrix: shifted,
            blend: BLEND_NONE,
            sampling: Sampling::Point,
        },
        &mut stream,
        bound(&t, Format::Rgba8888, 16, 4, 64),
        Some(bound(&s, Format::Rgba8888, 16, 4, 64)),
    )
    .unwrap();
    assert!(!drew);
    let inside = plan::crop((0, 0, 1, 1));
    let mut stream = Stream::new(1 << 17, tess());
    assert!(
        command::encode(
            Op::Blit {
                matrix: inside,
                blend: BLEND_NONE,
                sampling: Sampling::Point
            },
            &mut stream,
            bound(&t, Format::Rgba8888, 16, 4, 64),
            Some(bound(&s, Format::Rgba8888, 16, 4, 64)),
        )
        .unwrap()
    );
}

#[test]
fn a_singular_or_misaligned_blit_is_invalid_input() {
    let (t, s) = (target_buffer(), source_buffer());
    let singular = plan::Matrix { m: [[0.0; 3]; 3] };
    let mut stream = Stream::new(1 << 17, tess());
    let err = command::encode(
        Op::Blit {
            matrix: singular,
            blend: BLEND_NONE,
            sampling: Sampling::Point,
        },
        &mut stream,
        bound(&t, Format::Rgba8888, 16, 4, 64),
        Some(bound(&s, Format::Rgba8888, 16, 4, 64)),
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    let misaligned = buffer(0x8020_0000);
    let mut stream = Stream::new(1 << 17, tess());
    let err = command::encode(
        Op::Blit {
            matrix: IDENTITY,
            blend: BLEND_NONE,
            sampling: Sampling::Point,
        },
        &mut stream,
        bound(&t, Format::Rgba8888, 16, 4, 64),
        Some(bound(&misaligned, Format::Rgba8888, 16, 4, 60)),
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn an_index_format_is_not_a_render_target() {
    let ix = buffer(0x8050_0000);
    let mut stream = Stream::new(1 << 17, tess());
    let err = command::encode(
        Op::Clear {
            rect: (0, 0, 16, 4),
            color: 0,
        },
        &mut stream,
        bound(&ix, Format::Index8, 16, 4, 16),
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn a_target_too_small_for_its_layout_is_refused_by_the_stream() {
    let small = Scratch::new(0x8000_0000, 255);
    let mut stream = Stream::new(1 << 17, tess());
    let err = stream
        .render_target(
            &small,
            TargetFormat::Rgba8888,
            Layout {
                width: 16,
                height: 4,
                stride: 64,
            },
        )
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn format_codes_are_the_library_convert_tables() {
    let source = |f: Format| f.source().code();
    let target = |f: Format| f.target().map(TargetFormat::code);
    assert_eq!(source(Format::Rgba8888), 0x27);
    assert_eq!(source(Format::Bgra8888), 0x7);
    assert_eq!(source(Format::Rgbx8888), 0x26);
    assert_eq!(source(Format::L8), 0x0);
    assert_eq!(source(Format::A8), 0x2);
    assert_eq!(source(Format::Index8), 0x800);
    assert_eq!(target(Format::A8), Some(0x0));
    assert_eq!(target(Format::L8), Some(0x6));
    assert_eq!(target(Format::Rgba8888), Some(0x23));
    assert_eq!(target(Format::Bgra8888), Some(0x3));
    assert_eq!(target(Format::Rgbx8888), Some(0x22));
    assert_eq!(target(Format::Index8), None);
    // The bytes per pixel the stream checks a layout with are the image's.
    for f in [
        Format::Rgba8888,
        Format::Bgra8888,
        Format::Rgbx8888,
        Format::L8,
        Format::A8,
        Format::Index8,
    ] {
        assert_eq!(f.source().bytes_per_pixel(), f.bytes_per_pixel(), "{f:?}");
        if let Some(t) = f.target() {
            assert_eq!(t.bytes_per_pixel(), f.bytes_per_pixel(), "{f:?}");
        }
    }
}

#[test]
fn blend_codes_are_the_vg_lite_values() {
    assert_eq!(BLEND_NONE, 0x2000);
    assert_eq!(BLEND_SRC_OVER, 0x2001);
    assert_eq!(BLEND_DST_OVER, 0x2002);
    assert_eq!(
        plan::blend(crate::BlendMode::SrcOver, false).unwrap(),
        BLEND_SRC_OVER
    );
}

#[test]
fn the_rgb_word_is_little_endian_rgba() {
    assert_eq!(plan::color([0x11, 0x22, 0x33, 0xFF]), 0xFF33_2211);
}

const INIT: [u32; 32] = [
    0x30010a35, 0x40000000, 0x30010ac8, 0x00010000, 0x30010acb, 0x40010000, 0x30010acc, 0x00001000,
    0x30010a90, 0x00000000, 0x30010a91, 0x00000000, 0x30010a92, 0x00000000, 0x30010a93, 0x00000000,
    0x30010a94, 0x00000000, 0x30010a95, 0x00000000, 0x30010a96, 0x00000000, 0x30010a97, 0x00000000,
    0x30010a1b, 0x00000001, 0x10000007, 0x00000000, 0x20000007, 0x00000000, 0x00000000, 0x00000000,
];

const CLEAR_RECT: [u32; 30] = [
    0x30010a10, 0x00000123, 0x30010a11, 0x80000000, 0x30010a12, 0x00000040, 0x30010a13, 0x00040010,
    0x30010a34, 0x00000000, 0x30010a02, 0xff112233, 0x30010a39, 0x00000000, 0x30010a00, 0x10000001,
    0x40000001, 0x00000000, 0x00010001, 0x00020003, 0x30010a1b, 0x00000001, 0x30010a1b, 0x00000001,
    0x10000007, 0x00000000, 0x20000007, 0x00000000, 0x00000000, 0x00000000,
];

const CLEAR_FULL: [u32; 30] = [
    0x30010a10, 0x00000123, 0x30010a11, 0x80000000, 0x30010a12, 0x00000040, 0x30010a13, 0x00040010,
    0x30010a34, 0x00000000, 0x30010a02, 0xff112233, 0x30010a39, 0x00000000, 0x30010a00, 0x10000004,
    0x40000001, 0x00000000, 0x00000100, 0x00000000, 0x30010a1b, 0x00000001, 0x30010a1b, 0x00000001,
    0x10000007, 0x00000000, 0x20000007, 0x00000000, 0x00000000, 0x00000000,
];

const CLEAR_A8: [u32; 30] = [
    0x30010a10, 0x00000100, 0x30010a11, 0x80100000, 0x30010a12, 0x00000010, 0x30010a13, 0x00040010,
    0x30010a34, 0x00000000, 0x30010a02, 0x000000ff, 0x30010a39, 0x00000000, 0x30010a00, 0x10000004,
    0x40000001, 0x00000000, 0x00000040, 0x00000000, 0x30010a1b, 0x00000001, 0x30010a1b, 0x00000001,
    0x10000007, 0x00000000, 0x20000007, 0x00000000, 0x00000000, 0x00000000,
];

const BLIT_IDENTITY: [u32; 66] = [
    0x30010a10, 0x00000123, 0x30010a11, 0x80000000, 0x30010a12, 0x00000040, 0x30010a13, 0x00040010,
    0x30010ad1, 0x00000000, 0x30010a00, 0x10001001, 0x30010a02, 0x00000000, 0x30010a18, 0x3d000000,
    0x30010a19, 0x3e000000, 0x30010a1a, 0x3f800000, 0x30010a1c, 0x3d800000, 0x30010a1d, 0x00000000,
    0x30010a1e, 0x00000000, 0x30010a1f, 0x00000001, 0x30010a20, 0x00000000, 0x30010a21, 0x3e800000,
    0x30010a22, 0x00000000, 0x30010a25, 0x01000027, 0x30010a27, 0x00000000, 0x30010a29, 0x80200000,
    0x30010a34, 0x00000000, 0x30010a2b, 0x00000040, 0x30010a2d, 0x00000000, 0x30010a2f, 0x00040010,
    0x40000001, 0x00000000, 0x00000000, 0x00040010, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000,
    0x20000007, 0x00000000, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000, 0x20000007, 0x00000000,
    0x00000000, 0x00000000,
];

const BLIT_CROP: [u32; 66] = [
    0x30010a10, 0x00000123, 0x30010a11, 0x80000000, 0x30010a12, 0x00000040, 0x30010a13, 0x00040010,
    0x30010ad1, 0x00000000, 0x30010a00, 0x10001001, 0x30010a02, 0x00000000, 0x30010a18, 0x3e200000,
    0x30010a19, 0x3ec00000, 0x30010a1a, 0x3f800000, 0x30010a1c, 0x3d800000, 0x30010a1d, 0x80000000,
    0x30010a1e, 0x00000000, 0x30010a1f, 0x00000001, 0x30010a20, 0x80000000, 0x30010a21, 0x3e800000,
    0x30010a22, 0x00000000, 0x30010a25, 0x01000027, 0x30010a27, 0x00000000, 0x30010a29, 0x80200000,
    0x30010a34, 0x00000000, 0x30010a2b, 0x00000040, 0x30010a2d, 0x00000000, 0x30010a2f, 0x00040010,
    0x40000001, 0x00000000, 0x00000000, 0x0003000e, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000,
    0x20000007, 0x00000000, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000, 0x20000007, 0x00000000,
    0x00000000, 0x00000000,
];

const BLIT_FLIP_H: [u32; 66] = [
    0x30010a10, 0x00000123, 0x30010a11, 0x80000000, 0x30010a12, 0x00000040, 0x30010a13, 0x00040010,
    0x30010ad1, 0x00000000, 0x30010a00, 0x10001001, 0x30010a02, 0x00000000, 0x30010a18, 0x3f780000,
    0x30010a19, 0x3e000000, 0x30010a1a, 0x3f800000, 0x30010a1c, 0xbd800000, 0x30010a1d, 0x80000000,
    0x30010a1e, 0x00000000, 0x30010a1f, 0x00000001, 0x30010a20, 0x80000000, 0x30010a21, 0x3e800000,
    0x30010a22, 0x00000000, 0x30010a25, 0x01000027, 0x30010a27, 0x00000000, 0x30010a29, 0x80200000,
    0x30010a34, 0x00000000, 0x30010a2b, 0x00000040, 0x30010a2d, 0x00000000, 0x30010a2f, 0x00040010,
    0x40000001, 0x00000000, 0x00000000, 0x00040010, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000,
    0x20000007, 0x00000000, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000, 0x20000007, 0x00000000,
    0x00000000, 0x00000000,
];

const BLIT_ROT90: [u32; 66] = [
    0x30010a10, 0x00000123, 0x30010a11, 0x80300000, 0x30010a12, 0x00000040, 0x30010a13, 0x00100004,
    0x30010ad1, 0x00000000, 0x30010a00, 0x10009001, 0x30010a02, 0x00000000, 0x30010a18, 0x3d000000,
    0x30010a19, 0x3f600000, 0x30010a1a, 0x3f800000, 0x30010a1c, 0x00000000, 0x30010a1d, 0xbe800000,
    0x30010a1e, 0x00000000, 0x30010a1f, 0x00000001, 0x30010a20, 0x3d800000, 0x30010a21, 0x00000000,
    0x30010a22, 0x00000000, 0x30010a25, 0x01000027, 0x30010a27, 0x00000000, 0x30010a29, 0x80200000,
    0x30010a34, 0x00000000, 0x30010a2b, 0x00000040, 0x30010a2d, 0x00000000, 0x30010a2f, 0x00040010,
    0x40000001, 0x00000000, 0x00000000, 0x00100004, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000,
    0x20000007, 0x00000000, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000, 0x20000007, 0x00000000,
    0x00000000, 0x00000000,
];

const RESIZE_HALF: [u32; 66] = [
    0x30010a10, 0x00000123, 0x30010a11, 0x80400000, 0x30010a12, 0x00000040, 0x30010a13, 0x00020008,
    0x30010ad1, 0x00000000, 0x30010a00, 0x10001001, 0x30010a02, 0x00000000, 0x30010a18, 0x3d000000,
    0x30010a19, 0x3e000000, 0x30010a1a, 0x3f800000, 0x30010a1c, 0x3e000000, 0x30010a1d, 0x00000000,
    0x30010a1e, 0x00000000, 0x30010a1f, 0x00000001, 0x30010a20, 0x00000000, 0x30010a21, 0x3f000000,
    0x30010a22, 0x00000000, 0x30010a25, 0x01020027, 0x30010a27, 0x00000000, 0x30010a29, 0x80200000,
    0x30010a34, 0x00000000, 0x30010a2b, 0x00000040, 0x30010a2d, 0x00000000, 0x30010a2f, 0x00040010,
    0x40000001, 0x00000000, 0x00000000, 0x00020008, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000,
    0x20000007, 0x00000000, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000, 0x20000007, 0x00000000,
    0x00000000, 0x00000000,
];

const SRC_OVER_STREAM: [u32; 66] = [
    0x30010a10, 0x00000123, 0x30010a11, 0x80000000, 0x30010a12, 0x00000040, 0x30010a13, 0x00040010,
    0x30010ad1, 0x00000000, 0x30010a00, 0x10001101, 0x30010a02, 0x00000000, 0x30010a18, 0x3d000000,
    0x30010a19, 0x3e000000, 0x30010a1a, 0x3f800000, 0x30010a1c, 0x3d800000, 0x30010a1d, 0x00000000,
    0x30010a1e, 0x00000000, 0x30010a1f, 0x00000001, 0x30010a20, 0x00000000, 0x30010a21, 0x3e800000,
    0x30010a22, 0x00000000, 0x30010a25, 0x010000a7, 0x30010a27, 0x00000000, 0x30010a29, 0x80200000,
    0x30010a34, 0x00000000, 0x30010a2b, 0x00000040, 0x30010a2d, 0x00000000, 0x30010a2f, 0x00040010,
    0x40000001, 0x00000000, 0x00000000, 0x00040010, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000,
    0x20000007, 0x00000000, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000, 0x20000007, 0x00000000,
    0x00000000, 0x00000000,
];

const DST_OVER_STREAM: [u32; 66] = [
    0x30010a10, 0x00000123, 0x30010a11, 0x80000000, 0x30010a12, 0x00000040, 0x30010a13, 0x00040010,
    0x30010ad1, 0x00000000, 0x30010a00, 0x10001201, 0x30010a02, 0x00000000, 0x30010a18, 0x3d000000,
    0x30010a19, 0x3e000000, 0x30010a1a, 0x3f800000, 0x30010a1c, 0x3d800000, 0x30010a1d, 0x00000000,
    0x30010a1e, 0x00000000, 0x30010a1f, 0x00000001, 0x30010a20, 0x00000000, 0x30010a21, 0x3e800000,
    0x30010a22, 0x00000000, 0x30010a25, 0x010000a7, 0x30010a27, 0x00000000, 0x30010a29, 0x80200000,
    0x30010a34, 0x00000000, 0x30010a2b, 0x00000040, 0x30010a2d, 0x00000000, 0x30010a2f, 0x00040010,
    0x40000001, 0x00000000, 0x00000000, 0x00040010, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000,
    0x20000007, 0x00000000, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000, 0x20000007, 0x00000000,
    0x00000000, 0x00000000,
];

const PALETTE: [u32; 324] = [
    0x31000b00, 0xff000000, 0xff000001, 0xff000002, 0xff000003, 0xff000004, 0xff000005, 0xff000006,
    0xff000007, 0xff000008, 0xff000009, 0xff00000a, 0xff00000b, 0xff00000c, 0xff00000d, 0xff00000e,
    0xff00000f, 0xff000010, 0xff000011, 0xff000012, 0xff000013, 0xff000014, 0xff000015, 0xff000016,
    0xff000017, 0xff000018, 0xff000019, 0xff00001a, 0xff00001b, 0xff00001c, 0xff00001d, 0xff00001e,
    0xff00001f, 0xff000020, 0xff000021, 0xff000022, 0xff000023, 0xff000024, 0xff000025, 0xff000026,
    0xff000027, 0xff000028, 0xff000029, 0xff00002a, 0xff00002b, 0xff00002c, 0xff00002d, 0xff00002e,
    0xff00002f, 0xff000030, 0xff000031, 0xff000032, 0xff000033, 0xff000034, 0xff000035, 0xff000036,
    0xff000037, 0xff000038, 0xff000039, 0xff00003a, 0xff00003b, 0xff00003c, 0xff00003d, 0xff00003e,
    0xff00003f, 0xff000040, 0xff000041, 0xff000042, 0xff000043, 0xff000044, 0xff000045, 0xff000046,
    0xff000047, 0xff000048, 0xff000049, 0xff00004a, 0xff00004b, 0xff00004c, 0xff00004d, 0xff00004e,
    0xff00004f, 0xff000050, 0xff000051, 0xff000052, 0xff000053, 0xff000054, 0xff000055, 0xff000056,
    0xff000057, 0xff000058, 0xff000059, 0xff00005a, 0xff00005b, 0xff00005c, 0xff00005d, 0xff00005e,
    0xff00005f, 0xff000060, 0xff000061, 0xff000062, 0xff000063, 0xff000064, 0xff000065, 0xff000066,
    0xff000067, 0xff000068, 0xff000069, 0xff00006a, 0xff00006b, 0xff00006c, 0xff00006d, 0xff00006e,
    0xff00006f, 0xff000070, 0xff000071, 0xff000072, 0xff000073, 0xff000074, 0xff000075, 0xff000076,
    0xff000077, 0xff000078, 0xff000079, 0xff00007a, 0xff00007b, 0xff00007c, 0xff00007d, 0xff00007e,
    0xff00007f, 0xff000080, 0xff000081, 0xff000082, 0xff000083, 0xff000084, 0xff000085, 0xff000086,
    0xff000087, 0xff000088, 0xff000089, 0xff00008a, 0xff00008b, 0xff00008c, 0xff00008d, 0xff00008e,
    0xff00008f, 0xff000090, 0xff000091, 0xff000092, 0xff000093, 0xff000094, 0xff000095, 0xff000096,
    0xff000097, 0xff000098, 0xff000099, 0xff00009a, 0xff00009b, 0xff00009c, 0xff00009d, 0xff00009e,
    0xff00009f, 0xff0000a0, 0xff0000a1, 0xff0000a2, 0xff0000a3, 0xff0000a4, 0xff0000a5, 0xff0000a6,
    0xff0000a7, 0xff0000a8, 0xff0000a9, 0xff0000aa, 0xff0000ab, 0xff0000ac, 0xff0000ad, 0xff0000ae,
    0xff0000af, 0xff0000b0, 0xff0000b1, 0xff0000b2, 0xff0000b3, 0xff0000b4, 0xff0000b5, 0xff0000b6,
    0xff0000b7, 0xff0000b8, 0xff0000b9, 0xff0000ba, 0xff0000bb, 0xff0000bc, 0xff0000bd, 0xff0000be,
    0xff0000bf, 0xff0000c0, 0xff0000c1, 0xff0000c2, 0xff0000c3, 0xff0000c4, 0xff0000c5, 0xff0000c6,
    0xff0000c7, 0xff0000c8, 0xff0000c9, 0xff0000ca, 0xff0000cb, 0xff0000cc, 0xff0000cd, 0xff0000ce,
    0xff0000cf, 0xff0000d0, 0xff0000d1, 0xff0000d2, 0xff0000d3, 0xff0000d4, 0xff0000d5, 0xff0000d6,
    0xff0000d7, 0xff0000d8, 0xff0000d9, 0xff0000da, 0xff0000db, 0xff0000dc, 0xff0000dd, 0xff0000de,
    0xff0000df, 0xff0000e0, 0xff0000e1, 0xff0000e2, 0xff0000e3, 0xff0000e4, 0xff0000e5, 0xff0000e6,
    0xff0000e7, 0xff0000e8, 0xff0000e9, 0xff0000ea, 0xff0000eb, 0xff0000ec, 0xff0000ed, 0xff0000ee,
    0xff0000ef, 0xff0000f0, 0xff0000f1, 0xff0000f2, 0xff0000f3, 0xff0000f4, 0xff0000f5, 0xff0000f6,
    0xff0000f7, 0xff0000f8, 0xff0000f9, 0xff0000fa, 0xff0000fb, 0xff0000fc, 0xff0000fd, 0xff0000fe,
    0xff0000ff, 0x80000000, 0x30010a10, 0x00000123, 0x30010a11, 0x80000000, 0x30010a12, 0x00000040,
    0x30010a13, 0x00040010, 0x30010ad1, 0x00000000, 0x30010a00, 0x10001001, 0x30010a02, 0x00000000,
    0x30010a18, 0x3d000000, 0x30010a19, 0x3e000000, 0x30010a1a, 0x3f800000, 0x30010a1c, 0x3d800000,
    0x30010a1d, 0x00000000, 0x30010a1e, 0x00000000, 0x30010a1f, 0x00000001, 0x30010a20, 0x00000000,
    0x30010a21, 0x3e800000, 0x30010a22, 0x00000000, 0x30010a25, 0x01000800, 0x30010a27, 0x00000000,
    0x30010a29, 0x80500000, 0x30010a34, 0x00000000, 0x30010a2b, 0x00000010, 0x30010a2d, 0x00000000,
    0x30010a2f, 0x00040010, 0x40000001, 0x00000000, 0x00000000, 0x00040010, 0x30010a1b, 0x00000001,
    0x10000007, 0x00000000, 0x20000007, 0x00000000, 0x30010a1b, 0x00000001, 0x10000007, 0x00000000,
    0x20000007, 0x00000000, 0x00000000, 0x00000000,
];

// The stroke draw: the outline (inline data) and the draw words against the vendor's fresh-state
// streams for the same outlines (the oracle's `DRAW_O_*`, one process per fixture).

const STROKE_COLOUR: u32 = 0xFF11_2233;

fn stroke_words(outline: &[u8], target: Bound<'_, Scratch>) -> io::Result<Vec<u32>> {
    let mut stream = Stream::new(1 << 17, tess());
    let drew = command::encode(
        Op::Stroke {
            outline,
            colour: STROKE_COLOUR,
        },
        &mut stream,
        target,
        None,
    )?;
    assert!(drew, "a stroke draws");
    stream.finish()?;
    Ok(stream.words().to_vec())
}

#[test]
fn open_stroke_draws_match_the_vendor_word_for_word() {
    use crate::stroke_tests::*;
    let t = target_buffer();
    let target = bound(&t, Format::Rgba8888, 16, 16, 64);
    let cases: [(&[u8], &[u32]); 17] = [
        (GEOM_O_H_BUTT, &DRAW_O_H_BUTT),
        (GEOM_O_H_SQUARE, &DRAW_O_H_SQUARE),
        (GEOM_O_DIAG_BUTT, &DRAW_O_DIAG_BUTT),
        (GEOM_O_DIAG_SQUARE, &DRAW_O_DIAG_SQUARE),
        (GEOM_O_API_H_SQUARE, &DRAW_O_API_H_SQUARE),
        (GEOM_O_API_DIAG_SQUARE, &DRAW_O_API_DIAG_SQUARE),
        (GEOM_O_POLY_MITER, &DRAW_O_POLY_MITER),
        (GEOM_O_POLY_BEVEL, &DRAW_O_POLY_BEVEL),
        (GEOM_O_POLY_MITER_LIMITED, &DRAW_O_POLY_MITER_LIMITED),
        (GEOM_O_CTL_LIMITED_BEVEL, &DRAW_O_CTL_LIMITED_BEVEL),
        (GEOM_O_POLY_MITER_FALLBACK, &DRAW_O_POLY_MITER_FALLBACK),
        (GEOM_O_CTL_FALLBACK_BEVEL, &DRAW_O_CTL_FALLBACK_BEVEL),
        (GEOM_O_POLY_MITER_SQ, &DRAW_O_POLY_MITER_SQ),
        (GEOM_O_SINGLE_BUTT_WIDE, &DRAW_O_SINGLE_BUTT_WIDE),
        (GEOM_O_POLY3_SQUARE_W3, &DRAW_O_POLY3_SQUARE_W3),
        (GEOM_O_POLY_BEVEL_W2, &DRAW_O_POLY_BEVEL_W2),
        (GEOM_O_DUP_POINTS, &DRAW_O_DUP_POINTS),
    ];
    for (i, (outline, want)) in cases.iter().enumerate() {
        // The oracle submits the context's set-up with the first draw of a process (no finish
        // between init and draw), so the first 24 words of INIT (the tessellation and colour-key
        // writes) come before the draw.
        let mut full = INIT[..24].to_vec();
        full.extend(stroke_words(outline, target).unwrap());
        assert_eq!(full.as_slice(), *want, "fixture {i}");
    }
}

#[test]
fn a_line_is_the_square_capped_stroke_at_pixel_centres() {
    // `line_bytes` from (2, 3) to (12, 3) builds the points (2.5, 3.5) and (12.5, 3.5): the open
    // fixture `o_api_h_square` is exactly that outline.
    use crate::stroke::outline;
    use crate::stroke_tests::GEOM_O_API_H_SQUARE;
    let got = outline(
        &[(2.5, 3.5), (12.5, 3.5)],
        1.0,
        Cap::Square,
        Join::Miter,
        4.0,
        Subpath::Open,
    )
    .unwrap();
    assert_eq!(got.as_slice(), GEOM_O_API_H_SQUARE);
}

#[test]
fn a_stroke_larger_than_the_command_buffer_is_refused() {
    use crate::stroke_tests::GEOM_O_POLY_MITER;
    let t = target_buffer();
    let target = bound(&t, Format::Rgba8888, 16, 16, 64);
    let mut stream = Stream::new(64, tess());
    let err = command::encode(
        Op::Stroke {
            outline: GEOM_O_POLY_MITER,
            colour: STROKE_COLOUR,
        },
        &mut stream,
        target,
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
}

#[test]
fn a_blit_of_an_a8_source_leaves_in_premult_out_as_the_vendor_does() {
    // `vg_lite_blit` sets `in_premult` (0x10000000) for every source but A8.
    let (t, s) = (buffer(0x8010_0000), buffer(0x8020_0000));
    let got = words(
        Op::Blit {
            matrix: IDENTITY,
            blend: BLEND_NONE,
            sampling: Sampling::Point,
        },
        bound(&t, Format::A8, 16, 4, 16),
        Some(bound(&s, Format::A8, 16, 4, 16)),
    );
    assert_eq!(&got[10..12], &[0x3001_0A00, 0x0000_1001]);
    assert_eq!(&got[34..36], &[0x3001_0A25, 0x0100_0002]);
}

#[test]
fn a_palette_needs_index_indices() {
    let (t, s) = (target_buffer(), source_buffer());
    let colours = [0u32; 256];
    let mut stream = Stream::new(1 << 17, tess());
    let err = command::encode(
        Op::Palette { colours: &colours },
        &mut stream,
        bound(&t, Format::Rgba8888, 16, 4, 64),
        Some(bound(&s, Format::Rgba8888, 16, 4, 64)),
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(stream.words().is_empty());
}

#[test]
fn a_source_or_target_smaller_than_its_layout_is_refused_by_the_stream() {
    // The bounds check that stands between an operation and the GPU: a staging buffer one byte
    // short of the layout the operation describes is refused, for the target and the source.
    let (t, s) = (Scratch::new(0x8000_0000, 255), source_buffer());
    let mut stream = Stream::new(1 << 17, tess());
    let err = command::encode(
        Op::Blit {
            matrix: IDENTITY,
            blend: BLEND_NONE,
            sampling: Sampling::Point,
        },
        &mut stream,
        bound(&t, Format::Rgba8888, 16, 4, 64),
        Some(bound(&s, Format::Rgba8888, 16, 4, 64)),
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    let (t, s) = (target_buffer(), Scratch::new(0x8020_0000, 255));
    let mut stream = Stream::new(1 << 17, tess());
    let err = command::encode(
        Op::Blit {
            matrix: IDENTITY,
            blend: BLEND_NONE,
            sampling: Sampling::Point,
        },
        &mut stream,
        bound(&t, Format::Rgba8888, 16, 4, 64),
        Some(bound(&s, Format::Rgba8888, 16, 4, 64)),
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn encoding_a_warm_operation_allocates_nothing() {
    // The steady state: the stream's words and the outline's buffers are reused, so a second
    // round of every operation allocates nothing.
    let (t, s, ix) = (target_buffer(), source_buffer(), buffer(0x8050_0000));
    let colours = [0xFF00_0000u32; 256];
    let mut outliner = Outliner::default();
    let mut spare = Some(Stream::new(1 << 17, tess()));
    let mut round = |outliner: &mut Outliner| {
        let mut stream = spare.take().map(Stream::into_fresh).unwrap();
        let target = bound(&t, Format::Rgba8888, 16, 16, 64);
        let source = bound(&s, Format::Rgba8888, 16, 4, 64);
        let mut drew = command::encode(
            Op::Blit {
                matrix: plan::transform(Transform::Rotate90, 16, 4),
                blend: BLEND_NONE,
                sampling: Sampling::Point,
            },
            &mut stream,
            bound(&t, Format::Rgba8888, 4, 16, 64),
            Some(source),
        )
        .unwrap();
        drew &= command::encode(
            Op::Clear {
                rect: (1, 1, 3, 2),
                color: 1,
            },
            &mut stream,
            target,
            None,
        )
        .unwrap();
        drew &= command::encode(
            Op::Palette { colours: &colours },
            &mut stream,
            target,
            Some(bound(&ix, Format::Index8, 16, 4, 16)),
        )
        .unwrap();
        let outline = outliner
            .outline(
                &[(2.5, 3.5), (12.5, 9.5)],
                1.0,
                Cap::Square,
                Join::Miter,
                4.0,
                Subpath::Open,
            )
            .unwrap();
        drew &=
            command::encode(Op::Stroke { outline, colour: 1 }, &mut stream, target, None).unwrap();
        stream.finish().unwrap();
        assert!(drew);
        spare = Some(stream.into_fresh());
    };
    round(&mut outliner);
    let ((), allocated) = allocations(|| round(&mut outliner));
    assert_eq!(allocated, 0);
}
