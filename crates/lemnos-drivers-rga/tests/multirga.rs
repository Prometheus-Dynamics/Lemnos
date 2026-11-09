//! The `/dev/rga` route against scalar references written in this file, one measurement per
//! operation, printed as the largest difference. The tests skip where the node is absent (this
//! host has none). Untested on board: the field mappings are marked as unverified in the crate.
#![cfg(target_os = "linux")]
#![allow(clippy::print_stderr)]

use lemnos_drivers_rga::{BlendMode, Buffer, Channel, Image, MultiRga, RgaFormat, Rop, Transform};

fn device() -> Option<MultiRga> {
    match MultiRga::find() {
        Some(rga) => Some(rga),
        None => {
            eprintln!("skipping: no /dev/rga node");
            None
        }
    }
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

/// The largest difference between two buffers, printed under `what`.
fn measure(what: &str, got: &[u8], want: &[u8]) -> Result<u8, String> {
    if got.len() != want.len() {
        return Err(format!("{what}: {} bytes, want {}", got.len(), want.len()));
    }
    let diff = got
        .iter()
        .zip(want)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    eprintln!("{what}: max difference {diff}");
    Ok(diff)
}

/// Solid fill of `rect` in RGBA8888 (the reference).
fn fill_ref(base: &[u8], w: usize, rect: (usize, usize, usize, usize), rgba: [u8; 4]) -> Vec<u8> {
    let mut out = base.to_vec();
    let (l, t, rw, rh) = rect;
    for y in t..t + rh {
        for x in l..l + rw {
            out[(y * w + x) * 4..][..4].copy_from_slice(&rgba);
        }
    }
    out
}

/// `dst = src op dst` per byte (the reference for the mask ROP).
fn rop_ref(src: &[u8], dst: &[u8], op: Rop) -> Vec<u8> {
    src.iter()
        .zip(dst)
        .map(|(a, b)| match op {
            Rop::And => a & b,
            Rop::Or => a | b,
            Rop::Xor => a ^ b,
        })
        .collect()
}

/// Porter-Duff of straight-alpha RGBA `fg` onto `bg` (the reference for the blend).
fn blend_ref(fg: &[u8], bg: &[u8], mode: BlendMode) -> Vec<u8> {
    fg.as_chunks::<4>()
        .0
        .iter()
        .zip(bg.as_chunks::<4>().0)
        .flat_map(|(f, b)| {
            let (fa, ba) = (f[3] as f32 / 255.0, b[3] as f32 / 255.0);
            let (fo, bo) = match mode {
                BlendMode::SrcOver => (1.0, 1.0 - fa),
                BlendMode::Src => (1.0, 0.0),
                BlendMode::DstOver => (1.0 - ba, 1.0),
            };
            let alpha = fa * fo + ba * bo;
            let mut px = [0u8; 4];
            for c in 0..3 {
                let v = if alpha > 0.0 {
                    (f[c] as f32 * fa * fo + b[c] as f32 * ba * bo) / alpha
                } else {
                    0.0
                };
                px[c] = v.round() as u8;
            }
            px[3] = (alpha * 255.0).round() as u8;
            px
        })
        .collect()
}

/// Every operation's measured difference is within one level of the reference.
fn within_one(what: &str, got: &[u8], want: &[u8]) -> Result<(), String> {
    let diff = measure(what, got, want)?;
    (diff <= 1)
        .then_some(())
        .ok_or_else(|| format!("{what}: max difference {diff}"))
}

#[test]
fn fill_matches_the_reference() {
    let Some(rga) = device() else { return };
    let (w, h) = (64u32, 32u32);
    let image = Image::packed(RgaFormat::Rgba8888, w, h);
    let base = pattern(image.frame_len(), 9);
    let mut out = base.clone();
    rga.fill(image, Buffer::Write(&mut out), (5, 3, 40, 20), [1, 2, 3, 4])
        .expect("fill");
    within_one(
        "fill rgba 40x20 at (5, 3)",
        &out,
        &fill_ref(&base, w as usize, (5, 3, 40, 20), [1, 2, 3, 4]),
    )
    .expect("fill");
}

#[test]
fn mask_rop_matches_the_reference() {
    let Some(rga) = device() else { return };
    let (w, h) = (64u32, 16u32);
    let mask = Image::packed(RgaFormat::Rgba8888, w / 4, h);
    let (src, base) = (pattern(mask.frame_len(), 1), pattern(mask.frame_len(), 2));
    for op in [Rop::And, Rop::Or, Rop::Xor] {
        let mut dst = base.clone();
        rga.rop(w, h, Buffer::Read(&src), Buffer::Write(&mut dst), op)
            .expect("rop");
        within_one(&format!("mask rop {op:?}"), &dst, &rop_ref(&src, &base, op)).expect("rop");
    }
}

#[test]
fn blend_matches_the_reference() {
    let Some(rga) = device() else { return };
    let (w, h) = (32u32, 16u32);
    let image = Image::packed(RgaFormat::Rgba8888, w, h);
    let (fg, bg) = (pattern(image.frame_len(), 5), pattern(image.frame_len(), 6));
    for mode in [BlendMode::SrcOver, BlendMode::Src, BlendMode::DstOver] {
        let mut dst = bg.clone();
        rga.blend(
            image,
            Buffer::Read(&fg),
            Buffer::Write(&mut dst),
            mode,
            false,
        )
        .expect("blend");
        within_one(&format!("blend {mode:?}"), &dst, &blend_ref(&fg, &bg, mode)).expect("blend");
    }
}

#[test]
fn palette_matches_the_reference() {
    let Some(rga) = device() else { return };
    let (w, h) = (16u32, 8u32);
    let indices = pattern((w * h) as usize, 3);
    let mut table = [[0u8; 4]; 256];
    for (i, e) in table.iter_mut().enumerate() {
        *e = [i as u8, (i * 7) as u8, 255 - i as u8, 255];
    }
    let mut out = vec![0u8; (w * h * 4) as usize];
    rga.palette(
        w,
        h,
        Buffer::Read(&indices),
        &table,
        Buffer::Write(&mut out),
    )
    .expect("palette");
    let want: Vec<u8> = indices.iter().flat_map(|&i| table[i as usize]).collect();
    within_one("palette 8bpp", &out, &want).expect("palette");
}

/// The block's alpha is the mask: 255 where every colour channel lies in the range, 0 elsewhere,
/// and the colour of a pixel inside the range is the source's.
#[test]
fn color_key_matches_the_reference() {
    let Some(rga) = device() else { return };
    let (w, h) = (32u32, 16u32);
    let (min, max) = ([40, 40, 40], [200, 200, 200]);
    let src = pattern((w * h * 4) as usize, 9);
    let mut out = vec![0u8; (w * h * 4) as usize];
    rga.color_key(w, h, Buffer::Read(&src), min, max, Buffer::Write(&mut out))
        .expect("color key");
    let inside = |p: &[u8; 4]| (0..3).all(|c| min[c] <= p[c] && p[c] <= max[c]);
    let source = src.as_chunks::<4>().0;
    let got = out.as_chunks::<4>().0;
    assert!(source.iter().any(inside) && !source.iter().all(inside));
    let want: Vec<u8> = source
        .iter()
        .map(|p| if inside(p) { 255 } else { 0 })
        .collect();
    let mask: Vec<u8> = got.iter().map(|p| p[3]).collect();
    let diff = measure("color key mask", &mask, &want).expect("mask");
    assert_eq!(diff, 0, "the mask must be exact");
    for (g, s) in got.iter().zip(source).filter(|(_, s)| inside(s)) {
        within_one("color key colour", &g[..3], &s[..3]).expect("colour");
    }
}

#[test]
fn gray_to_rgba_matches_the_reference() {
    let Some(rga) = device() else { return };
    let (w, h) = (32u32, 16u32);
    let gray = pattern((w * h) as usize, 4);
    let mut out = vec![0u8; (w * h * 4) as usize];
    rga.gray_to_rgba(w, h, Buffer::Read(&gray), Buffer::Write(&mut out))
        .expect("gray");
    let want: Vec<u8> = gray.iter().flat_map(|&g| [g, g, g, 255]).collect();
    within_one("gray to rgba", &out, &want).expect("gray");
}

/// Bit-exact, every byte value of each channel: 256 pixels a row, the channel counting up, the
/// other bytes from a pattern. A caps row for this operation waits on this test passing.
#[test]
fn a_channel_to_gray_is_exact_for_every_byte() {
    let Some(rga) = device() else { return };
    let (w, h) = (256u32, 4u32);
    for (channel, c) in [(Channel::Red, 0), (Channel::Green, 1), (Channel::Blue, 2)] {
        let mut src = pattern((w * h * 4) as usize, 13 + c as u8);
        for (i, px) in src.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            px[c] = (i % 256) as u8;
        }
        let mut out = vec![0u8; (w * h) as usize];
        rga.channel_to_gray(w, h, channel, Buffer::Read(&src), Buffer::Write(&mut out))
            .expect("channel to gray");
        let want: Vec<u8> = src.as_chunks::<4>().0.iter().map(|px| px[c]).collect();
        let diff = measure(&format!("{channel:?} to gray"), &out, &want).expect("channel");
        assert_eq!(diff, 0, "{channel:?} must be exact");
    }
}

#[test]
fn flips_and_turns_match_the_reference() {
    let Some(rga) = device() else { return };
    let (w, h) = (16usize, 8usize);
    let src_image = Image::packed(RgaFormat::Rgba8888, w as u32, h as u32);
    let input = pattern(src_image.frame_len(), 8);
    for t in [
        Transform::FlipHorizontal,
        Transform::FlipVertical,
        Transform::Rotate90,
        Transform::Rotate180,
        Transform::Rotate270,
    ] {
        let (ow, oh) = t.output_size(w as u32, h as u32);
        let dst_image = Image::packed(RgaFormat::Rgba8888, ow, oh);
        let mut out = vec![0u8; dst_image.frame_len()];
        rga.transform(
            src_image,
            Buffer::Read(&input),
            dst_image,
            Buffer::Write(&mut out),
            t,
        )
        .unwrap_or_else(|e| panic!("{t:?}: {e}"));
        let want = reference_turn(&input, w, h, t);
        within_one(&format!("{t:?}"), &out, &want).expect("turn");
    }
}

/// The reference geometry of a flip or clockwise turn of RGBA8888.
fn reference_turn(src: &[u8], w: usize, h: usize, t: Transform) -> Vec<u8> {
    let (ow, oh) = t.output_size(w as u32, h as u32);
    let (ow, oh) = (ow as usize, oh as usize);
    let mut out = vec![0u8; ow * oh * 4];
    for oy in 0..oh {
        for ox in 0..ow {
            let (sx, sy) = match t {
                Transform::FlipHorizontal => (w - 1 - ox, oy),
                Transform::FlipVertical => (ox, h - 1 - oy),
                Transform::Rotate90 => (oy, h - 1 - ox),
                Transform::Rotate180 => (w - 1 - ox, h - 1 - oy),
                Transform::Rotate270 => (w - 1 - oy, ox),
            };
            let (from, to) = ((sy * w + sx) * 4, (oy * ow + ox) * 4);
            out[to..to + 4].copy_from_slice(&src[from..from + 4]);
        }
    }
    out
}
