//! The board check: each operation through a real GPU, read back and compared with small
//! references written here (exact copies, flips, turns, crops, fills, palettes, an opaque
//! SRC_OVER, a flat source through each resize filter, an axis-aligned line and stroke, and the
//! dma-buf path). Ignored by default; run with `--ignored`.
//!
//! - `LEMNOS_VGLITE_NODE`: the GPU's node, default `/dev/vg_lite`.
//! - `LEMNOS_VGLITE_HEAP`: the DMA heap the dma-buf check takes its buffers from, default
//!   `/dev/dma_heap/system`.
//!
//! Carried from the dev branch's `tests/vglite.rs`, which skipped without the device.

#![cfg(target_os = "linux")]
#![expect(
    clippy::print_stderr,
    reason = "the check reports what it staged for the board run's record"
)]

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, OwnedFd};

use lemnos_driver_vglite::{
    BlendMode, Cap, Filter, Format, Frame, Image, Join, RunError, Stroke, Transform, Vglite,
};
use lemnos_linux_sys::dma_buf::{self, SYNC_END, SYNC_READ, SYNC_START, SYNC_WRITE};
use lemnos_linux_sys::{dma_heap, mmap::Mapping};

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn gpu() -> Vglite {
    Vglite::new(env("LEMNOS_VGLITE_NODE", "/dev/vg_lite"))
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 + 7) as u8).collect()
}

/// `out` pixel (x, y) is `src` pixel `from(x, y)`; tightly packed `bpp`-byte pixels.
fn remap(
    src: &[u8],
    width: usize,
    bpp: usize,
    (out_w, out_h): (usize, usize),
    from: impl Fn(usize, usize) -> (usize, usize),
) -> Vec<u8> {
    let mut out = vec![0u8; out_w * out_h * bpp];
    for y in 0..out_h {
        for x in 0..out_w {
            let (sx, sy) = from(x, y);
            let (s, d) = ((sy * width + sx) * bpp, (y * out_w + x) * bpp);
            out[d..d + bpp].copy_from_slice(&src[s..s + bpp]);
        }
    }
    out
}

#[test]
#[ignore = "needs the GPU: a board run"]
fn crops_flips_and_turns_exactly() {
    let mut gpu = gpu();
    let src = Image::packed(Format::Rgba8888, 64, 32);
    let from = pattern(src.frame_len());
    let (w, h) = (64, 32);

    let crop = Image::packed(Format::Rgba8888, 16, 8);
    let mut got = vec![0u8; crop.frame_len()];
    gpu.crop_bytes(0, src, &from, (5, 3, 16, 8), crop, &mut got)
        .unwrap();
    let want = remap(&from, w, 4, (16, 8), |x, y| (x + 5, y + 3));
    assert_eq!(got, want, "crop");

    let mut got = vec![0u8; src.frame_len()];
    gpu.transform_bytes(0, src, &from, src, &mut got, Transform::FlipHorizontal)
        .unwrap();
    assert_eq!(
        got,
        remap(&from, w, 4, (w, h), |x, y| (w - 1 - x, y)),
        "flip"
    );

    let mut got = vec![0u8; src.frame_len()];
    gpu.transform_bytes(0, src, &from, src, &mut got, Transform::Rotate180)
        .unwrap();
    assert_eq!(
        got,
        remap(&from, w, 4, (w, h), |x, y| (w - 1 - x, h - 1 - y)),
        "rotate 180"
    );

    // A clockwise quarter turn: output (x, y) is source (y, H - 1 - x).
    let turned = Image::packed(Format::Rgba8888, h as u32, w as u32);
    let mut got = vec![0u8; turned.frame_len()];
    gpu.transform_bytes(0, src, &from, turned, &mut got, Transform::Rotate90)
        .unwrap();
    let want = remap(&from, w, 4, (h, w), |x, y| (y, h - 1 - x));
    assert_eq!(got, want, "rotate 90");
}

#[test]
#[ignore = "needs the GPU: a board run"]
fn fills_rectangles_exactly() {
    let mut gpu = gpu();
    let img = Image::packed(Format::Rgba8888, 16, 8);
    let mut buf = vec![0x10u8; img.frame_len()];
    gpu.fill_bytes(0, img, &mut buf, (2, 1, 5, 3), [1, 2, 3, 4])
        .unwrap();
    for y in 0..8usize {
        for x in 0..16usize {
            let at = (y * 16 + x) * 4;
            let inside = (2..7).contains(&x) && (1..4).contains(&y);
            let want = if inside { [1, 2, 3, 4] } else { [0x10; 4] };
            assert_eq!(&buf[at..at + 4], &want, "({x}, {y})");
        }
    }

    // An A8 mask takes a colour whose four bytes agree, whichever byte the unit reads as alpha.
    let mask = Image::packed(Format::A8, 16, 4);
    let mut buf = vec![0u8; mask.frame_len()];
    gpu.fill_bytes(0, mask, &mut buf, (0, 0, 16, 4), [0x7F; 4])
        .unwrap();
    assert!(buf.iter().all(|&b| b == 0x7F), "a8");
}

#[test]
#[ignore = "needs the GPU: a board run"]
fn palettes_exactly() {
    let mut gpu = gpu();
    let lut: [[u8; 4]; 256] = std::array::from_fn(|i| [i as u8, 255 - i as u8, (3 * i) as u8, 255]);
    let indices = pattern(16 * 4);
    let mut got = vec![0u8; 16 * 4 * 4];
    gpu.palette_bytes(0, (16, 4), &indices, &lut, &mut got)
        .unwrap();
    let want: Vec<u8> = indices.iter().flat_map(|&i| lut[i as usize]).collect();
    assert_eq!(got, want, "palette");
}

#[test]
#[ignore = "needs the GPU: a board run"]
fn blends_an_opaque_source_over_as_a_copy() {
    let mut gpu = gpu();
    let img = Image::packed(Format::Rgba8888, 16, 4);
    let mut fg = pattern(img.frame_len());
    for px in fg.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    let mut dst = vec![0x33u8; img.frame_len()];
    gpu.blend_bytes(0, img, &fg, img, &mut dst, BlendMode::SrcOver, false)
        .unwrap();
    assert_eq!(dst, fg, "src over");
}

#[test]
#[ignore = "needs the GPU: a board run"]
fn keeps_a_flat_source_flat_under_resize() {
    let mut gpu = gpu();
    let src = Image::packed(Format::Rgba8888, 16, 8);
    let flat: Vec<u8> = [0x40u8, 0x80, 0xC0, 0xFF].repeat(16 * 8);
    let dst = Image::packed(Format::Rgba8888, 8, 4);
    for filter in [Filter::Bilinear, Filter::Area] {
        let mut out = vec![0u8; dst.frame_len()];
        gpu.resize_bytes(0, src, &flat, dst, &mut out, filter)
            .unwrap();
        assert!(
            out.as_chunks::<4>()
                .0
                .iter()
                .all(|px| *px == [0x40, 0x80, 0xC0, 0xFF]),
            "{filter:?}"
        );
    }
}

/// A horizontal line from (1, 3) to (6, 3) covers exactly pixels 1 to 6 of row 3 (both ends
/// included), and a 2-wide butt stroke from (1, 2) to (7, 2) exactly rows 1 and 2 of columns 1 to
/// 6; nothing else changes.
#[test]
#[ignore = "needs the GPU: a board run"]
fn draws_an_axis_aligned_line_and_stroke_on_exact_pixels() {
    let mut gpu = gpu();
    let img = Image::packed(Format::Rgba8888, 8, 8);
    let mut buf = vec![0u8; img.frame_len()];
    gpu.line_bytes(0, img, &mut buf, (1, 3), (6, 3), [255; 4])
        .unwrap();
    let mut wide = vec![0u8; img.frame_len()];
    let stroke = Stroke {
        width: 2.0,
        cap: Cap::Butt,
        join: Join::Miter,
        miter_limit: 4.0,
        closed: false,
    };
    gpu.stroke_bytes(
        0,
        img,
        &mut wide,
        &[(1.0, 2.0), (7.0, 2.0)],
        stroke,
        [255; 4],
    )
    .unwrap();
    for y in 0..8u32 {
        for x in 0..8u32 {
            let at = ((y * 8 + x) * 4) as usize;
            let line = y == 3 && (1..=6).contains(&x);
            let band = (1..=2).contains(&y) && (1..=6).contains(&x);
            assert_eq!(
                buf[at..at + 4],
                if line { [255; 4] } else { [0; 4] },
                "line {x},{y}"
            );
            assert_eq!(
                wide[at..at + 4],
                if band { [255; 4] } else { [0; 4] },
                "stroke {x},{y}"
            );
        }
    }
}

#[test]
#[ignore = "needs the GPU: a board run"]
fn refuses_l8_lines() {
    let mut gpu = gpu();
    let img = Image::packed(Format::L8, 8, 8);
    let mut buf = vec![0u8; img.frame_len()];
    let err = gpu
        .line_bytes(0, img, &mut buf, (0, 0), (7, 7), [255; 4])
        .unwrap_err();
    assert!(matches!(err, RunError::Refused(_)));
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

fn write(buffer: &OwnedFd, pixels: &[u8]) {
    let mut map = Mapping::new(buffer.as_fd(), pixels.len(), 0, true).expect("map");
    dma_buf::sync(buffer.as_fd(), SYNC_START | SYNC_WRITE).expect("sync");
    map.as_mut_slice().copy_from_slice(pixels);
    dma_buf::sync(buffer.as_fd(), SYNC_END | SYNC_WRITE).expect("sync");
}

fn read(buffer: &OwnedFd, len: usize) -> Vec<u8> {
    let map = Mapping::new(buffer.as_fd(), len, 0, false).expect("map");
    dma_buf::sync(buffer.as_fd(), SYNC_START | SYNC_READ).expect("sync");
    let out = map.as_slice().to_vec();
    dma_buf::sync(buffer.as_fd(), SYNC_END | SYNC_READ).expect("sync");
    out
}

/// Dma-bufs from a heap, staged both ways.
#[test]
#[ignore = "needs the GPU: a board run"]
fn takes_dmabufs_through_staging() {
    let mut gpu = gpu();
    let img = Image::packed(Format::Rgba8888, 64, 32);
    let from = pattern(img.frame_len());
    let heap_path = env("LEMNOS_VGLITE_HEAP", "/dev/dma_heap/system");
    let heap = File::open(&heap_path).expect("the heap");
    let len = img.frame_len() as u64;
    let src = dma_heap::allocate(heap.as_fd(), len).expect("a dma-buf");
    let dst = dma_heap::allocate(heap.as_fd(), len).expect("a dma-buf");
    write(&src, &from);
    let cost = gpu
        .transform_dmabuf(
            0,
            Frame {
                image: img,
                fd: src.as_fd(),
            },
            Frame {
                image: img,
                fd: dst.as_fd(),
            },
            Transform::Rotate180,
        )
        .unwrap();
    eprintln!("{heap_path}: {} bytes staged", cost.staged_bytes);
    let want = remap(&from, 64, 4, (64, 32), |x, y| (63 - x, 31 - y));
    assert_eq!(read(&dst, from.len()), want);
    assert_eq!(cost.staged_bytes, 2 * from.len());
}
