//! VGLite on a K230 or K230D: the operations checked against small references written here
//! (exact copies, flips, turns, crops, fills, palettes), on `/dev/vg_lite`. Skips where the device
//! is absent. Untested on board: a run on a K230 is what turns these from compiled into measured.
//!
//! Cut from the old suite: its resize and line comparisons used the reference kernels of the
//! source repository, which this crate no longer depends on. Resize is checked here only on a flat
//! source (which any filter must keep flat); an axis-aligned line is checked on exact pixels, and
//! an L8 line is refused.
#![cfg(target_os = "linux")]
#![allow(clippy::print_stderr)]

use std::io;
use std::os::fd::AsFd;

use lemnos_drivers_vglite::{Backing, BlendMode, Filter, Format, Frame, Image, Transform, Vglite};
use lemnos_linux::dma::{Access, DmaBuf, DmaHeap};

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

fn gpu() -> Option<Vglite> {
    match Vglite::load() {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            eprintln!("skipping: {e}");
            None
        }
    }
}

#[test]
fn vglite_crops_flips_and_turns_exactly() {
    let Some(gpu) = gpu() else { return };
    let src = Image::packed(Format::Rgba8888, 64, 32);
    let from = pattern(src.frame_len());
    let (w, h) = (64, 32);

    let crop = Image::packed(Format::Rgba8888, 16, 8);
    let mut got = vec![0u8; crop.frame_len()];
    gpu.crop_bytes(src, &from, (5, 3, 16, 8), crop, &mut got)
        .unwrap();
    let want = remap(&from, w, 4, (16, 8), |x, y| (x + 5, y + 3));
    assert_eq!(got, want, "crop");

    let mut got = vec![0u8; src.frame_len()];
    gpu.transform_bytes(src, &from, src, &mut got, Transform::FlipHorizontal)
        .unwrap();
    assert_eq!(
        got,
        remap(&from, w, 4, (w, h), |x, y| (w - 1 - x, y)),
        "flip"
    );

    let mut got = vec![0u8; src.frame_len()];
    gpu.transform_bytes(src, &from, src, &mut got, Transform::Rotate180)
        .unwrap();
    assert_eq!(
        got,
        remap(&from, w, 4, (w, h), |x, y| (w - 1 - x, h - 1 - y)),
        "rotate 180"
    );

    // A clockwise quarter turn: output (x, y) is source (y, H - 1 - x).
    let turned = Image::packed(Format::Rgba8888, h as u32, w as u32);
    let mut got = vec![0u8; turned.frame_len()];
    gpu.transform_bytes(src, &from, turned, &mut got, Transform::Rotate90)
        .unwrap();
    let want = remap(&from, w, 4, (h, w), |x, y| (y, h - 1 - x));
    assert_eq!(got, want, "rotate 90");
}

#[test]
fn vglite_fills_rectangles_exactly() {
    let Some(gpu) = gpu() else { return };
    let img = Image::packed(Format::Rgba8888, 16, 8);
    let mut buf = vec![0x10u8; img.frame_len()];
    gpu.fill_bytes(img, &mut buf, (2, 1, 5, 3), [1, 2, 3, 4])
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
    gpu.fill_bytes(mask, &mut buf, (0, 0, 16, 4), [0x7F; 4])
        .unwrap();
    assert!(buf.iter().all(|&b| b == 0x7F), "a8");
}

#[test]
fn vglite_palettes_exactly() {
    let Some(gpu) = gpu() else { return };
    let lut: [[u8; 4]; 256] = std::array::from_fn(|i| [i as u8, 255 - i as u8, (3 * i) as u8, 255]);
    let indices = pattern(16 * 4);
    let mut got = vec![0u8; 16 * 4 * 4];
    gpu.palette_bytes(16, 4, &indices, &lut, &mut got).unwrap();
    let want: Vec<u8> = indices.iter().flat_map(|&i| lut[i as usize]).collect();
    assert_eq!(got, want, "palette");
}

#[test]
fn vglite_blends_an_opaque_source_over_as_a_copy() {
    let Some(gpu) = gpu() else { return };
    let img = Image::packed(Format::Rgba8888, 16, 4);
    let mut fg = pattern(img.frame_len());
    for px in fg.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    let mut dst = vec![0x33u8; img.frame_len()];
    gpu.blend_bytes(img, &fg, img, &mut dst, BlendMode::SrcOver, false)
        .unwrap();
    assert_eq!(dst, fg, "src over");
}

#[test]
fn vglite_keeps_a_flat_source_flat_under_resize() {
    let Some(gpu) = gpu() else { return };
    let src = Image::packed(Format::Rgba8888, 16, 8);
    let flat: Vec<u8> = [0x40u8, 0x80, 0xC0, 0xFF].repeat(16 * 8);
    let dst = Image::packed(Format::Rgba8888, 8, 4);
    let mut out = vec![0u8; dst.frame_len()];
    gpu.resize_bytes(src, &flat, dst, &mut out, Filter::Bilinear)
        .unwrap();
    assert!(
        out.as_chunks::<4>()
            .0
            .iter()
            .all(|px| *px == [0x40, 0x80, 0xC0, 0xFF])
    );
    let mut out = vec![0u8; dst.frame_len()];
    gpu.resize_bytes(src, &flat, dst, &mut out, Filter::Area)
        .unwrap();
    assert!(
        out.as_chunks::<4>()
            .0
            .iter()
            .all(|px| *px == [0x40, 0x80, 0xC0, 0xFF])
    );
}

/// A horizontal line from (1, 3) to (6, 3) covers exactly pixels 1 to 6 of row 3 (both ends
/// included); nothing else changes. Untested on board until a run on a K230.
#[test]
fn vglite_draws_an_axis_aligned_line_on_exact_pixels() {
    let Some(gpu) = gpu() else { return };
    let img = Image::packed(Format::Rgba8888, 8, 8);
    let mut buf = vec![0u8; img.frame_len()];
    gpu.line_bytes(img, &mut buf, (1, 3), (6, 3), [255; 4])
        .unwrap();
    for y in 0..8u32 {
        for x in 0..8u32 {
            let at = ((y * 8 + x) * 4) as usize;
            let want = if y == 3 && (1..=6).contains(&x) {
                [255; 4]
            } else {
                [0; 4]
            };
            assert_eq!(buf[at..at + 4], want, "pixel {x},{y}");
        }
    }
}

#[test]
fn vglite_refuses_l8_lines() {
    let Some(gpu) = gpu() else { return };
    let img = Image::packed(Format::L8, 8, 8);
    let mut buf = vec![0u8; img.frame_len()];
    let err = gpu
        .line_bytes(img, &mut buf, (0, 0), (7, 7), [255; 4])
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

fn frame(image: Image, buf: &DmaBuf, backing: Backing) -> Frame<'_> {
    Frame {
        image,
        fd: buf.as_fd(),
        backing,
    }
}

/// Dmabufs both ways: the `system` heap (scattered, staged) and a CMA heap when the board has
/// one (contiguous, mapped in place).
#[test]
fn vglite_takes_dmabufs() {
    let Some(gpu) = gpu() else { return };
    let img = Image::packed(Format::Rgba8888, 64, 32);
    let from = pattern(img.frame_len());
    for (heap, backing) in [
        ("system", Backing::Scattered),
        ("linux,cma", Backing::Contiguous),
    ] {
        let Ok(heap) = DmaHeap::open(heap) else {
            eprintln!("no {heap} heap");
            continue;
        };
        let (src, dst) = (
            heap.allocate(img.frame_len()).unwrap(),
            heap.allocate(img.frame_len()).unwrap(),
        );
        let mut map = src.map().unwrap();
        src.begin_cpu_access(Access::Write).unwrap();
        map.as_mut_slice()[..from.len()].copy_from_slice(&from);
        src.end_cpu_access(Access::Write).unwrap();
        let cost = gpu
            .transform_dmabuf(
                frame(img, &src, backing),
                frame(img, &dst, backing),
                Transform::Rotate180,
            )
            .unwrap();
        eprintln!(
            "{} {backing:?}: {} bytes staged",
            heap.name(),
            cost.staged_bytes
        );
        let out = dst.map().unwrap();
        dst.begin_cpu_access(Access::Read).unwrap();
        let got = out.as_slice()[..from.len()].to_vec();
        dst.end_cpu_access(Access::Read).unwrap();
        let want = remap(&from, 64, 4, (64, 32), |x, y| (63 - x, 31 - y));
        assert_eq!(got, want, "{backing:?}");
        assert_eq!(cost.staged_bytes == 0, backing == Backing::Contiguous);
    }
}
