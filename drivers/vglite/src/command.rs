//! The command stream of each VGLite operation, built through
//! `lemnos_driver_vglite_sys::Stream` so that every address and extent in it is checked against
//! the buffer it names.
//!
//! Ported from `VGLite/vg_lite.c`, `vg_lite_image.c`, `vg_lite_path.c` and `vg_lite_context.h`
//! (Vivante, MIT). The notice the vendor ships with those files:
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
//! The streams are those of `VGLite/vg_lite.c` (`vg_lite_clear`, `vg_lite_blit`, the palette
//! upload of `vg_lite_image.c`) and `VGLite/vg_lite_path.c` (`vg_lite_draw`'s stroke branch), of
//! `kendryte/k230_linux_sdk` at `fc6cc21` (`buildroot-overlay/package/vg_lite/`). Every value is
//! written in full, as a fresh context writes it: the vendor skips a register it believes
//! unchanged.
//!
//! Each word goes through a typed method of `lemnos_driver_vglite_sys::Stream`, which checks the
//! buffers' extents, the formats, the configuration and path control bits and the rectangles; see
//! that crate. Where the vendor's source does not name a register, the constant here says what
//! it is used for and nothing more (untested on board).

use std::io;

use lemnos_driver_vglite_sys::{Buffer, Layout, SourceOptions, Stream};

use crate::plan::{
    self, BLEND_DST_OVER, BLEND_NONE, BLEND_SRC_OVER, Format, IDENTITY, Image, Matrix, Sampling,
};

/// `vg_lite_blit`'s `in_premult` bit (`gcFEATURE_VG_SRC_PREMULTIPLIED` 1), which a blit sets
/// for every source but A8. The clear and the path draw set the same bit always.
const IN_PREMULT: u32 = 0x1000_0000;
/// The image mode `NORMAL_IMAGE_MODE` gives (`vg_lite_blit`'s `imageMode`).
const IMAGE_MODE_NORMAL: u32 = 0x1001;
/// The `transparency_mode` a rotated blit sets (`gcFEATURE_VG_BORDER_CULLING` 1).
const ROTATED_TRANSPARENT: u32 = 0x8000;
/// `vg_lite_clear`'s modes: a rectangle, or the pixel engine's clear of whole rows.
const CLEAR_RECTANGLE: u32 = 0x1000_0001;
const CLEAR_PIXEL_ENGINE: u32 = 0x1000_0004;

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
    /// A stroke outline (`vg_lite_update_stroke`'s FP32 path bytes) drawn in `colour` with the
    /// identity matrix, as `vg_lite_draw` does for a stroke path that is not uploaded.
    Stroke { outline: &'c [u8], colour: u32 },
}

/// Encodes `op` into `stream`. Returns `false` when the operation draws nothing; the stream is
/// then left as it was, for the caller to drop.
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
        Op::Stroke { outline, colour } => stroke(stream, target, outline, colour),
        Op::Palette { colours } => {
            let Some(source) = source else {
                return plan::invalid("a palette has indices".into());
            };
            if source.image.format != Format::Index8 {
                return plan::invalid(format!(
                    "palette indices as {:?}, not Index8",
                    source.image.format
                ));
            }
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

/// `set_render_target`: the target's format, address, stride and size. No mirror bit:
/// `VG_LITE_ORIENTATION_TOP_BOTTOM` is 0 in the library.
fn set_render_target<'a, B: Buffer>(
    stream: &mut Stream<'a>,
    target: Bound<'a, B>,
) -> io::Result<()> {
    let format = target.image.format.target().ok_or_else(|| {
        plan::invalid_err(format!("{:?} is not a render target", target.image.format))
    })?;
    stream.render_target(target.buffer, format, target.layout())
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
    stream.colour(color)?;
    stream.zero(0x0A39)?;
    if x == 0 && y == 0 && width == target.image.width {
        let size = target.image.stride.checked_mul(height).ok_or_else(|| {
            plan::invalid_err(format!(
                "a clear of {height} rows of {} bytes",
                target.image.stride
            ))
        })?;
        stream.config(CLEAR_PIXEL_ENGINE)?;
        stream.pe_clear(size)?;
    } else {
        stream.config(CLEAR_RECTANGLE)?;
        stream.rectangle(x, y, width, height)?;
    }
    stream.target_flush()?;
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
        hi.0.min(i32::try_from(target.width).unwrap_or(i32::MAX)),
        hi.1.min(i32::try_from(target.height).unwrap_or(i32::MAX)),
    );
    if hi.0 <= lo.0 || hi.1 <= lo.1 {
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
    let align = source.image.format.stride_align();
    if !source.image.stride.is_multiple_of(align) {
        return plan::invalid(format!(
            "source stride {} is not a multiple of {align}",
            source.image.stride
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
    let options = SourceOptions {
        bilinear: sampling == Sampling::BiLinear,
        luma_conversion: target.image.format == Format::L8
            && !matches!(source.image.format, Format::L8 | Format::A8),
        read_split: blending != 0,
    };

    stream.zero(0x0AD1)?;
    stream.config(in_premult | IMAGE_MODE_NORMAL | blending | transparency)?;
    stream.colour(0)?;
    stream.steps(steps.c, steps.x, steps.y)?;
    stream.source(
        source.buffer,
        source.image.format.source(),
        source.layout(),
        options,
    )?;
    // `left` and `top` are at least 0 and below `right` and `bottom`, which are at most the
    // target's sides: every value fits `u32`.
    stream.rectangle(
        left as u32,
        top as u32,
        (right - left) as u32,
        (bottom - top) as u32,
    )?;
    stream.flush()?;
    Ok(true)
}

/// The path draw's control word (`vg_lite_draw`, non-parallel branch): the control bit, FP32 path
/// data (`convert_path_format`), `VG_LITE_LOW` quality (non-antialiased: `convert_path_quality`
/// gives 0 for it), no fill rule (non-zero), no tiling.
const PATH_CONTROL: u32 = 0x0100_0000 | 0x0030_0000;
/// The stroke bit the stroke window sets.
const PATH_STROKE: u32 = 0x200;
/// The configuration word of a path draw with no blend: linear target, no scissor, no colour
/// transform, no matrix.
const PATH_CONFIG_NONE: u32 = 0x1000_0000;

/// A stroke draw of `outline` over the whole target, in `colour`, with the identity matrix.
/// The outline is inline data, so the command buffer bounds it. The window is the whole target,
/// as the vendor's is the path's bounds clipped to the target, in one window
/// (`s_context.tessbuf.tess_w_h` is set to the window's size).
fn stroke<'a, B: Buffer>(
    stream: &mut Stream<'a>,
    target: Bound<'a, B>,
    outline: &[u8],
    colour: u32,
) -> io::Result<bool> {
    if outline.is_empty() {
        return Ok(false);
    }
    set_render_target(stream, target)?;
    stream.config(PATH_CONFIG_NONE)?;
    stream.colour(colour)?;
    stream.path_control(PATH_CONTROL)?;
    // `new_matrix` for the identity with scale 1 and bias 0: (m00, m01, (m00 + m01) * 0 + m02,
    // and the same of the second row), then the translation.
    let m = IDENTITY.m;
    let (scale, bias) = (1.0f32, 0.0f32);
    stream.path_transform(
        scale,
        bias,
        [
            m[0][0] * scale,
            m[0][1] * scale,
            (m[0][0] + m[0][1]) * bias + m[0][2],
            m[1][0] * scale,
            m[1][1] * scale,
            (m[1][0] + m[1][1]) * bias + m[1][2],
        ],
        [m[0][2], m[1][2]],
    )?;
    stream.tessellation_window(0, 0, target.image.width, target.image.height)?;
    stream.path_control(PATH_CONTROL | PATH_STROKE)?;
    stream.colour(colour)?;
    stream.path_data(outline)?;
    Ok(true)
}

/// `program_tessellation` (this chip's branch) and the colour-key reset of `vg_lite_init`.
pub(crate) fn init(stream: &mut Stream<'_>) -> io::Result<()> {
    stream.tessellation_setup()?;
    stream.colour_key_reset()
}
