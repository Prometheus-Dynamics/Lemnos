//! Resizes on the CPU, within one [`PixelFormat`]: each plane on its own (a 4:2:0 frame's
//! chroma at its own half size), each byte of a pixel as its own channel.
//!
//! The definition, which the scalar loops and the RVV rows both follow to the byte:
//! - **Area** ([`Filter::Area`]): an integer factor `f`, the same rule as the units' (the output
//!   is `max(1, src / f)` per axis). Each output byte is the mean of its `f` by `f` block, as
//!   `(sum + n / 2) / n`; a block past the plane's edge repeats the last column or row (only a
//!   chroma plane of odd size meets one).
//! - **Bilinear** ([`Filter::Bilinear`]), with the source position of output `x` set by
//!   [`Sampling`] in 16.16 fixed point, and 8-bit weights: `x0 = p >> 16`,
//!   `fx = (p >> 8) & 255`, `x1 = x0 + 1`, both held at the last column (and `fx` at 0 there).
//!   The output is `((a (256 - fx) + b fx) (256 - fy) + (c (256 - fx) + d fx) fy + 32768) >> 16`
//!   for the four neighbours `a b` above `c d`.
//!
//! The units' own phase and rounding are unmeasured (see [`crate::caps`]); [`Sampling`] names
//! the phase each unit is documented to use.

use std::io;

use super::convert::{Kind, check, chroma, kind};
use crate::{Filter, Layout};

/// Where bilinear output `x` samples the source, for a source of `s` and output of `d` pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sampling {
    /// Pixel centres: `(x + 0.5) s / d - 0.5`, held at 0 (OpenCV's `INTER_LINEAR`, and the
    /// K230D's VGLite route).
    Centres,
    /// Corners on corners: `x (s - 1) / (d - 1)` (the mainline RGA2 driver's factor).
    Corners,
    /// From the origin: `x s / d` (the mainline RGA3 driver's factor).
    Origin,
}

impl Sampling {
    /// The source position of output `x`, 16.16 fixed point.
    pub(super) fn position(self, x: usize, s: usize, d: usize) -> i64 {
        let (x, s, d) = (x as i64, s as i64, d as i64);
        match self {
            Sampling::Centres => ((((2 * x + 1) * s) << 16) / (2 * d) - (1 << 15)).max(0),
            Sampling::Corners if d == 1 => 0,
            Sampling::Corners => ((x * (s - 1)) << 16) / (d - 1),
            Sampling::Origin => ((x * s) << 16) / d,
        }
    }
}

/// One axis of a bilinear resize: each output's two source indices and the weight of the second.
pub(super) fn axis(sampling: Sampling, s: usize, d: usize) -> Vec<(usize, usize, u32)> {
    (0..d)
        .map(|x| {
            let p = sampling.position(x, s, d);
            let x0 = (p >> 16) as usize;
            if x0 + 1 >= s {
                (s - 1, s - 1, 0)
            } else {
                (x0, x0 + 1, ((p >> 8) & 255) as u32)
            }
        })
        .collect()
}

/// A plane of a frame: where it starts, its stride and size, and its pixels' bytes.
#[derive(Clone, Copy, Debug)]
pub(super) struct Plane {
    pub start: usize,
    pub stride: usize,
    pub width: usize,
    pub height: usize,
    /// Bytes per pixel; each is a channel of its own.
    pub step: usize,
}

fn planes(layout: Layout) -> Vec<Plane> {
    let (w, h, stride) = (
        layout.width as usize,
        layout.height as usize,
        layout.stride as usize,
    );
    let (cw, chh) = (w.div_ceil(2), h.div_ceil(2));
    let plane = |start, stride, width, height, step| Plane {
        start,
        stride,
        width,
        height,
        step,
    };
    match kind(layout.format) {
        Kind::Packed { bpp, .. } => vec![plane(0, stride, w, h, bpp)],
        Kind::Gray => vec![plane(0, stride, w, h, 1)],
        Kind::Yuv420 {
            interleaved,
            u_first,
        } => {
            let c = chroma(layout, interleaved, u_first);
            let luma = plane(0, stride, w, h, 1);
            if interleaved {
                vec![luma, plane(c.u.min(c.v), c.stride, cw, chh, 2)]
            } else {
                vec![
                    luma,
                    plane(c.u, c.stride, cw, chh, 1),
                    plane(c.v, c.stride, cw, chh, 1),
                ]
            }
        }
    }
}

/// Resizes `src` into `dst`, of the same format, with `filter`. `sampling` is the bilinear
/// phase; an area resize ignores it.
pub fn resize(
    src: Layout,
    src_buf: &[u8],
    dst: Layout,
    dst_buf: &mut [u8],
    filter: Filter,
    sampling: Sampling,
) -> io::Result<()> {
    if src.format != dst.format {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "resize keeps the format ({:?} to {:?}); convert first or after",
                src.format, dst.format
            ),
        ));
    }
    if src.width == 0 || src.height == 0 || dst.width == 0 || dst.height == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resize of an empty frame",
        ));
    }
    check(src, src_buf.len())?;
    check(dst, dst_buf.len())?;
    let pairs = planes(src).into_iter().zip(planes(dst));
    match filter {
        Filter::Area => {
            let (sw, sh, dw, dh) = (src.width, src.height, dst.width, dst.height);
            let Some(f) = (1..=sw.max(sh)).find(|f| (sw / f).max(1) == dw && (sh / f).max(1) == dh)
            else {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("area resize of {sw}x{sh} to {dw}x{dh} is not an integer downscale"),
                ));
            };
            for (s, d) in pairs {
                area_plane(src_buf, s, dst_buf, d, f as usize);
            }
        }
        Filter::Bilinear => {
            for (s, d) in pairs {
                bilinear_plane(src_buf, s, dst_buf, d, sampling);
            }
        }
    }
    Ok(())
}

fn area_plane(src: &[u8], s: Plane, dst: &mut [u8], d: Plane, f: usize) {
    let n = (f * f) as u32;
    #[cfg(all(target_os = "linux", target_arch = "riscv64"))]
    let mut acc = vec![0u32; d.width];
    for y in 0..d.height {
        let out = &mut dst[d.start + y * d.stride..][..d.width * d.step];
        for c in 0..s.step {
            // Outputs whose block lies inside the row: vector rows where they run.
            #[cfg_attr(
                not(all(target_os = "linux", target_arch = "riscv64")),
                allow(unused_mut)
            )]
            let mut done = 0;
            #[cfg(all(target_os = "linux", target_arch = "riscv64"))]
            {
                use lemnos_linux_sys::rvv::{Strided, accumulate, divide};
                let full = (s.width / f).min(d.width);
                acc[..full].fill(0);
                let mut ok = true;
                for j in 0..f {
                    let row = &src[s.start + (y * f + j).min(s.height - 1) * s.stride..]
                        [..s.width * s.step];
                    for i in 0..f {
                        ok &= accumulate(
                            &mut acc[..full],
                            Strided::new(row, i * s.step + c, f * s.step),
                        );
                    }
                }
                if ok && divide(&acc[..full], n, Strided::new(out, c, d.step)) {
                    done = full;
                }
            }
            for x in done..d.width {
                let mut sum = 0u32;
                for j in 0..f {
                    let row = s.start + (y * f + j).min(s.height - 1) * s.stride;
                    for i in 0..f {
                        let col = (x * f + i).min(s.width - 1);
                        sum += u32::from(src[row + col * s.step + c]);
                    }
                }
                out[x * d.step + c] = ((sum + n / 2) / n) as u8;
            }
        }
    }
}

fn bilinear_plane(src: &[u8], s: Plane, dst: &mut [u8], d: Plane, sampling: Sampling) {
    let xs = axis(sampling, s.width, d.width);
    let ys = axis(sampling, s.height, d.height);
    #[cfg(all(target_os = "linux", target_arch = "riscv64"))]
    let table = lemnos_linux_sys::rvv::BilinearTable::new(
        xs.iter()
            .map(|&(x0, x1, fx)| ((x0 * s.step) as u32, (x1 * s.step) as u32, fx)),
    );
    for (y, &(y0, y1, fy)) in ys.iter().enumerate() {
        let r0 = &src[s.start + y0 * s.stride..][..s.width * s.step];
        let r1 = &src[s.start + y1 * s.stride..][..s.width * s.step];
        let out = &mut dst[d.start + y * d.stride..][..d.width * d.step];
        for c in 0..s.step {
            #[cfg(all(target_os = "linux", target_arch = "riscv64"))]
            {
                use lemnos_linux_sys::rvv::{Strided, bilinear};
                if bilinear(&table, &r0[c..], &r1[c..], fy, Strided::new(out, c, d.step)) {
                    continue;
                }
            }
            for (x, &(x0, x1, fx)) in xs.iter().enumerate() {
                let at = |r: &[u8], i: usize| u32::from(r[i * s.step + c]);
                let top = at(r0, x0) * (256 - fx) + at(r0, x1) * fx;
                let bottom = at(r1, x0) * (256 - fx) + at(r1, x1) * fx;
                out[x * d.step + c] = ((top * (256 - fy) + bottom * fy + 32768) >> 16) as u8;
            }
        }
    }
}
