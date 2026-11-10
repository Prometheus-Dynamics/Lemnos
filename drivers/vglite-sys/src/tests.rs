//! The layouts against the host C compiler, and every check of the stream.
//!
//! The sizes and offsets below were printed by `cc` (x86-64, LP64, the riscv64 Linux ABI's
//! layout too) from `vg_lite_kernel.h` and `vg_lite_ioctl.h` of `kendryte/k230_linux_sdk` at
//! `fc6cc21`, with a small program of `sizeof` and `offsetof` lines; they agree with the dev
//! branch's pinned values.

use super::*;
use std::mem::offset_of;

use lemnos_measure::test_alloc::allocations;

#[test]
fn numbers_match_the_header() {
    assert_eq!(IOCTL, 30_000);
    assert_eq!(
        [
            command::INITIALIZE,
            command::TERMINATE,
            command::ALLOCATE,
            command::FREE,
            command::SUBMIT,
            command::WAIT,
            command::MAP,
            command::UNMAP,
            command::CHECK,
        ],
        [0, 1, 2, 3, 4, 5, 8, 9, 10]
    );
    assert_eq!(
        [
            error::SUCCESS,
            error::INVALID_ARGUMENT,
            error::OUT_OF_MEMORY,
            error::NO_CONTEXT,
            error::TIMEOUT,
            error::OUT_OF_RESOURCES,
            error::GENERIC_IO,
            error::NOT_SUPPORT,
            error::ALREADY_EXISTS,
            error::NOT_ALIGNED,
            error::FLEXA_TIME_OUT,
            error::FLEXA_HANDSHAKE_FAIL,
            error::SYSTEM_CALL_FAIL,
        ],
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
    );
    assert_eq!(MAP_DMABUF, 0x4);
    assert_eq!(CMDBUF_COUNT, 2);
    assert_eq!(INFINITE, 0xFFFF_FFFF);
    assert_eq!(WINDOW_BYTES, 0x0200_0000);
}

#[test]
fn the_layouts_match_the_c_compiler() {
    assert_eq!(size_of::<IoctlData>(), 24);
    assert_eq!(offset_of!(IoctlData, error), 4);
    assert_eq!(offset_of!(IoctlData, bytes), 8);
    assert_eq!(offset_of!(IoctlData, buffer), 16);

    assert_eq!(size_of::<Initialize>(), 88);
    assert_eq!(offset_of!(Initialize, tess_height), 8);
    assert_eq!(offset_of!(Initialize, context), 16);
    assert_eq!(offset_of!(Initialize, capabilities), 24);
    assert_eq!(offset_of!(Initialize, command_buffer), 32);
    assert_eq!(offset_of!(Initialize, command_buffer_gpu), 48);
    assert_eq!(offset_of!(Initialize, physical_addr), 56);
    assert_eq!(offset_of!(Initialize, logical_addr), 64);
    assert_eq!(offset_of!(Initialize, tessbuf_size), 72);
    assert_eq!(offset_of!(Initialize, countbuf_size), 76);
    assert_eq!(offset_of!(Initialize, tess_w_h), 80);

    assert_eq!(size_of::<KernelContext>(), 128);
    assert_eq!(offset_of!(KernelContext, command_buffer_physical), 48);
    assert_eq!(offset_of!(KernelContext, tess_buffer), 56);
    assert_eq!(offset_of!(KernelContext, tessbuf_physical), 80);
    assert_eq!(offset_of!(KernelContext, power_context), 88);
    assert_eq!(offset_of!(KernelContext, power_context_physical), 112);
    assert_eq!(offset_of!(KernelContext, power_context_capacity), 120);

    assert_eq!(size_of::<Allocate>(), 48);
    assert_eq!(offset_of!(Allocate, contiguous), 4);
    assert_eq!(offset_of!(Allocate, flags), 8);
    assert_eq!(offset_of!(Allocate, memory_handle), 16);
    assert_eq!(offset_of!(Allocate, memory), 24);
    assert_eq!(offset_of!(Allocate, kmemory), 32);
    assert_eq!(offset_of!(Allocate, memory_gpu), 40);
    assert_eq!(size_of::<Free>(), 8);

    assert_eq!(size_of::<Map>(), 40);
    assert_eq!(offset_of!(Map, bytes), 4);
    assert_eq!(offset_of!(Map, logical), 8);
    assert_eq!(offset_of!(Map, physical), 16);
    assert_eq!(offset_of!(Map, dma_buf_fd), 20);
    assert_eq!(offset_of!(Map, memory_handle), 24);
    assert_eq!(offset_of!(Map, memory_gpu), 32);
    assert_eq!(size_of::<Unmap>(), 8);

    assert_eq!(size_of::<Submit>(), 24);
    assert_eq!(offset_of!(Submit, commands), 8);
    assert_eq!(offset_of!(Submit, command_size), 16);
    assert_eq!(offset_of!(Submit, command_id), 20);
    assert_eq!(size_of::<Wait>(), 24);
    assert_eq!(offset_of!(Wait, timeout_ms), 8);
    assert_eq!(offset_of!(Wait, event_mask), 12);
    assert_eq!(offset_of!(Wait, event_got), 16);

    assert_eq!(size_of::<Terminate>(), 8);
    assert_eq!(size_of::<Info>(), 8);
    assert_eq!(offset_of!(Info, reg), 4);
}

#[test]
fn identification_reaches_three_registers() {
    assert_eq!(Identification::ChipId.address(), 0x20);
    assert_eq!(Identification::Revision.address(), 0x24);
    assert_eq!(Identification::Cid.address(), 0x30);
}

#[test]
fn opening_a_missing_node_fails_without_touching_anything() {
    let err = Device::open("/nonexistent/vg_lite").unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
}

// The stream.

fn tess() -> Tessellation {
    Tessellation {
        physical: 0x4000_0000,
        size: 0x1_0000,
        count_size: 0x1000,
        tess_w_h: 0,
    }
}

fn layout(width: u32, height: u32, stride: u32) -> Layout {
    Layout {
        width,
        height,
        stride,
    }
}

fn kind<T: std::fmt::Debug>(r: io::Result<T>) -> io::ErrorKind {
    r.expect_err("must be refused").kind()
}

const STATE: u32 = 0x3001_0000;

#[test]
fn addresses_come_from_buffers_and_their_extent_is_checked() {
    let mut s = Stream::new(1 << 17, tess());
    // 64 bytes a row times 4 rows is 256: one byte short of this buffer.
    let small = Scratch::new(0x8000_0000, 255);
    assert_eq!(
        kind(s.render_target(&small, TargetFormat::Rgba8888, layout(16, 4, 64))),
        io::ErrorKind::InvalidInput
    );
    let exact = Scratch::new(0x8000_0000, 256);
    // A stride that holds no row at the format's bytes per pixel, a zero dimension, a span past
    // the GPU address space, a width past the 16-bit field, a stride reaching the tiling flag.
    assert!(
        s.render_target(&exact, TargetFormat::Rgba8888, layout(16, 4, 60))
            .is_err()
    );
    assert!(
        s.render_target(&exact, TargetFormat::Rgba8888, layout(0, 4, 64))
            .is_err()
    );
    let high = Scratch::new(0xFFFF_FF01, 1 << 20);
    assert!(
        s.source(
            &high,
            SourceFormat::Rgba8888,
            layout(16, 4, 64),
            SourceOptions::default()
        )
        .is_err()
    );
    let huge = Scratch::new(0, usize::MAX);
    assert!(
        s.render_target(&huge, TargetFormat::A8, layout(65_536, 1, 65_536))
            .is_err()
    );
    assert!(
        s.render_target(&huge, TargetFormat::A8, layout(16, 1, 0x1000_0000))
            .is_err()
    );
    assert!(s.words().is_empty(), "a refused call adds nothing");
    s.render_target(&exact, TargetFormat::Rgba8888, layout(16, 4, 64))
        .unwrap();
    assert_eq!(
        s.words(),
        &[
            STATE | 0x0A10,
            0x123,
            STATE | 0x0A11,
            0x8000_0000,
            STATE | 0x0A12,
            64,
            STATE | 0x0A13,
            16 | (4 << 16),
        ]
    );
}

#[test]
fn the_format_sets_the_bytes_per_pixel_the_extent_is_checked_with() {
    // 16 pixels of one byte fit a 16-byte row; 16 pixels of four do not. The format is what the
    // GPU writes, so a one-byte layout cannot be claimed for a four-byte target.
    let buffer = Scratch::new(0x8000_0000, 64);
    let mut s = Stream::new(1 << 17, tess());
    s.render_target(&buffer, TargetFormat::A8, layout(16, 4, 16))
        .unwrap();
    assert!(
        s.render_target(&buffer, TargetFormat::Bgra8888, layout(16, 4, 16))
            .is_err()
    );
    assert!(
        s.source(
            &buffer,
            SourceFormat::Rgbx8888,
            layout(16, 4, 16),
            SourceOptions::default()
        )
        .is_err()
    );
    s.source(
        &buffer,
        SourceFormat::Index8,
        layout(16, 4, 16),
        SourceOptions {
            bilinear: true,
            luma_conversion: true,
            read_split: true,
        },
    )
    .unwrap();
    // The source word carries the format and the three options, and nothing else.
    let word = s.words()[9];
    assert_eq!(s.words()[8], STATE | 0x0A25);
    assert_eq!(word, 0x800 | SOURCE_REGISTER_BITS);
}

#[test]
fn the_configuration_word_takes_only_the_encoders_bits() {
    let mut s = Stream::new(1 << 17, tess());
    // The modes, with the blends and transparency; an A8 blit leaves out `in_premult`.
    for word in [
        0x1000_0001,
        0x1000_0004,
        0x1000_1001,
        0x1000_9301,
        0x1000_0000,
        0x1001,
    ] {
        s.config(word).unwrap();
    }
    // The target tiling (0x40), the stripe mode (bit 29) and the FIFO bit (bit 7) of
    // `vg_lite.c`, any other bit, and modes that name no primitive are refused.
    for word in [
        0x1000_0041,
        0x3000_1001,
        0x1000_1081,
        0x1000_0000 | 0x0004_0000,
        0x1000_0005,
        0x1000_1004,
        0x1000_1000,
        0xFFFF_FFFF,
    ] {
        assert_eq!(
            kind(s.config(word)),
            io::ErrorKind::InvalidInput,
            "{word:#x}"
        );
    }
}

#[test]
fn rectangles_and_clears_stay_inside_the_target_and_their_mode() {
    let mut s = Stream::new(1 << 17, tess());
    s.config(0x1000_0001).unwrap();
    assert!(s.rectangle(0, 0, 1, 1).is_err(), "no target yet");
    let target = Scratch::new(0x8000_0000, 256);
    s.render_target(&target, TargetFormat::Rgba8888, layout(16, 4, 64))
        .unwrap();
    s.rectangle(1, 1, 3, 2).unwrap();
    assert!(s.rectangle(14, 0, 3, 1).is_err(), "runs past the width");
    assert!(s.rectangle(0, 3, 1, 2).is_err(), "runs past the height");
    assert!(s.rectangle(0, 0, 0, 1).is_err(), "empty");
    assert!(
        s.rectangle(u32::MAX, 0, 2, 1).is_err(),
        "wraps on the addition"
    );
    // A pixel-engine clear needs its own mode, and stays inside the extent.
    assert!(s.pe_clear(256).is_err(), "rectangle mode");
    s.config(0x1000_0004).unwrap();
    assert!(s.rectangle(0, 0, 1, 1).is_err(), "pixel-engine mode");
    s.pe_clear(256).unwrap();
    assert!(s.pe_clear(257).is_err(), "past the target's extent");
    // In the image mode a rectangle needs a source in this stream.
    s.config(0x1000_1001).unwrap();
    assert!(s.rectangle(0, 0, 16, 4).is_err(), "no source yet");
    let source = Scratch::new(0x8020_0000, 256);
    s.source(
        &source,
        SourceFormat::Rgba8888,
        layout(16, 4, 64),
        SourceOptions::default(),
    )
    .unwrap();
    s.rectangle(0, 0, 16, 4).unwrap();
    // The path mode takes no rectangle.
    s.config(0x1000_0000).unwrap();
    assert!(s.rectangle(0, 0, 1, 1).is_err(), "path mode");
}

#[test]
fn colour_table_and_tessellation_come_from_the_stream_and_the_context() {
    let mut s = Stream::new(1 << 17, tess());
    s.tessellation_setup().unwrap();
    assert_eq!(
        s.words(),
        &[
            STATE | 0x0A35,
            0x4000_0000,
            STATE | 0x0AC8,
            0x1_0000,
            STATE | 0x0ACB,
            0x4001_0000,
            STATE | 0x0ACC,
            0x1000,
        ]
    );
    let mut t = Stream::new(1 << 17, tess());
    t.clut(&[7; 256]).unwrap();
    assert_eq!(t.words()[0], 0x3100_0B00);
    assert_eq!(t.words()[257], 0x8000_0000);
    // A tessellation buffer that wraps the address space is refused.
    let wraps = Tessellation {
        physical: 0xFFFF_F000,
        ..tess()
    };
    assert!(Stream::new(1 << 17, wraps).tessellation_setup().is_err());
}

#[test]
fn only_finished_kernel_streams_are_submittable() {
    let mut s: Stream<'_> = Stream::new(1 << 17, tess());
    assert!(s.submittable(1 << 17).is_err(), "not finished");
    s.finish().unwrap();
    s.submittable(1 << 17).unwrap();
    assert!(s.submittable(8).is_err(), "larger than the command buffer");
    assert!(s.finish().is_err(), "nothing may follow the end");
    assert!(s.colour(0).is_err(), "nothing may follow the end");

    let mut scratch = Stream::new(1 << 17, tess());
    let buffer = Scratch::new(0x8000_0000, 256);
    scratch
        .render_target(&buffer, TargetFormat::Rgba8888, layout(16, 4, 64))
        .unwrap();
    scratch.finish().unwrap();
    assert!(
        scratch.submittable(1 << 17).is_err(),
        "scratch memory is refused"
    );
}

#[test]
fn zero_writes_reach_only_the_zero_registers() {
    let mut s = Stream::new(1 << 17, tess());
    for address in [0x0A27, 0x0A2D, 0x0A34, 0x0A39, 0x0AD1] {
        s.zero(address).unwrap();
    }
    for address in [
        0x0A00, 0x0A10, 0x0A11, 0x0A25, 0x0A29, 0x0A35, 0x0ACB, 0x0B00,
    ] {
        assert!(s.zero(address).is_err(), "{address:#x}");
    }
}

#[test]
fn path_data_is_inline_whole_words_after_its_set_up() {
    let target = Scratch::new(0x8000_0000, 0x1000);
    let mut s = Stream::new(1024, tess());
    // Not whole words, and empty, are refused before anything else.
    assert!(s.path_data(&[0; 6]).is_err());
    assert!(s.path_data(&[]).is_err());
    let data = [1, 0, 0, 0, 2, 0, 0, 0];
    assert!(s.path_data(&data).is_err(), "no target");
    s.render_target(&target, TargetFormat::Rgba8888, layout(16, 4, 64))
        .unwrap();
    s.config(0x1000_0000).unwrap();
    assert!(s.path_data(&data).is_err(), "no window");
    s.tessellation_window(0, 0, 16, 4).unwrap();
    assert!(s.path_data(&data).is_err(), "no path control");
    s.path_control(0x0130_0200).unwrap();
    s.config(0x1000_0001).unwrap();
    assert!(s.path_data(&data).is_err(), "not the path mode");
    s.config(0x1000_0000).unwrap();
    s.zero(0x0A34).unwrap();
    assert!(s.path_data(&data).is_err(), "path control zeroed");
    s.path_control(0x0130_0200).unwrap();
    let before = s.words().len();
    // 8 bytes: a header of one unit, a zero word, the data, no padding.
    s.path_data(&data).unwrap();
    assert_eq!(&s.words()[before..], &[0x4000_0001, 0, 1, 2]);
    // 12 bytes: two units, padded with one zero word.
    let before = s.words().len();
    s.path_data(&[3, 0, 0, 0, 4, 0, 0, 0, 5, 0, 0, 0]).unwrap();
    assert_eq!(&s.words()[before..], &[0x4000_0002, 0, 3, 4, 5, 0]);
    // A block larger than the buffer is refused and adds nothing.
    let before = s.words().len();
    assert_eq!(kind(s.path_data(&[0; 1024])), io::ErrorKind::OutOfMemory);
    assert_eq!(s.words().len(), before);
}

#[test]
fn path_control_takes_fp32_with_the_stroke_and_quality_bits_only() {
    let mut s = Stream::new(1 << 17, tess());
    for word in [0x0130_0000, 0x0130_0200, 0x0130_0203] {
        s.path_control(word).unwrap();
    }
    // Tiling (0x2000000), the even-odd fill rule (0x10), another data format, no control bit.
    for word in [0x0330_0200, 0x0130_0210, 0x0110_0200, 0x0030_0200, 0] {
        assert!(s.path_control(word).is_err(), "{word:#x}");
    }
}

#[test]
fn the_tessellation_window_is_inside_the_target_and_its_size_is_the_contexts() {
    let buffer = Scratch::new(0x8000_0000, 0x1000);
    let mut s = Stream::new(1 << 16, tess());
    assert!(s.tessellation_window(0, 0, 4, 4).is_err(), "no target");
    s.render_target(&buffer, TargetFormat::Rgba8888, layout(16, 4, 64))
        .unwrap();
    assert!(s.tessellation_window(13, 0, 4, 4).is_err());
    let before = s.words().len();
    s.tessellation_window(0, 0, 16, 4).unwrap();
    assert_eq!(
        &s.words()[before..],
        &[
            0x3001_0a1b,
            0x0001_1000,
            0x3001_0a3d,
            0x400,
            0x3001_0a39,
            0,
            0x3001_0a3a,
            0x0004_0010,
        ]
    );
}

#[test]
fn the_stream_never_grows_past_one_command_buffer() {
    // 64 bytes: 16 words. Four colour writes fill eight; the colour table never fits.
    let mut s = Stream::new(64, tess());
    for _ in 0..4 {
        s.colour(1).unwrap();
    }
    assert_eq!(kind(s.clut(&[0; 256])), io::ErrorKind::OutOfMemory);
    assert_eq!(s.words().len(), 8);
    assert_eq!(
        kind(s.steps([0.0; 3], [0.0; 3], [0.0; 3])),
        io::ErrorKind::OutOfMemory
    );
    s.finish().unwrap();
    assert_eq!(s.words().len(), 16);
}

#[test]
fn a_fresh_stream_keeps_its_words_and_allocates_nothing() {
    let buffer = Scratch::new(0x8000_0000, 0x1000);
    let mut s = Stream::new(1 << 17, tess());
    s.render_target(&buffer, TargetFormat::Rgba8888, layout(16, 4, 64))
        .unwrap();
    s.finish().unwrap();
    let (fresh, allocated) = allocations(|| {
        let mut fresh = s.into_fresh();
        fresh
            .render_target(&buffer, TargetFormat::Rgba8888, layout(16, 4, 64))
            .unwrap();
        fresh.config(0x1000_0001).unwrap();
        fresh.rectangle(0, 0, 4, 4).unwrap();
        fresh.clut(&[0; 256]).unwrap();
        fresh.finish().unwrap();
        fresh
    });
    assert_eq!(allocated, 0);
    assert_eq!(fresh.words().len(), 8 + 2 + 4 + 258 + 8);
}
