//! The V4L2 route against the kernel's `vim2m` test driver, and against the mainline RGA driver:
//! conversion, scaling, cropping, flips and turns, and dma-buf in and out with buffers from
//! `/dev/dma_heap/system`. Every reference below is a scalar function written here.
//!
//! These open device nodes, so they run only with `LEMNOS_TEST_DEVICES=1` set (and then skip a
//! device that is absent). Without it each test returns before touching `/dev`.
#![cfg(target_os = "linux")]
#![allow(clippy::print_stderr)]

use std::io;
use std::os::fd::{AsFd, OwnedFd};

use lemnos_driver_rga::{Filter, Layout, M2mRga, PixelFormat, Rect, Transform, find_m2m_device};
use lemnos_linux_sys::{dma_heap, mmap::Mapping};

/// Whether the device tests were asked for.
fn devices() -> bool {
    let on = std::env::var_os("LEMNOS_TEST_DEVICES").is_some_and(|v| v == "1");
    if !on {
        eprintln!("skipping: set LEMNOS_TEST_DEVICES=1 to run against device nodes");
    }
    on
}

fn vim2m() -> Option<M2mRga> {
    if !devices() {
        return None;
    }
    match find_m2m_device("vim2m") {
        Some(path) => Some(M2mRga::on_device(&path)),
        None => {
            eprintln!("skipping: no vim2m device (modprobe vim2m)");
            None
        }
    }
}

fn rga() -> Option<M2mRga> {
    if !devices() {
        return None;
    }
    match M2mRga::find() {
        Some(rga) => Some(rga),
        None => {
            eprintln!("skipping: no rockchip-rga device");
            None
        }
    }
}

fn frame(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn swap_rb(rgb: &[u8]) -> Vec<u8> {
    rgb.as_chunks::<3>()
        .0
        .iter()
        .flat_map(|p| [p[2], p[1], p[0]])
        .collect()
}

/// The largest difference between two frames, or an error when the lengths differ.
fn max_diff(got: &[u8], want: &[u8]) -> Result<u8, String> {
    if got.len() != want.len() {
        return Err(format!("{} bytes, want {}", got.len(), want.len()));
    }
    Ok(got
        .iter()
        .zip(want)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0))
}

/// Within one level, printing the measured difference (the tolerance the resizes are held to).
fn within_one(what: &str, got: &[u8], want: &[u8]) -> Result<(), String> {
    let diff = max_diff(got, want)?;
    eprintln!("{what}: max difference {diff}");
    (diff <= 1)
        .then_some(())
        .ok_or_else(|| format!("max difference {diff}"))
}

// Scalar references. `ch` is the bytes per pixel; each is written for the frames the tests use.

fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    rgb.as_chunks::<3>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2], 255])
        .collect()
}

fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect()
}

/// The `rect` (left, top, width, height) of a `w`-pixel frame with `ch` bytes a pixel.
fn crop(src: &[u8], w: usize, ch: usize, rect: (usize, usize, usize, usize)) -> Vec<u8> {
    let (left, top, rw, rh) = rect;
    (0..rh)
        .flat_map(|y| {
            let at = ((top + y) * w + left) * ch;
            src[at..at + rw * ch].to_vec()
        })
        .collect()
}

fn flip(src: &[u8], w: usize, h: usize, ch: usize, horizontal: bool, vertical: bool) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    for y in 0..h {
        for x in 0..w {
            let sx = if horizontal { w - 1 - x } else { x };
            let sy = if vertical { h - 1 - y } else { y };
            let (from, to) = ((sy * w + sx) * ch, (y * w + x) * ch);
            out[to..to + ch].copy_from_slice(&src[from..from + ch]);
        }
    }
    out
}

/// A clockwise turn by `quarters` quarter turns (1, 2 or 3), the output as `(bytes, w, h)`.
fn rotate(src: &[u8], w: usize, h: usize, ch: usize, quarters: u32) -> (Vec<u8>, usize, usize) {
    let (ow, oh) = if quarters % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; ow * oh * ch];
    for oy in 0..oh {
        for ox in 0..ow {
            let (sx, sy) = match quarters {
                1 => (oy, h - 1 - ox),
                2 => (w - 1 - ox, h - 1 - oy),
                _ => (w - 1 - oy, ox),
            };
            let (from, to) = ((sy * w + sx) * ch, (oy * ow + ox) * ch);
            out[to..to + ch].copy_from_slice(&src[from..from + ch]);
        }
    }
    (out, ow, oh)
}

/// An integer area downscale of an `ch`-byte frame: each output the rounded mean of its block.
fn resize_area(src: &[u8], w: usize, h: usize, ch: usize, factor: usize) -> Vec<u8> {
    let (ow, oh) = ((w / factor).max(1), (h / factor).max(1));
    let n = factor * factor;
    let mut out = vec![0u8; ow * oh * ch];
    for oy in 0..oh {
        for ox in 0..ow {
            for c in 0..ch {
                let sum: usize = (0..factor)
                    .flat_map(|dy| (0..factor).map(move |dx| (dy, dx)))
                    .map(|(dy, dx)| {
                        let at = ((oy * factor + dy) * w + ox * factor + dx) * ch + c;
                        src[at] as usize
                    })
                    .sum();
                out[(oy * ow + ox) * ch + c] = ((sum + n / 2) / n) as u8;
            }
        }
    }
    out
}

/// A bilinear upscale with OpenCV's half-pixel centres, rounded.
fn resize_bilinear(src: &[u8], sw: usize, sh: usize, ch: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut out = vec![0u8; dw * dh * ch];
    let at = |x: usize, y: usize, c: usize| src[(y * sw + x) * ch + c] as f32;
    for oy in 0..dh {
        let fy = ((oy as f32 + 0.5) * sh as f32 / dh as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, y1) = (fy.floor() as usize, (fy.floor() as usize + 1).min(sh - 1));
        let wy = fy - y0 as f32;
        for ox in 0..dw {
            let fx = ((ox as f32 + 0.5) * sw as f32 / dw as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
            let (x0, x1) = (fx.floor() as usize, (fx.floor() as usize + 1).min(sw - 1));
            let wx = fx - x0 as f32;
            for c in 0..ch {
                let top = at(x0, y0, c) * (1.0 - wx) + at(x1, y0, c) * wx;
                let bottom = at(x0, y1, c) * (1.0 - wx) + at(x1, y1, c) * wx;
                out[(oy * dw + ox) * ch + c] = (top * (1.0 - wy) + bottom * wy).round() as u8;
            }
        }
    }
    out
}
/// A dma-buf from `/dev/dma_heap/system`, mapped for filling and checking.
struct Buffer {
    dmabuf: OwnedFd,
    len: usize,
}

impl Buffer {
    fn alloc(len: usize) -> io::Result<Self> {
        let heap = std::fs::File::open("/dev/dma_heap/system")?;
        Ok(Self {
            dmabuf: dma_heap::allocate(heap.as_fd(), len as u64)?,
            len,
        })
    }

    fn fill(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut map = Mapping::new(self.dmabuf.as_fd(), self.len, 0, true)?;
        map.as_mut_slice().copy_from_slice(bytes);
        Ok(())
    }

    fn read(&self) -> io::Result<Vec<u8>> {
        Ok(Mapping::new(self.dmabuf.as_fd(), self.len, 0, false)?
            .as_slice()
            .to_vec())
    }
}

fn dma_or_skip(len: usize) -> Option<Buffer> {
    match Buffer::alloc(len) {
        Ok(b) => Some(b),
        Err(e) => {
            eprintln!("skipping: no /dev/dma_heap/system access ({e})");
            None
        }
    }
}

#[test]
fn vim2m_converts_rgb24_to_bgr24() {
    let Some(mut rga) = vim2m() else { return };
    let (src, dst) = (
        Layout::packed(PixelFormat::Rgb24, 640, 480),
        Layout::packed(PixelFormat::Bgr24, 640, 480),
    );
    for seed in 0..3 {
        let input = frame(src.frame_len(), seed);
        let mut out = vec![0u8; dst.frame_len()];
        let n = rga
            .convert(src, &input, None, dst, &mut out)
            .expect("convert");
        assert_eq!(n, dst.frame_len());
        assert_eq!(out, swap_rb(&input), "frame {seed}");
    }
}

/// A crop the device does not take is an error, never a silent full-frame conversion. vim2m has
/// no selection support, so asking it to crop must fail; on a device that crops (the RGA) the
/// same call converts the rectangle.
#[test]
fn vim2m_refuses_a_crop_it_cannot_do() {
    let Some(mut rga) = vim2m() else { return };
    let (src, dst) = (
        Layout::packed(PixelFormat::Rgb24, 640, 480),
        Layout::packed(PixelFormat::Rgb24, 320, 240),
    );
    let crop = Rect {
        left: 16,
        top: 8,
        width: 320,
        height: 240,
    };
    let input = frame(src.frame_len(), 1);
    let mut out = vec![0u8; dst.frame_len()];
    let err = rga
        .convert(src, &input, Some(crop), dst, &mut out)
        .expect_err("vim2m cannot crop");
    eprintln!("refused as expected: {err}");
}

/// Scaling down to the destination's size: vim2m samples the nearest source pixel.
#[test]
fn vim2m_scales_to_the_destination_size() {
    let Some(mut rga) = vim2m() else { return };
    let (src, dst) = (
        Layout::packed(PixelFormat::Rgb24, 640, 480),
        Layout::packed(PixelFormat::Rgb24, 320, 240),
    );
    let input = frame(src.frame_len(), 7);
    let mut out = vec![0u8; dst.frame_len()];
    match rga.convert(src, &input, None, dst, &mut out) {
        Ok(n) => assert_eq!(n, dst.frame_len()),
        Err(err) => return eprintln!("skipping: this vim2m does not scale ({err})"),
    }
    // Every output pixel is some source pixel of its 2x2 block.
    for y in 0..240usize {
        for x in 0..320usize {
            let got = &out[(y * 320 + x) * 3..][..3];
            let found = (0..2).any(|dy| {
                (0..2).any(|dx| {
                    let (sx, sy) = (2 * x + dx, 2 * y + dy);
                    &input[(sy * 640 + sx) * 3..][..3] == got
                })
            });
            assert!(found, "pixel ({x}, {y})");
        }
    }
}

/// A dmabuf in, a dmabuf out: no CPU copy in the conversion.
#[test]
fn vim2m_blits_between_dmabufs() {
    let Some(mut rga) = vim2m() else { return };
    let (src, dst) = (
        Layout::packed(PixelFormat::Rgb24, 640, 480),
        Layout::packed(PixelFormat::Bgr24, 640, 480),
    );
    let (Some(mut a), Some(b)) = (dma_or_skip(src.frame_len()), dma_or_skip(dst.frame_len()))
    else {
        return;
    };
    let input = frame(src.frame_len(), 3);
    a.fill(&input).expect("fill");
    let n = rga
        .blit(src, a.dmabuf.as_fd(), None, dst, b.dmabuf.as_fd())
        .expect("blit");
    assert_eq!(n, dst.frame_len());
    assert_eq!(b.read().expect("read"), swap_rb(&input));
}

/// The mainline RGA driver against the references: the V4L2 route's colour conversions are exact.
#[test]
fn rga_converts_like_the_references() {
    let Some(mut rga) = rga() else { return };
    let (w, h) = (640u32, 480u32);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i % 251) as u8).collect();
    let mut out = vec![0u8; (w * h * 4) as usize];
    rga.rgb_to_rgba(w, h, &rgb, &mut out).expect("rgb_to_rgba");
    assert_eq!(max_diff(&out, &rgb_to_rgba(&rgb)), Ok(0));
    let rgba = rgb_to_rgba(&rgb);
    let mut out = vec![0u8; (w * h * 3) as usize];
    rga.rgba_to_rgb(w, h, &rgba, &mut out).expect("rgba_to_rgb");
    assert_eq!(max_diff(&out, &rgba_to_rgb(&rgba)), Ok(0));
}

/// Resize plumbing on vim2m (it samples nearest, so values are not checked), and the filters
/// the mainline driver cannot honour refused before any device call.
#[test]
fn vim2m_resizes_and_refuses_what_the_driver_cannot_do() {
    let Some(mut rga) = vim2m() else { return };
    let (src, dst) = (
        Layout::packed(PixelFormat::Rgb24, 128, 96),
        Layout::packed(PixelFormat::Rgb24, 64, 48),
    );
    let input = frame(src.frame_len(), 3);
    let mut out = vec![0u8; dst.frame_len()];
    match rga.resize_bytes(src, &input, dst, &mut out, Filter::Area) {
        Ok(n) => assert_eq!(n, dst.frame_len()),
        Err(err) => eprintln!("skipping the area resize: this vim2m does not scale ({err})"),
    }
    let refused = |rga: &mut M2mRga, dst: Layout, filter| {
        let mut out = vec![0u8; dst.frame_len()];
        rga.resize_bytes(src, &input, dst, &mut out, filter)
            .unwrap_err()
            .kind()
    };
    let unsupported = io::ErrorKind::Unsupported;
    assert_eq!(refused(&mut rga, dst, Filter::Bilinear), unsupported);
    let third = Layout::packed(PixelFormat::Rgb24, 60, 48);
    assert_eq!(refused(&mut rga, third, Filter::Area), unsupported);
}

/// The mainline RGA driver resizes against the references (within one level).
#[test]
fn rga_resizes_like_the_references() {
    let Some(mut rga) = rga() else { return };
    let (w, h) = (640u32, 480u32);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i % 253) as u8).collect();
    let (s, d) = (
        Layout::packed(PixelFormat::Rgb24, w, h),
        Layout::packed(PixelFormat::Rgb24, w / 4, h / 4),
    );
    let mut out = vec![0u8; d.frame_len()];
    rga.resize_bytes(s, &rgb, d, &mut out, Filter::Area)
        .expect("area rgb");
    within_one(
        "area rgb 640x480 / 4",
        &out,
        &resize_area(&rgb, w as usize, h as usize, 3, 4),
    )
    .expect("area rgb");
    let (sw, sh) = (160u32, 120u32);
    let rgb: Vec<u8> = (0..sw * sh * 3).map(|i| (i % 253) as u8).collect();
    let (s, d) = (
        Layout::packed(PixelFormat::Rgb24, sw, sh),
        Layout::packed(PixelFormat::Rgb24, sw * 2, sh * 2),
    );
    let mut out = vec![0u8; d.frame_len()];
    rga.resize_bytes(s, &rgb, d, &mut out, Filter::Bilinear)
        .expect("bilinear rgb");
    within_one(
        "bilinear rgb 160x120 x 2",
        &out,
        &resize_bilinear(
            &rgb,
            sw as usize,
            sh as usize,
            3,
            2 * sw as usize,
            2 * sh as usize,
        ),
    )
    .expect("bilinear rgb");
}

/// A full-frame crop is a copy (vim2m copies RGB24 unchanged); a destination that is not the
/// rectangle's size is refused before any device call.
#[test]
fn vim2m_copies_through_a_full_frame_crop() {
    let Some(mut rga) = vim2m() else { return };
    let src = Layout::packed(PixelFormat::Rgb24, 64, 48);
    let input = frame(src.frame_len(), 11);
    let mut out = vec![0u8; src.frame_len()];
    let full = Rect {
        left: 0,
        top: 0,
        width: 64,
        height: 48,
    };
    rga.crop_bytes(src, &input, full, src, &mut out).unwrap();
    assert_eq!(out, input);
    let half = Layout::packed(PixelFormat::Rgb24, 32, 48);
    let err = rga
        .crop_bytes(src, &input, full, half, &mut out)
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

/// The mainline RGA driver crop against the reference.
#[test]
fn rga_crops_like_the_reference() {
    let Some(mut rga) = rga() else { return };
    let (w, h) = (640u32, 480u32);
    let rgb = frame((w * h * 3) as usize, 5);
    let rect = Rect {
        left: 64,
        top: 32,
        width: 320,
        height: 240,
    };
    let (s, d) = (
        Layout::packed(PixelFormat::Rgb24, w, h),
        Layout::packed(PixelFormat::Rgb24, 320, 240),
    );
    let mut out = vec![0u8; d.frame_len()];
    rga.crop_bytes(s, &rgb, rect, d, &mut out).expect("crop");
    assert_eq!(
        max_diff(&out, &crop(&rgb, w as usize, 3, (64, 32, 320, 240))),
        Ok(0)
    );
}

/// Flips through the context's controls on vim2m (it copies RGB24, so the result is exact);
/// vim2m has no rotate control, so a turn is refused as `Unsupported`.
#[test]
fn vim2m_flips_through_the_controls() {
    let Some(mut rga) = vim2m() else { return };
    let (w, h) = (64u32, 48u32);
    let src = Layout::packed(PixelFormat::Rgb24, w, h);
    let input = frame(src.frame_len(), 17);
    for (t, horizontal, vertical) in [
        (Transform::FlipHorizontal, true, false),
        (Transform::FlipVertical, false, true),
    ] {
        let mut out = vec![0u8; src.frame_len()];
        rga.transform_bytes(src, &input, src, &mut out, t).unwrap();
        let want = flip(&input, w as usize, h as usize, 3, horizontal, vertical);
        assert!(out == want, "{t:?}");
    }
    let turned = Layout::packed(PixelFormat::Rgb24, h, w);
    let mut out = vec![0u8; turned.frame_len()];
    match rga.transform_bytes(src, &input, turned, &mut out, Transform::Rotate90) {
        Err(e) if e.kind() == io::ErrorKind::Unsupported => {}
        other => panic!("vim2m has no rotate control, got {other:?}"),
    }
    let err = rga
        .transform_bytes(src, &input, src, &mut out, Transform::Rotate90)
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

/// The mainline RGA driver flips and turns against the references.
#[test]
fn rga_flips_and_turns_like_the_references() {
    let Some(mut rga) = rga() else { return };
    let (w, h) = (320u32, 240u32);
    let src = Layout::packed(PixelFormat::Rgb24, w, h);
    let input = frame(src.frame_len(), 23);
    let (wu, hu) = (w as usize, h as usize);
    for t in [
        Transform::FlipHorizontal,
        Transform::FlipVertical,
        Transform::Rotate90,
        Transform::Rotate180,
        Transform::Rotate270,
    ] {
        let (ow, oh) = t.output_size(w, h);
        let dst = Layout::packed(PixelFormat::Rgb24, ow, oh);
        let mut out = vec![0u8; dst.frame_len()];
        rga.transform_bytes(src, &input, dst, &mut out, t)
            .unwrap_or_else(|e| panic!("{t:?}: {e}"));
        let want = match t {
            Transform::FlipHorizontal => flip(&input, wu, hu, 3, true, false),
            Transform::FlipVertical => flip(&input, wu, hu, 3, false, true),
            Transform::Rotate90 => rotate(&input, wu, hu, 3, 1).0,
            Transform::Rotate180 => rotate(&input, wu, hu, 3, 2).0,
            Transform::Rotate270 => rotate(&input, wu, hu, 3, 3).0,
        };
        assert_eq!(max_diff(&out, &want), Ok(0), "{t:?}");
    }
}
