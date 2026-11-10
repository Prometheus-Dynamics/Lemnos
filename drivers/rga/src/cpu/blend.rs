//! Porter-Duff composites on the CPU, of premultiplied `Abgr32` (B, G, R, A in memory) images of
//! one size.
//!
//! The definition, per byte of each pixel (the three colours and alpha alike), with
//! `d(x) = (x + 127) / 255` the nearest byte of `x / 255`, and every result held at 255:
//! - [`BlendOp::Src`]: the foreground.
//! - [`BlendOp::SrcOver`]: `fg + d(bg (255 - fg_alpha))`.
//! - [`BlendOp::DstOver`]: `bg + d(fg (255 - bg_alpha))`.
//!
//! Premultiplied input keeps every colour byte at or below its alpha, so the sum never passes
//! 255 for valid input; the hold only matters for input that is not premultiplied. Scalar loops
//! only: no unit's own rounding is known (see [`crate::caps`]).

use std::io;

use super::convert::check;
use crate::{Layout, PixelFormat};

/// How the foreground is combined with the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendOp {
    /// The foreground replaces the background.
    Src,
    /// The foreground over the background.
    SrcOver,
    /// The background over the foreground.
    DstOver,
}

fn d(x: u32) -> u32 {
    (x + 127) / 255
}

/// Composites `fg` with `bg` into `out` with `op`. All three are premultiplied `Abgr32` of the
/// same width and height, each with its own stride.
pub fn blend(
    op: BlendOp,
    fg: Layout,
    fg_buf: &[u8],
    bg: Layout,
    bg_buf: &[u8],
    out: Layout,
    out_buf: &mut [u8],
) -> io::Result<()> {
    let size = (fg.width, fg.height);
    if [fg, bg, out]
        .iter()
        .any(|l| l.format != PixelFormat::Abgr32 || (l.width, l.height) != size)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("a blend takes three Abgr32 images of one size: {fg:?}, {bg:?}, {out:?}"),
        ));
    }
    check(fg, fg_buf.len())?;
    check(bg, bg_buf.len())?;
    check(out, out_buf.len())?;
    let (w, h) = (fg.width as usize, fg.height as usize);
    for y in 0..h {
        let f = &fg_buf[y * fg.stride as usize..][..4 * w];
        let b = &bg_buf[y * bg.stride as usize..][..4 * w];
        let o = &mut out_buf[y * out.stride as usize..][..4 * w];
        let pixels = f.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0);
        for (o, (f, b)) in o.as_chunks_mut::<4>().0.iter_mut().zip(pixels) {
            *o = match op {
                BlendOp::Src => *f,
                BlendOp::SrcOver => {
                    let keep = 255 - u32::from(f[3]);
                    [0, 1, 2, 3]
                        .map(|i| (u32::from(f[i]) + d(u32::from(b[i]) * keep)).min(255) as u8)
                }
                BlendOp::DstOver => {
                    let keep = 255 - u32::from(b[3]);
                    [0, 1, 2, 3]
                        .map(|i| (u32::from(b[i]) + d(u32::from(f[i]) * keep)).min(255) as u8)
                }
            };
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(px: [u8; 4]) -> (Layout, Vec<u8>) {
        (Layout::packed(PixelFormat::Abgr32, 1, 1), px.to_vec())
    }

    fn run(op: BlendOp, f: [u8; 4], b: [u8; 4]) -> [u8; 4] {
        let ((fl, fb), (bl, bb)) = (one(f), one(b));
        let mut out = [0u8; 4];
        blend(op, fl, &fb, bl, &bb, fl, &mut out).unwrap();
        out
    }

    #[test]
    fn the_three_operations_follow_their_definitions() {
        let opaque = [10, 20, 30, 255];
        let clear = [0, 0, 0, 0];
        let half = [50, 60, 70, 128];
        assert_eq!(run(BlendOp::Src, half, opaque), half);
        assert_eq!(run(BlendOp::SrcOver, opaque, half), opaque);
        assert_eq!(run(BlendOp::SrcOver, clear, half), half);
        assert_eq!(run(BlendOp::DstOver, half, opaque), opaque);
        assert_eq!(run(BlendOp::DstOver, half, clear), half);
        // 50 + (10 * 127 + 127) / 255 = 50 + 5.
        assert_eq!(run(BlendOp::SrcOver, half, opaque), [55, 70, 85, 255]);
        // Not premultiplied: held at 255, not wrapped.
        assert_eq!(
            run(BlendOp::SrcOver, [255, 0, 0, 0], [255, 0, 0, 0]),
            [255, 0, 0, 0]
        );
    }

    #[test]
    fn strides_are_kept_and_mismatches_refused() {
        let l = Layout {
            format: PixelFormat::Abgr32,
            width: 2,
            height: 2,
            stride: 12,
        };
        let fg = vec![200u8; 24];
        let bg = vec![1u8; 24];
        let mut out = vec![0xA5u8; 24];
        blend(BlendOp::Src, l, &fg, l, &bg, l, &mut out).unwrap();
        assert!(out[..8].iter().all(|&b| b == 200));
        assert!(out[8..12].iter().all(|&b| b == 0xA5));
        let rgb = Layout::packed(PixelFormat::Rgb24, 2, 2);
        assert!(blend(BlendOp::Src, l, &fg, rgb, &bg, l, &mut out).is_err());
        assert!(blend(BlendOp::Src, l, &fg[..20], l, &bg, l, &mut out).is_err());
        assert!(blend(BlendOp::Src, l, &fg, l, &bg, l, &mut out[..23]).is_err());
    }
}
