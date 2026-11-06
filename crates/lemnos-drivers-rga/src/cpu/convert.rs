//! Pixel-format conversion on the CPU, between any two [`PixelFormat`]s of one size.
//!
//! The definition, which the scalar loops and the RVV rows both follow to the byte:
//! - **Matrices** ([`Matrix`]): the ones the units offer. The mainline RGA2 driver's default is
//!   BT.601 limited range (`RGA_SRC_CSC_MODE_BT601_R0`), BT.709 limited for a Rec. 709 colour
//!   space; RGA3 adds BT.601 full range; the multi-RGA gray input is BT.601 full range.
//!   Coefficients are in 1/1024ths, rounded to nearest, as librga scales the RGA's own (each row
//!   of the RGB to YUV matrix sums to its exact total: 1024 or 879 for luma, 0 for chroma).
//! - **Rounding**: `(sum + 512) >> 10`, an arithmetic shift, then a clamp to 0..=255.
//! - **4:2:0 chroma**: from YUV, each pixel takes the chroma sample of its 2x2 block (nearest);
//!   to YUV, the block's R, G and B are each averaged as `(a + b + c + d + 2) >> 2` (a block cut
//!   by an odd edge repeats its last column or row), then converted.
//! - **Gray** is full-range luma: RGB to gray takes the matrix's luma weights without the
//!   limited range's scale; gray to RGB repeats the byte. Gray to and from limited-range Y maps
//!   0..=255 onto 16..=235 as `16 + (219 g + 127) / 255` and back as
//!   `((y - 16) 255 + 109) / 219`, clamped; YUV made from gray has neutral chroma (128).
//! - **Alpha**: a four-byte destination's A or X byte is 255, except from `Abgr32` to `Abgr32`,
//!   which keeps the source's alpha.
//!
//! The units' own coefficient precision and rounding are unmeasured (see [`crate::caps`]), so
//! how close this is to a unit's output is not known yet.

use std::io;

use crate::{Layout, PixelFormat};

/// The YCbCr matrix and range of a conversion between RGB and YUV.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Matrix {
    /// BT.601, Y 16..=235 and chroma 16..=240 (the mainline RGA2 driver's default).
    Bt601Limited,
    /// BT.601, full range (JPEG).
    Bt601Full,
    /// BT.709, limited range.
    Bt709Limited,
}

/// One matrix's coefficients in 1/1024ths.
#[derive(Clone, Copy, Debug)]
pub(super) struct Coefficients {
    /// YUV to RGB: the luma scale, and V into R, U into G, V into G (negative), U into B.
    pub y: i32,
    pub vr: i32,
    pub ug: i32,
    pub vg: i32,
    pub ub: i32,
    /// RGB to YUV: rows for Y, U and V, each R, G, B.
    pub to_y: [i32; 3],
    pub to_u: [i32; 3],
    pub to_v: [i32; 3],
    /// Luma weights for gray (full range).
    pub gray: [i32; 3],
    /// The black level of Y: 16 limited, 0 full.
    pub y_offset: i32,
}

impl Matrix {
    pub(super) const fn coefficients(self) -> Coefficients {
        match self {
            Matrix::Bt601Limited => Coefficients {
                y: 1192,
                vr: 1634,
                ug: -401,
                vg: -832,
                ub: 2066,
                to_y: [263, 516, 100],
                to_u: [-152, -298, 450],
                to_v: [450, -377, -73],
                gray: [306, 601, 117],
                y_offset: 16,
            },
            Matrix::Bt601Full => Coefficients {
                y: 1024,
                vr: 1436,
                ug: -352,
                vg: -731,
                ub: 1815,
                to_y: [306, 601, 117],
                to_u: [-173, -339, 512],
                to_v: [512, -429, -83],
                gray: [306, 601, 117],
                y_offset: 0,
            },
            Matrix::Bt709Limited => Coefficients {
                y: 1192,
                vr: 1836,
                ug: -218,
                vg: -546,
                ub: 2163,
                to_y: [187, 629, 63],
                to_u: [-103, -347, 450],
                to_v: [450, -409, -41],
                gray: [218, 732, 74],
                y_offset: 16,
            },
        }
    }

    fn limited(self) -> bool {
        self.coefficients().y_offset != 0
    }
}

/// `(sum + 512) >> 10 + add`, clamped to a byte: every product sum here ends this way.
pub(super) fn mix(sum: i32, add: i32) -> u8 {
    (((sum + 512) >> 10) + add).clamp(0, 255) as u8
}

/// How a format lays out its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// Bytes per pixel, the offsets of R, G and B, and of the fourth byte.
    Packed {
        bpp: usize,
        rgb: [usize; 3],
        fourth: Option<usize>,
    },
    Gray,
    /// 4:2:0: chroma interleaved (`NV12`, `NV21`) or in two planes (`YU12`), U first or not.
    Yuv420 {
        interleaved: bool,
        u_first: bool,
    },
}

pub(super) fn kind(format: PixelFormat) -> Kind {
    match format {
        PixelFormat::Rgb24 => Kind::Packed {
            bpp: 3,
            rgb: [0, 1, 2],
            fourth: None,
        },
        PixelFormat::Bgr24 => Kind::Packed {
            bpp: 3,
            rgb: [2, 1, 0],
            fourth: None,
        },
        PixelFormat::Abgr32 | PixelFormat::Xbgr32 => Kind::Packed {
            bpp: 4,
            rgb: [2, 1, 0],
            fourth: Some(3),
        },
        PixelFormat::Grey => Kind::Gray,
        PixelFormat::Nv12 => Kind::Yuv420 {
            interleaved: true,
            u_first: true,
        },
        PixelFormat::Nv21 => Kind::Yuv420 {
            interleaved: true,
            u_first: false,
        },
        PixelFormat::Yuv420 => Kind::Yuv420 {
            interleaved: false,
            u_first: true,
        },
    }
}

/// Where a 4:2:0 frame's chroma is: each sample's U and V byte offsets for chroma column `cx`
/// of chroma row `cy` are `u + cy * stride + cx * step` and the same from `v`.
#[derive(Clone, Copy, Debug)]
pub(super) struct Chroma {
    pub u: usize,
    pub v: usize,
    pub stride: usize,
    pub step: usize,
}

pub(super) fn chroma(layout: Layout, interleaved: bool, u_first: bool) -> Chroma {
    let stride = layout.stride as usize;
    let luma = stride * layout.height as usize;
    if interleaved {
        let (u, v) = if u_first {
            (luma, luma + 1)
        } else {
            (luma + 1, luma)
        };
        Chroma {
            u,
            v,
            stride,
            step: 2,
        }
    } else {
        let half = stride / 2;
        let rows = layout.height.div_ceil(2) as usize;
        Chroma {
            u: luma,
            v: luma + half * rows,
            stride: half,
            step: 1,
        }
    }
}

/// Checks a layout against its buffer: a stride that holds a row, and the frame's bytes.
pub(super) fn check(layout: Layout, len: usize) -> io::Result<()> {
    let w = layout.width as usize;
    let row = match kind(layout.format) {
        Kind::Packed { bpp, .. } => bpp * w,
        Kind::Gray => w,
        Kind::Yuv420 { interleaved, .. } => {
            if interleaved {
                w.next_multiple_of(2)
            } else {
                // Each chroma plane's row is half the stride.
                2 * w.div_ceil(2)
            }
        }
    };
    let need = layout.format.frame_len(layout.stride, layout.height);
    if (layout.stride as usize) < row || len < need {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{:?} {}x{} at stride {} needs rows of {row} bytes and {need} bytes in all, has {len}",
                layout.format, layout.width, layout.height, layout.stride
            ),
        ));
    }
    Ok(())
}

/// Converts `src` into `dst`, which has the same width and height, with `matrix` where RGB
/// and YUV meet. A resize is [`super::resize`]'s, first or after.
pub fn convert(
    src: Layout,
    src_buf: &[u8],
    dst: Layout,
    dst_buf: &mut [u8],
    matrix: Matrix,
) -> io::Result<()> {
    if (src.width, src.height) != (dst.width, dst.height) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "convert keeps the size ({}x{} to {}x{}); resize first or after",
                src.width, src.height, dst.width, dst.height
            ),
        ));
    }
    check(src, src_buf.len())?;
    check(dst, dst_buf.len())?;
    let (w, h) = (src.width as usize, src.height as usize);
    let (ss, ds) = (src.stride as usize, dst.stride as usize);
    let k = matrix.coefficients();
    match (kind(src.format), kind(dst.format)) {
        (
            Kind::Packed {
                bpp: sb,
                rgb: sc,
                fourth: sf,
            },
            Kind::Packed {
                bpp: db,
                rgb: dc,
                fourth: df,
            },
        ) => {
            let keep_alpha = src.format == PixelFormat::Abgr32 && dst.format == PixelFormat::Abgr32;
            for y in 0..h {
                let s = &src_buf[y * ss..][..sb * w];
                let d = &mut dst_buf[y * ds..][..db * w];
                for (p, q) in s.chunks_exact(sb).zip(d.chunks_exact_mut(db)) {
                    for c in 0..3 {
                        q[dc[c]] = p[sc[c]];
                    }
                    if let Some(f) = df {
                        q[f] = match (keep_alpha, sf) {
                            (true, Some(a)) => p[a],
                            _ => 255,
                        };
                    }
                }
            }
        }
        (Kind::Packed { bpp, rgb, .. }, Kind::Gray) => {
            for y in 0..h {
                let s = &src_buf[y * ss..][..bpp * w];
                let d = &mut dst_buf[y * ds..][..w];
                super::rows::mix3_packed(s, bpp, rgb, k.gray, 0, d);
            }
        }
        (Kind::Gray, Kind::Packed { bpp, rgb, fourth }) => {
            for y in 0..h {
                let s = &src_buf[y * ss..][..w];
                let d = &mut dst_buf[y * ds..][..bpp * w];
                for (&g, q) in s.iter().zip(d.chunks_exact_mut(bpp)) {
                    for c in rgb {
                        q[c] = g;
                    }
                    if let Some(f) = fourth {
                        q[f] = 255;
                    }
                }
            }
        }
        (Kind::Gray, Kind::Gray) => copy_plane(src_buf, ss, dst_buf, ds, w, h),
        (
            Kind::Packed { bpp, rgb, .. },
            Kind::Yuv420 {
                interleaved,
                u_first,
            },
        ) => {
            let ch = chroma(dst, interleaved, u_first);
            for y in 0..h {
                let s = &src_buf[y * ss..][..bpp * w];
                let d = &mut dst_buf[y * ds..][..w];
                super::rows::mix3_packed(s, bpp, rgb, k.to_y, k.y_offset, d);
            }
            for cy in 0..h.div_ceil(2) {
                let r0 = &src_buf[2 * cy * ss..][..bpp * w];
                let r1 = &src_buf[(2 * cy + 1).min(h - 1) * ss..][..bpp * w];
                super::rows::rgb_to_chroma(r0, r1, bpp, rgb, &k, dst_buf, ch, cy, w);
            }
        }
        (
            Kind::Yuv420 {
                interleaved,
                u_first,
            },
            Kind::Packed { bpp, rgb, fourth },
        ) => {
            let ch = chroma(src, interleaved, u_first);
            for y in 0..h {
                let row = &mut dst_buf[y * ds..][..bpp * w];
                super::rows::yuv_to_rgb(src_buf, y * ss, ch, y / 2, &k, row, bpp, rgb, fourth, w);
            }
        }
        (
            Kind::Gray,
            Kind::Yuv420 {
                interleaved,
                u_first,
            },
        ) => {
            for y in 0..h {
                let s = &src_buf[y * ss..][..w];
                let d = &mut dst_buf[y * ds..][..w];
                for (d, &g) in d.iter_mut().zip(s) {
                    *d = if matrix.limited() {
                        gray_to_limited(g)
                    } else {
                        g
                    };
                }
            }
            let ch = chroma(dst, interleaved, u_first);
            for cy in 0..h.div_ceil(2) {
                for cx in 0..w.div_ceil(2) {
                    dst_buf[ch.u + cy * ch.stride + cx * ch.step] = 128;
                    dst_buf[ch.v + cy * ch.stride + cx * ch.step] = 128;
                }
            }
        }
        (Kind::Yuv420 { .. }, Kind::Gray) => {
            for y in 0..h {
                let s = &src_buf[y * ss..][..w];
                let d = &mut dst_buf[y * ds..][..w];
                for (d, &v) in d.iter_mut().zip(s) {
                    *d = if matrix.limited() {
                        limited_to_gray(v)
                    } else {
                        v
                    };
                }
            }
        }
        (
            Kind::Yuv420 {
                interleaved: si,
                u_first: su,
            },
            Kind::Yuv420 {
                interleaved: di,
                u_first: du,
            },
        ) => {
            copy_plane(src_buf, ss, dst_buf, ds, w, h);
            let (a, b) = (chroma(src, si, su), chroma(dst, di, du));
            for cy in 0..h.div_ceil(2) {
                for cx in 0..w.div_ceil(2) {
                    dst_buf[b.u + cy * b.stride + cx * b.step] =
                        src_buf[a.u + cy * a.stride + cx * a.step];
                    dst_buf[b.v + cy * b.stride + cx * b.step] =
                        src_buf[a.v + cy * a.stride + cx * a.step];
                }
            }
        }
    }
    Ok(())
}

fn copy_plane(src: &[u8], ss: usize, dst: &mut [u8], ds: usize, w: usize, h: usize) {
    for y in 0..h {
        dst[y * ds..][..w].copy_from_slice(&src[y * ss..][..w]);
    }
}

fn gray_to_limited(g: u8) -> u8 {
    (16 + (219 * u32::from(g) + 127) / 255) as u8
}

fn limited_to_gray(y: u8) -> u8 {
    ((i32::from(y) - 16) * 255 + 109)
        .div_euclid(219)
        .clamp(0, 255) as u8
}
