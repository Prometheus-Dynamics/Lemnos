//! Conversions and resizes against a reference written per pixel from the definitions in
//! `convert` and `resize`, sharing none of their row code. Under qemu with V on, the rows run
//! as RVV, so a match is a match with the scalar loops too (`rvv-qemu.sh` in this crate).

use super::*;
use crate::{Filter, Layout, PixelFormat};

const FORMATS: [PixelFormat; 8] = [
    PixelFormat::Rgb24,
    PixelFormat::Bgr24,
    PixelFormat::Abgr32,
    PixelFormat::Xbgr32,
    PixelFormat::Grey,
    PixelFormat::Nv12,
    PixelFormat::Nv21,
    PixelFormat::Yuv420,
];
const MATRICES: [Matrix; 3] = [
    Matrix::Bt601Limited,
    Matrix::Bt601Full,
    Matrix::Bt709Limited,
];

fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (s >> 33) as u8
        })
        .collect()
}

/// A layout with padding past each row (an even stride, as the chroma planes halve it).
fn padded(format: PixelFormat, w: u32, h: u32) -> Layout {
    let row = match format {
        PixelFormat::Rgb24 | PixelFormat::Bgr24 => 3 * w,
        PixelFormat::Abgr32 | PixelFormat::Xbgr32 => 4 * w,
        _ => w.next_multiple_of(2),
    };
    Layout {
        format,
        width: w,
        height: h,
        stride: (row + 6).next_multiple_of(2),
    }
}

fn frame(layout: Layout, seed: u64) -> Vec<u8> {
    noise(layout.format.frame_len(layout.stride, layout.height), seed)
}

/// The reference's view of a pixel: R, G, B and the fourth byte, or Y, U, V.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Px {
    Rgb([i32; 3], Option<u8>),
    Gray(i32),
    Yuv([i32; 3]),
}

fn chroma_at(l: Layout, x: usize, y: usize) -> (usize, usize) {
    let (s, h) = (l.stride as usize, l.height as usize);
    let luma = s * h;
    let (cx, cy) = (x / 2, y / 2);
    match l.format {
        PixelFormat::Nv12 => (luma + cy * s + 2 * cx, luma + cy * s + 2 * cx + 1),
        PixelFormat::Nv21 => (luma + cy * s + 2 * cx + 1, luma + cy * s + 2 * cx),
        _ => {
            let half = s / 2;
            let u = luma + cy * half + cx;
            (u, u + half * h.div_ceil(2))
        }
    }
}

fn get(l: Layout, buf: &[u8], x: usize, y: usize) -> Px {
    let s = l.stride as usize;
    let b = |i: usize| i32::from(buf[i]);
    match l.format {
        PixelFormat::Rgb24 => Px::Rgb(
            [b(y * s + 3 * x), b(y * s + 3 * x + 1), b(y * s + 3 * x + 2)],
            None,
        ),
        PixelFormat::Bgr24 => Px::Rgb(
            [b(y * s + 3 * x + 2), b(y * s + 3 * x + 1), b(y * s + 3 * x)],
            None,
        ),
        PixelFormat::Abgr32 | PixelFormat::Xbgr32 => {
            let o = y * s + 4 * x;
            Px::Rgb([b(o + 2), b(o + 1), b(o)], Some(buf[o + 3]))
        }
        PixelFormat::Grey => Px::Gray(b(y * s + x)),
        _ => {
            let (u, v) = chroma_at(l, x, y);
            Px::Yuv([b(y * s + x), b(u), b(v)])
        }
    }
}

fn round(sum: i32, add: i32) -> i32 {
    (((sum + 512) >> 10) + add).clamp(0, 255)
}

/// The expected byte(s) of pixel (x, y) of `dst` converted from `src`, from the definitions.
fn expect(src: Layout, sb: &[u8], dst: Layout, x: usize, y: usize, m: Matrix) -> Px {
    let k = m.coefficients();
    let limited = k.y_offset != 0;
    let (w, h) = (src.width as usize, src.height as usize);
    let rgb_out = |rgb: [i32; 3]| {
        let fourth = matches!(dst.format, PixelFormat::Abgr32 | PixelFormat::Xbgr32).then_some(255);
        Px::Rgb(rgb, fourth)
    };
    match (get(src, sb, x, y), dst.format) {
        (
            Px::Rgb(rgb, a),
            PixelFormat::Rgb24 | PixelFormat::Bgr24 | PixelFormat::Abgr32 | PixelFormat::Xbgr32,
        ) => {
            let keep = src.format == PixelFormat::Abgr32 && dst.format == PixelFormat::Abgr32;
            match rgb_out(rgb) {
                Px::Rgb(c, Some(_)) if keep => Px::Rgb(c, a),
                p => p,
            }
        }
        (Px::Rgb(c, _), PixelFormat::Grey) => Px::Gray(round(
            k.gray[0] * c[0] + k.gray[1] * c[1] + k.gray[2] * c[2],
            0,
        )),
        (Px::Rgb(c, _), _) => {
            let yv = round(
                k.to_y[0] * c[0] + k.to_y[1] * c[1] + k.to_y[2] * c[2],
                k.y_offset,
            );
            let (xa, xb) = (x / 2 * 2, (x / 2 * 2 + 1).min(w - 1));
            let (ya, yb) = (y / 2 * 2, (y / 2 * 2 + 1).min(h - 1));
            let mut avg = [0; 3];
            for (i, a) in avg.iter_mut().enumerate() {
                let at = |x, y| match get(src, sb, x, y) {
                    Px::Rgb(c, _) => c[i],
                    _ => unreachable!(),
                };
                *a = (at(xa, ya) + at(xb, ya) + at(xa, yb) + at(xb, yb) + 2) >> 2;
            }
            let dot = |r: [i32; 3]| r[0] * avg[0] + r[1] * avg[1] + r[2] * avg[2];
            Px::Yuv([yv, round(dot(k.to_u), 128), round(dot(k.to_v), 128)])
        }
        (Px::Gray(g), PixelFormat::Grey) => Px::Gray(g),
        (
            Px::Gray(g),
            PixelFormat::Rgb24 | PixelFormat::Bgr24 | PixelFormat::Abgr32 | PixelFormat::Xbgr32,
        ) => rgb_out([g; 3]),
        (Px::Gray(g), _) => Px::Yuv([
            if limited {
                16 + (219 * g + 127) / 255
            } else {
                g
            },
            128,
            128,
        ]),
        (Px::Yuv([yv, ..]), PixelFormat::Grey) => Px::Gray(if limited {
            ((yv - 16) * 255 + 109).div_euclid(219).clamp(0, 255)
        } else {
            yv
        }),
        (Px::Yuv([yv, u, v]), PixelFormat::Nv12 | PixelFormat::Nv21 | PixelFormat::Yuv420) => {
            Px::Yuv([yv, u, v])
        }
        (Px::Yuv([yv, u, v]), _) => {
            let (yy, uu, vv) = (k.y * (yv - k.y_offset), u - 128, v - 128);
            rgb_out([
                round(yy + k.vr * vv, 0),
                round(yy + k.ug * uu + k.vg * vv, 0),
                round(yy + k.ub * uu, 0),
            ])
        }
    }
}

#[test]
fn every_conversion_matches_its_definition() {
    for (w, h) in [(1, 1), (2, 2), (5, 3), (16, 2), (33, 5), (130, 3)] {
        for sf in FORMATS {
            let src = padded(sf, w, h);
            let sb = frame(src, u64::from(w * 31 + h));
            for df in FORMATS {
                let dst = padded(df, w, h);
                for m in MATRICES {
                    let mut scratch = Scratch::default();
                    let mut db = vec![0xA5; dst.format.frame_len(dst.stride, h)];
                    convert(src, &sb, dst, &mut db, m, &mut scratch).unwrap();
                    for y in 0..h as usize {
                        for x in 0..w as usize {
                            let want = expect(src, &sb, dst, x, y, m);
                            assert_eq!(
                                get(dst, &db, x, y),
                                want,
                                "{sf:?} to {df:?} {w}x{h} {m:?} at ({x}, {y})"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn black_and_white_land_on_the_range_ends() {
    let src = Layout::packed(PixelFormat::Nv12, 2, 2);
    let mut rgb = [0u8; 12];
    let dst = Layout::packed(PixelFormat::Rgb24, 2, 2);
    for (y, want) in [(16u8, 0u8), (235, 255)] {
        let frame = [y, y, y, y, 128, 128];
        convert(
            src,
            &frame,
            dst,
            &mut rgb,
            Matrix::Bt601Limited,
            &mut Scratch::default(),
        )
        .unwrap();
        assert!(rgb.iter().all(|&b| b == want), "Y {y}: {rgb:?}");
    }
}

#[test]
fn full_range_round_trips_within_two() {
    let w = 64;
    let rgb_l = Layout::packed(PixelFormat::Rgb24, w, 2);
    let yuv_l = Layout::packed(PixelFormat::Yuv420, w, 2);
    // Two equal rows of pairs of equal pixels, so the chroma average loses nothing.
    let mut rgb = noise(3 * w as usize, 9);
    for x in (0..w as usize).step_by(2) {
        let p: [u8; 3] = rgb[3 * x..3 * x + 3].try_into().unwrap();
        rgb[3 * x + 3..3 * x + 6].copy_from_slice(&p);
    }
    rgb.extend_from_within(..);
    let mut yuv = vec![0; yuv_l.frame_len()];
    let mut back = vec![0; rgb.len()];
    convert(
        rgb_l,
        &rgb,
        yuv_l,
        &mut yuv,
        Matrix::Bt601Full,
        &mut Scratch::default(),
    )
    .unwrap();
    convert(
        yuv_l,
        &yuv,
        rgb_l,
        &mut back,
        Matrix::Bt601Full,
        &mut Scratch::default(),
    )
    .unwrap();
    for (a, b) in rgb.iter().zip(&back) {
        assert!(a.abs_diff(*b) <= 2, "{a} came back as {b}");
    }
}

/// The expected bytes of a resize, per plane and channel, from the definitions.
fn expect_resize(
    src: Layout,
    sb: &[u8],
    dst: Layout,
    filter: Filter,
    sampling: Sampling,
) -> Vec<Vec<u8>> {
    fn plane_list(l: Layout) -> Vec<(usize, usize, usize, usize, usize, usize)> {
        // start, stride, width, height, step, channels
        let (w, h, s) = (l.width as usize, l.height as usize, l.stride as usize);
        let luma = s * h;
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        match l.format {
            PixelFormat::Rgb24 | PixelFormat::Bgr24 => vec![(0, s, w, h, 3, 3)],
            PixelFormat::Abgr32 | PixelFormat::Xbgr32 => vec![(0, s, w, h, 4, 4)],
            PixelFormat::Grey => vec![(0, s, w, h, 1, 1)],
            PixelFormat::Nv12 | PixelFormat::Nv21 => {
                vec![(0, s, w, h, 1, 1), (luma, s, cw, ch, 2, 2)]
            }
            _ => vec![
                (0, s, w, h, 1, 1),
                (luma, s / 2, cw, ch, 1, 1),
                (luma + s / 2 * ch, s / 2, cw, ch, 1, 1),
            ],
        }
    }
    let f = (1..=src.width.max(src.height))
        .find(|f| (src.width / f).max(1) == dst.width && (src.height / f).max(1) == dst.height);
    let mut out = Vec::new();
    for (s, d) in plane_list(src).into_iter().zip(plane_list(dst)) {
        let mut bytes = Vec::new();
        let at = |x: usize, y: usize, c: usize| u32::from(sb[s.0 + y * s.1 + x * s.4 + c]);
        for y in 0..d.3 {
            for x in 0..d.2 {
                for c in 0..s.5 {
                    let v = match filter {
                        Filter::Area => {
                            let f = f.unwrap() as usize;
                            let n = (f * f) as u32;
                            let mut sum = 0;
                            for j in 0..f {
                                for i in 0..f {
                                    sum +=
                                        at((x * f + i).min(s.2 - 1), (y * f + j).min(s.3 - 1), c);
                                }
                            }
                            (sum + n / 2) / n
                        }
                        Filter::Bilinear => {
                            let pick = |o: usize, sn: usize, dn: usize| {
                                let p = sampling.position(o, sn, dn);
                                let i0 = (p >> 16) as usize;
                                if i0 + 1 >= sn {
                                    (sn - 1, sn - 1, 0)
                                } else {
                                    (i0, i0 + 1, ((p >> 8) & 255) as u32)
                                }
                            };
                            let (x0, x1, fx) = pick(x, s.2, d.2);
                            let (y0, y1, fy) = pick(y, s.3, d.3);
                            let top = at(x0, y0, c) * (256 - fx) + at(x1, y0, c) * fx;
                            let bot = at(x0, y1, c) * (256 - fx) + at(x1, y1, c) * fx;
                            (top * (256 - fy) + bot * fy + 32768) >> 16
                        }
                    };
                    bytes.push(v as u8);
                }
            }
        }
        out.push(bytes);
    }
    out
}

fn read_planes(l: Layout, buf: &[u8]) -> Vec<Vec<u8>> {
    let (w, h, s) = (l.width as usize, l.height as usize, l.stride as usize);
    let luma = s * h;
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let take = |start: usize, stride: usize, row: usize, rows: usize| -> Vec<u8> {
        (0..rows)
            .flat_map(|y| buf[start + y * stride..][..row].to_vec())
            .collect()
    };
    match l.format {
        PixelFormat::Rgb24 | PixelFormat::Bgr24 => vec![take(0, s, 3 * w, h)],
        PixelFormat::Abgr32 | PixelFormat::Xbgr32 => vec![take(0, s, 4 * w, h)],
        PixelFormat::Grey => vec![take(0, s, w, h)],
        PixelFormat::Nv12 | PixelFormat::Nv21 => vec![take(0, s, w, h), take(luma, s, 2 * cw, ch)],
        _ => vec![
            take(0, s, w, h),
            take(luma, s / 2, cw, ch),
            take(luma + s / 2 * ch, s / 2, cw, ch),
        ],
    }
}

#[test]
fn every_resize_matches_its_definition() {
    let sizes = [
        ((1, 1), (3, 2)),
        ((7, 5), (3, 2)),
        ((40, 9), (20, 4)),
        ((64, 6), (16, 2)),
        ((33, 7), (130, 3)),
        ((130, 4), (33, 9)),
        ((9, 9), (9, 9)),
    ];
    for ((sw, sh), (dw, dh)) in sizes {
        for format in FORMATS {
            let src = padded(format, sw, sh);
            let sb = frame(src, u64::from(sw + 7 * sh));
            let dst = padded(format, dw, dh);
            for (filter, sampling) in [
                (Filter::Area, Sampling::Centres),
                (Filter::Bilinear, Sampling::Centres),
                (Filter::Bilinear, Sampling::Corners),
                (Filter::Bilinear, Sampling::Origin),
            ] {
                let mut db = vec![0xA5; dst.format.frame_len(dst.stride, dh)];
                let result = resize(
                    src,
                    &sb,
                    dst,
                    &mut db,
                    filter,
                    sampling,
                    &mut Scratch::default(),
                );
                let integer =
                    (1..=sw.max(sh)).any(|f| (sw / f).max(1) == dw && (sh / f).max(1) == dh);
                if filter == Filter::Area && !integer {
                    assert!(result.is_err(), "{format:?} area {sw}x{sh} to {dw}x{dh}");
                    continue;
                }
                result.unwrap();
                assert_eq!(
                    read_planes(dst, &db),
                    expect_resize(src, &sb, dst, filter, sampling),
                    "{format:?} {filter:?} {sampling:?} {sw}x{sh} to {dw}x{dh}"
                );
            }
        }
    }
}

#[test]
fn centred_bilinear_at_the_same_size_is_a_copy() {
    let l = Layout::packed(PixelFormat::Rgb24, 17, 3);
    let src = frame(l, 3);
    let mut dst = vec![0; src.len()];
    resize(
        l,
        &src,
        l,
        &mut dst,
        Filter::Bilinear,
        Sampling::Centres,
        &mut Scratch::default(),
    )
    .unwrap();
    assert_eq!(src, dst);
}

#[test]
fn mismatched_and_short_frames_are_refused() {
    let a = Layout::packed(PixelFormat::Rgb24, 4, 4);
    let b = Layout::packed(PixelFormat::Rgb24, 2, 2);
    let src = vec![0; a.frame_len()];
    let mut dst = vec![0; a.frame_len()];
    assert!(
        convert(
            a,
            &src,
            b,
            &mut dst,
            Matrix::Bt601Full,
            &mut Scratch::default()
        )
        .is_err()
    );
    assert!(
        resize(
            a,
            &src,
            Layout::packed(PixelFormat::Grey, 2, 2),
            &mut dst,
            Filter::Area,
            Sampling::Centres,
            &mut Scratch::default()
        )
        .is_err()
    );
    assert!(
        convert(
            a,
            &src[..10],
            a,
            &mut dst,
            Matrix::Bt601Full,
            &mut Scratch::default()
        )
        .is_err()
    );
    assert!(
        resize(
            a,
            &src,
            b,
            &mut dst[..3],
            Filter::Area,
            Sampling::Centres,
            &mut Scratch::default()
        )
        .is_err()
    );
}
