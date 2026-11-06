//! The command stream of each VGLite operation, built through `lemnos_linux_sys::vg_lite::Stream`
//! so that every address and extent in it is checked against the buffer it names.
//!
//! Ported from `VGLite/vg_lite.c`, `vg_lite_image.c` and `vg_lite_context.h` of the K230 Linux
//! SDK v0.6.1 (Vivante, MIT). The notice the vendor ships with those files:
//!
//! ```text
//! Copyright 2012 - 2022 Vivante Corporation, Santa Clara, California.
//! All Rights Reserved.
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy of this
//! software and associated documentation files (the 'Software'), to deal in the Software
//! without restriction, including without limitation the rights to use, copy, modify, merge,
//! publish, distribute, sub license, and/or sell copies of the Software, and to permit persons
//! to whom the Software is furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice (including the next paragraph) shall
//! be included in all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED 'AS IS', WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
//! INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
//! PURPOSE AND NON-INFRINGEMENT. IN NO EVENT SHALL VIVANTE AND/OR ITS SUPPLIERS BE LIABLE FOR
//! ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE,
//! ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.
//! ```
//!
//! The register addresses are the ones the vendor writes. Where the vendor's source names a
//! register, the name is used; where it does not, the constant says what it is used for and
//! nothing more (untested on board).
//!
//! Scope: the streams of clear, blit, palette (a 256-entry colour table and an `Index8` blit).
//! Lines are strokes and are not here.
//!
//! Rectangles: a rectangle is refused by the stream unless it lies inside the render target's
//! width and height. Whether the GPU also clips its writes to that size, not just to the
//! target's extent, is unverified on board; the extent check is what this crate relies on.

use std::io;

use lemnos_linux_sys::vg_lite::{Buffer, Layout, Stream};

use crate::plan::{
    self, BLEND_DST_OVER, BLEND_NONE, BLEND_SRC_OVER, Format, IDENTITY, Image, Matrix, Sampling,
};

/// The premultiply bit of the render target's format word (`premultiply_dst`).
const PREMULTIPLY_DST: u32 = 0x100;
/// `vg_lite_blit`'s `in_premult` bit (`gcFEATURE_VG_SRC_PREMULTIPLIED` 1).
const IN_PREMULT: u32 = 0x1000_0000;
/// The image mode `NORMAL_IMAGE_MODE` gives (`vg_lite_blit`'s `imageMode`).
const IMAGE_MODE_NORMAL: u32 = 0x1001;
/// The source's premultiply enable (`src_premultiply_enable` with `HW_PREMULTIPLY` 0).
const SRC_PREMULTIPLY: u32 = 0x0100_0000;
/// The `transparency_mode` a rotated blit sets (`gcFEATURE_VG_BORDER_CULLING` 1).
const ROTATED_TRANSPARENT: u32 = 0x8000;
/// The conversion bit of a source written to an L8 target.
const LUMA_CONVERSION: u32 = 0x8000_0000;
/// `ahb_read_split`: set when the blend reads the target.
const AHB_READ_SPLIT: u32 = 0x80;

/// A buffer as an operation uses it: its layout, which the stream checks, and the buffer
/// itself, borrowed so the stream cannot outlive it.
pub(crate) struct Bound<'a, B: Buffer> {
    pub(crate) image: Image,
    pub(crate) buffer: &'a B,
}

impl<B: Buffer> Clone for Bound<'_, B> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B: Buffer> Copy for Bound<'_, B> {}

impl<B: Buffer> Bound<'_, B> {
    fn layout(&self) -> Layout {
        Layout {
            bytes_per_pixel: self.image.format.bytes_per_pixel(),
            width: self.image.width,
            height: self.image.height,
            stride: self.image.stride,
        }
    }
}

/// One operation's parameters.
pub(crate) enum Op<'c> {
    /// `vg_lite_clear` of `rect` (left, top, width, height) to `color` (`0xAABBGGRR`).
    Clear {
        rect: (u32, u32, u32, u32),
        color: u32,
    },
    /// `vg_lite_blit` of the source through `matrix`, with a `plan` blend and `sampling`.
    Blit {
        matrix: Matrix,
        blend: u32,
        sampling: Sampling,
    },
    /// A 256-entry colour table, then an identity blit of an `Index8` source through it.
    Palette { colours: &'c [u32; 256] },
}

/// Encodes `op` into `stream`. Returns `false` when the operation draws nothing; the stream is
/// then left as it was for the caller to drop.
pub(crate) fn encode<'a, B: Buffer>(
    op: Op<'_>,
    stream: &mut Stream<'a>,
    target: Bound<'a, B>,
    source: Option<Bound<'a, B>>,
) -> io::Result<bool> {
    match op {
        Op::Clear { rect, color } => clear(stream, target, rect, color),
        Op::Blit {
            matrix,
            blend,
            sampling,
        } => {
            let Some(source) = source else {
                return plan::invalid("a blit has a source".into());
            };
            blit(stream, target, source, &matrix, blend, sampling)
        }
        Op::Palette { colours } => {
            let Some(source) = source else {
                return plan::invalid("a palette has indices".into());
            };
            stream.clut(colours)?;
            blit(
                stream,
                target,
                source,
                &IDENTITY,
                BLEND_NONE,
                Sampling::Point,
            )
        }
    }
}

/// `set_render_target`: the target's format, address, stride and size.
fn set_render_target<'a, B: Buffer>(
    stream: &mut Stream<'a>,
    target: Bound<'a, B>,
) -> io::Result<()> {
    let code = target.image.format.target_code().ok_or_else(|| {
        plan::invalid_err(format!("{:?} is not a render target", target.image.format))
    })?;
    // No mirror bit: `VG_LITE_ORIENTATION_TOP_BOTTOM` is 0 in the library.
    stream.render_target(target.buffer, code | PREMULTIPLY_DST, target.layout())
}

fn clear<'a, B: Buffer>(
    stream: &mut Stream<'a>,
    target: Bound<'a, B>,
    rect: (u32, u32, u32, u32),
    color: u32,
) -> io::Result<bool> {
    let (x, y, width, height) = rect;
    if width == 0 || height == 0 {
        return Ok(false);
    }
    set_render_target(stream, target)?;
    stream.zero(0x0A34)?;
    stream.plain(0x0A02, color)?;
    stream.zero(0x0A39)?;
    if x == 0 && y == 0 && width == target.image.width {
        let size = target.image.stride.checked_mul(height).ok_or_else(|| {
            plan::invalid_err(format!(
                "a clear of {height} rows of {} bytes",
                target.image.stride
            ))
        })?;
        stream.plain(0x0A00, 0x1000_0004)?;
        stream.pe_clear(size)?;
    } else {
        stream.plain(0x0A00, 0x1000_0001)?;
        stream.rectangle(x, y, width, height)?;
    }
    stream.plain(0x0A1B, 1)?;
    Ok(true)
}

/// `convert_blend`: the blend bits of the configuration word.
fn blend_bits(blend: u32) -> u32 {
    match blend {
        BLEND_SRC_OVER => 0x100,
        BLEND_DST_OVER => 0x200,
        _ => 0,
    }
}

/// The clipped rectangle a blit writes, as `(left, top, right, bottom)`, or `None` when empty.
/// The corners of the source are mapped through `matrix` and their bounds clipped to the target
/// (`vg_lite_blit`'s first half).
fn draw_bounds(
    target: Image,
    source: Image,
    matrix: &Matrix,
    sampling: Sampling,
) -> io::Result<Option<(i32, i32, i32, i32)>> {
    let (w, h) = (source.width as f32, source.height as f32);
    let mut lo = (i32::MAX, i32::MAX);
    let mut hi = (i32::MIN, i32::MIN);
    for (x, y) in [(0.0, 0.0), (0.0, h), (w, h), (w, 0.0)] {
        let Some((px, py)) = plan::map_point(matrix, x, y, sampling) else {
            return plan::invalid("a blit corner maps behind the projection plane".into());
        };
        lo = (lo.0.min(px), lo.1.min(py));
        hi = (hi.0.max(px), hi.1.max(py));
    }
    let lo = (lo.0.max(0), lo.1.max(0));
    let hi = (
        hi.0.min(target.width as i32),
        hi.1.min(target.height as i32),
    );
    if hi.0 - lo.0 <= 0 || hi.1 - lo.1 <= 0 {
        return Ok(None);
    }
    Ok(Some((lo.0, lo.1, hi.0, hi.1)))
}

fn blit<'a, B: Buffer>(
    stream: &mut Stream<'a>,
    target: Bound<'a, B>,
    source: Bound<'a, B>,
    matrix: &Matrix,
    blend: u32,
    sampling: Sampling,
) -> io::Result<bool> {
    if !source
        .image
        .stride
        .is_multiple_of(source.image.format.stride_align())
    {
        return plan::invalid(format!(
            "source stride {} is not a multiple of {}",
            source.image.stride,
            source.image.format.stride_align()
        ));
    }
    let Some((left, top, right, bottom)) =
        draw_bounds(target.image, source.image, matrix, sampling)?
    else {
        return Ok(false);
    };
    set_render_target(stream, target)?;
    let inverse =
        plan::inverse(matrix).ok_or_else(|| plan::invalid_err("a singular blit matrix".into()))?;
    let steps = plan::steps(&inverse, source.image.width, source.image.height, sampling);

    // The source is straight alpha and premultiplied by the unit; A8 sources carry no colour.
    let in_premult = if source.image.format == Format::A8 {
        0
    } else {
        IN_PREMULT
    };
    let rotated = matrix.m[0][1] != 0.0
        || matrix.m[1][0] != 0.0
        || matrix.m[2][0] != 0.0
        || matrix.m[2][1] != 0.0
        || matrix.m[2][2] != 1.0;
    let transparency = if rotated && blend == BLEND_NONE {
        ROTATED_TRANSPARENT
    } else {
        0
    };
    let blending = blend_bits(blend);
    let conversion = if target.image.format == Format::L8
        && !matches!(source.image.format, Format::L8 | Format::A8)
    {
        LUMA_CONVERSION
    } else {
        0
    };
    let ahb_read_split = if blending != 0 { AHB_READ_SPLIT } else { 0 };

    stream.zero(0x0AD1)?;
    stream.plain(
        0x0A00,
        in_premult | IMAGE_MODE_NORMAL | blending | transparency,
    )?;
    stream.plain(0x0A02, 0)?;
    stream.plain(0x0A18, steps.c[0].to_bits())?;
    stream.plain(0x0A19, steps.c[1].to_bits())?;
    stream.plain(0x0A1A, steps.c[2].to_bits())?;
    stream.plain(0x0A1C, steps.x[0].to_bits())?;
    stream.plain(0x0A1D, steps.x[1].to_bits())?;
    stream.plain(0x0A1E, steps.x[2].to_bits())?;
    stream.plain(0x0A1F, 1)?;
    stream.plain(0x0A20, steps.y[0].to_bits())?;
    stream.plain(0x0A21, steps.y[1].to_bits())?;
    stream.plain(0x0A22, steps.y[2].to_bits())?;
    stream.plain(
        0x0A25,
        source.image.format.source_code()
            | sampling.filter_bits()
            | conversion
            | ahb_read_split
            | SRC_PREMULTIPLY,
    )?;
    stream.source_surface(source.buffer, source.layout())?;
    stream.rectangle(
        left as u32,
        top as u32,
        (right - left) as u32,
        (bottom - top) as u32,
    )?;
    stream.flush()?;
    Ok(true)
}

/// `program_tessellation` (the C908 build) and the colour-key reset of `vg_lite_init`.
pub(crate) fn init(stream: &mut Stream<'_>) -> io::Result<()> {
    stream.tessellation_setup()?;
    for key in 0x0A90..=0x0A97 {
        stream.plain(key, 0)?;
    }
    Ok(())
}
