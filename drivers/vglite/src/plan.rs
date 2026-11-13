//! What each operation asks of VGLite, worked out before any command is encoded: the blit
//! matrix, the sampling, the blend, the formats. Anything the unit cannot do as asked is
//! [`io::ErrorKind::Unsupported`], never a substitute.
//!
//! The matrix helpers are ports of `transform`, `inverse` and the step set-up of `vg_lite_blit`
//! in `VGLite/vg_lite.c` (`kendryte/k230_linux_sdk` at `fc6cc21`, Vivante, MIT; the notice is at
//! the top of `command.rs`).

use std::io;

use lemnos_driver_vglite_sys::{SourceFormat, TargetFormat};

/// A resize filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Filter {
    /// The mean of the source pixels under each output pixel. VGLite has no averaging filter:
    /// only an exact 2x downscale is served (see [`crate::Vglite::resize_bytes`]).
    Area,
    /// Bilinear, sampled at pixel centres.
    Bilinear,
}

/// A flip or a clockwise quarter turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Transform {
    /// Mirror left to right.
    FlipHorizontal,
    /// Mirror top to bottom.
    FlipVertical,
    /// A quarter turn clockwise.
    Rotate90,
    /// Both flips.
    Rotate180,
    /// Three quarter turns clockwise.
    Rotate270,
}

impl Transform {
    /// The output size for a `width` by `height` input.
    pub fn output_size(self, width: u32, height: u32) -> (u32, u32) {
        match self {
            Self::Rotate90 | Self::Rotate270 => (height, width),
            _ => (width, height),
        }
    }
}

/// A pixel format the unit reads and writes, as the vendor's build for this chip has them
/// (`vg_lite_options.h`: no 24-bit RGB, no planar YUV input).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// R, G, B, A bytes.
    Rgba8888,
    /// B, G, R, A bytes.
    Bgra8888,
    /// R, G, B, X bytes: the fourth byte is ignored and the pixel is opaque.
    Rgbx8888,
    /// One luma byte.
    L8,
    /// One alpha byte.
    A8,
    /// 8-bit indices through the colour table: a source of [`crate::Vglite::palette_bytes`].
    Index8,
}

impl Format {
    /// Bytes per pixel.
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgba8888 | Self::Bgra8888 | Self::Rgbx8888 => 4,
            Self::L8 | Self::A8 | Self::Index8 => 1,
        }
    }

    /// The format as a source (`convert_source_format`).
    pub(crate) fn source(self) -> SourceFormat {
        match self {
            Self::L8 => SourceFormat::L8,
            Self::A8 => SourceFormat::A8,
            Self::Rgba8888 => SourceFormat::Rgba8888,
            Self::Bgra8888 => SourceFormat::Bgra8888,
            Self::Rgbx8888 => SourceFormat::Rgbx8888,
            Self::Index8 => SourceFormat::Index8,
        }
    }

    /// The format as a render target (`convert_target_format`), or `None` where VGLite has no
    /// target for it (the library refuses `Index8`).
    pub(crate) fn target(self) -> Option<TargetFormat> {
        match self {
            Self::A8 => Some(TargetFormat::A8),
            Self::L8 => Some(TargetFormat::L8),
            Self::Rgba8888 => Some(TargetFormat::Rgba8888),
            Self::Bgra8888 => Some(TargetFormat::Bgra8888),
            Self::Rgbx8888 => Some(TargetFormat::Rgbx8888),
            Self::Index8 => None,
        }
    }

    /// The row alignment VGLite asks of a source, in bytes (`_check_source_aligned`).
    pub(crate) fn stride_align(self) -> u32 {
        match self.bytes_per_pixel() {
            4 => 64,
            _ => 16,
        }
    }
}

/// The largest width or height: the GPU's size registers hold 16 bits a side.
pub const MAX_SIDE: u32 = u16::MAX as u32;

/// An image's size and layout. The stride is in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image {
    /// The pixel format.
    pub format: Format,
    /// Width in pixels.
    pub width: u32,
    /// Height in rows.
    pub height: u32,
    /// Bytes from one row to the next.
    pub stride: u32,
}

impl Image {
    /// Tightly packed rows.
    pub fn packed(format: Format, width: u32, height: u32) -> Self {
        Self {
            format,
            width,
            height,
            stride: width.saturating_mul(format.bytes_per_pixel()),
        }
    }

    /// The bytes the image spans: full rows but the last.
    pub fn frame_len(&self) -> usize {
        if self.height == 0 {
            return 0;
        }
        (self.height as usize - 1) * self.stride as usize + self.row_len()
    }

    pub(crate) fn row_len(&self) -> usize {
        self.width as usize * self.format.bytes_per_pixel() as usize
    }

    /// The row stride `vg_lite_allocate` gives a buffer it allocates: the row rounded up to 16
    /// pixels (`VG_LITE_ALIGN(width * mul / div, 16 * mul / div)`).
    pub(crate) fn staged_stride(&self) -> u32 {
        let bpp = self.format.bytes_per_pixel();
        (self.width * bpp).next_multiple_of(16 * bpp)
    }

    /// The image is one VGLite can take (not empty, each side within the 16-bit fields of its
    /// registers) and `len` bytes hold it.
    pub(crate) fn check(&self, len: usize) -> io::Result<()> {
        if self.width == 0 || self.height == 0 {
            return invalid(format!("empty image {self:?}"));
        }
        if self.width > MAX_SIDE || self.height > MAX_SIDE {
            return invalid(format!("{self:?}: a side past {MAX_SIDE}"));
        }
        if (self.stride as usize) < self.row_len() {
            return invalid(format!("stride shorter than a row: {self:?}"));
        }
        if len < self.frame_len() {
            return invalid(format!(
                "{len} bytes for an image of {}: {self:?}",
                self.frame_len()
            ));
        }
        Ok(())
    }
}

/// A Porter-Duff mode of [`crate::Vglite::blend_bytes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendMode {
    /// The foreground replaces the destination.
    Src,
    /// The foreground over the destination (straight alpha).
    SrcOver,
    /// The destination over the foreground (straight alpha).
    DstOver,
}

/// How a blit samples its source: the point the centre of each target pixel maps to, or the
/// bilinear filter at the half-pixel phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sampling {
    Point,
    BiLinear,
}

/// `VG_LITE_BLEND_NONE`: the source replaces the destination.
pub(crate) const BLEND_NONE: u32 = 0x2000;
/// `VG_LITE_BLEND_SRC_OVER`.
pub(crate) const BLEND_SRC_OVER: u32 = 0x2001;
/// `VG_LITE_BLEND_DST_OVER`.
pub(crate) const BLEND_DST_OVER: u32 = 0x2002;

pub(crate) fn invalid<T>(why: String) -> io::Result<T> {
    Err(invalid_err(why))
}

pub(crate) fn invalid_err(why: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why)
}

pub(crate) fn unsupported<T>(why: String) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::Unsupported, why))
}

/// `rect` (left, top, width, height) is non-empty and inside a `width` by `height` frame.
pub(crate) fn check_rect(rect: (u32, u32, u32, u32), width: u32, height: u32) -> io::Result<()> {
    let (left, top, w, h) = rect;
    let inside = w > 0
        && h > 0
        && left.checked_add(w).is_some_and(|r| r <= width)
        && top.checked_add(h).is_some_and(|b| b <= height);
    if inside {
        Ok(())
    } else {
        invalid(format!("{rect:?} is not inside a {width}x{height} frame"))
    }
}

/// The output is `want` in size and `format`, or the call would be a resize or a conversion.
pub(crate) fn check_output(dst: Image, want: (u32, u32), format: Format) -> io::Result<()> {
    if (dst.width, dst.height) != want {
        return invalid(format!(
            "a {}x{} output for a {}x{} result; that is a resize",
            dst.width, dst.height, want.0, want.1
        ));
    }
    if dst.format != format {
        return unsupported(format!(
            "{:?} output from {format:?}: one format at a time",
            dst.format
        ));
    }
    Ok(())
}

/// A 3x3 matrix, `m[row][column]`: a point `(x, y)` maps to `m * (x, y, 1)`, as
/// `vg_lite_matrix_t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Matrix {
    pub(crate) m: [[f32; 3]; 3],
}

pub(crate) const IDENTITY: Matrix = Matrix {
    m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
};

fn affine(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Matrix {
    Matrix {
        m: [[a, b, c], [d, e, f], [0.0, 0.0, 1.0]],
    }
}

/// The inverse of `matrix` (`inverse` in the library), or `None` when it is singular.
pub(crate) fn inverse(matrix: &Matrix) -> Option<Matrix> {
    let m = &matrix.m;
    let det00 = (m[1][1] * m[2][2]) - (m[2][1] * m[1][2]);
    let det01 = (m[2][0] * m[1][2]) - (m[1][0] * m[2][2]);
    let det02 = (m[1][0] * m[2][1]) - (m[2][0] * m[1][1]);
    let d = (m[0][0] * det00) + (m[0][1] * det01) + (m[0][2] * det02);
    if d == 0.0 {
        return None;
    }
    let d = 1.0 / d;
    let is_affine = m[2][0] == 0.0 && m[2][1] == 0.0 && m[2][2] == 1.0;
    Some(Matrix {
        m: [
            [
                d * det00,
                d * ((m[2][1] * m[0][2]) - (m[0][1] * m[2][2])),
                d * ((m[0][1] * m[1][2]) - (m[1][1] * m[0][2])),
            ],
            [
                d * det01,
                d * ((m[0][0] * m[2][2]) - (m[2][0] * m[0][2])),
                d * ((m[1][0] * m[0][2]) - (m[0][0] * m[1][2])),
            ],
            [
                if is_affine { 0.0 } else { d * det02 },
                if is_affine {
                    0.0
                } else {
                    d * ((m[2][0] * m[0][1]) - (m[0][0] * m[2][1]))
                },
                if is_affine {
                    1.0
                } else {
                    d * ((m[0][0] * m[1][1]) - (m[1][0] * m[0][1]))
                },
            ],
        ],
    })
}

/// Where `(x, y)` lands in the target (`transform` in the library): the projected point,
/// rounded half up and truncated, or `None` when the point maps to infinity or behind.
pub(crate) fn map_point(matrix: &Matrix, x: f32, y: f32, sampling: Sampling) -> Option<(i32, i32)> {
    let m = &matrix.m;
    let (mut x, mut y) = (x, y);
    let projective =
        m[0][1] != 0.0 || m[1][0] != 0.0 || m[2][0] != 0.0 || m[2][1] != 0.0 || m[2][2] != 1.0;
    if projective && sampling == Sampling::BiLinear {
        if x != 0.0 {
            x += 0.5;
        }
        if y != 0.0 {
            y += 0.5;
        }
    }
    let px = (x * m[0][0]) + (y * m[0][1]) + m[0][2];
    let py = (x * m[1][0]) + (y * m[1][1]) + m[1][2];
    let pw = (x * m[2][0]) + (y * m[2][1]) + m[2][2];
    if pw <= 0.0 {
        return None;
    }
    Some((((px / pw) + 0.5) as i32, ((py / pw) + 0.5) as i32))
}

/// The per-pixel steps of a blit: the colour, x and y increments of the inverse matrix, each as
/// `[step, per row or column, offset]` (`c_step`, `x_step`, `y_step` in `vg_lite_blit`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Steps {
    pub(crate) c: [f32; 3],
    pub(crate) x: [f32; 3],
    pub(crate) y: [f32; 3],
}

/// The steps of `inverse` for a source of `width` by `height`, sampled as `sampling`.
pub(crate) fn steps(inverse: &Matrix, width: u32, height: u32, sampling: Sampling) -> Steps {
    let m = &inverse.m;
    let (w, h) = (width as f32, height as f32);
    match sampling {
        Sampling::Point => Steps {
            x: [m[0][0] / w, m[1][0] / h, m[2][0]],
            y: [m[0][1] / w, m[1][1] / h, m[2][1]],
            c: [
                (0.5 * (m[0][0] + m[0][1]) + m[0][2]) / w,
                (0.5 * (m[1][0] + m[1][1]) + m[1][2]) / h,
                0.5 * (m[2][0] + m[2][1]) + m[2][2],
            ],
        },
        Sampling::BiLinear => Steps {
            x: [
                (m[0][0] - 0.5 * m[2][0]) / w,
                (m[1][0] - 0.5 * m[2][0]) / h,
                m[2][0],
            ],
            y: [
                (m[0][1] - 0.5 * m[2][1]) / w,
                (m[1][1] - 0.5 * m[2][1]) / h,
                m[2][1],
            ],
            c: [
                (0.5 * (m[0][0] + m[0][1]) - 0.25 * (m[2][0] + m[2][1]) + m[0][2] - 0.5 * m[2][2])
                    / w,
                (0.5 * (m[1][0] + m[1][1]) - 0.25 * (m[2][0] + m[2][1]) + m[1][2] - 0.5 * m[2][2])
                    / h,
                0.5 * (m[2][0] + m[2][1]) + m[2][2],
            ],
        },
    }
}

/// The matrix and sampling of a resize of `src` to `dst`.
///
/// Bilinear: a scale by `dst / src` per axis, either way. Area: only a 2x downscale, as a
/// bilinear blit at half scale. At the half-pixel phase each output centre falls on the corner
/// of four source pixels, so the weights are a quarter each: the 2x2 mean, if VGLite's rounding
/// agrees (unmeasured). VGLite has no averaging filter; larger factors would need passes that
/// each round, so they are refused. A trailing odd row or column is cropped away, as the
/// reference does.
pub(crate) fn resize(src: Image, dst: Image, filter: Filter) -> io::Result<(Matrix, Sampling)> {
    if src.format != dst.format {
        return unsupported(format!(
            "resize from {:?} to {:?}: one format at a time",
            src.format, dst.format
        ));
    }
    let (sw, sh, dw, dh) = (src.width, src.height, dst.width, dst.height);
    match filter {
        Filter::Area => {
            if (sw / 2).max(1) != dw || (sh / 2).max(1) != dh || sw < 2 || sh < 2 {
                return unsupported(format!(
                    "area {sw}x{sh} to {dw}x{dh}: VGLite has no averaging filter; only a 2x \
                     downscale (a half-phase bilinear blit) is offered"
                ));
            }
            Ok((affine(0.5, 0.0, 0.0, 0.0, 0.5, 0.0), Sampling::BiLinear))
        }
        Filter::Bilinear => {
            if (sw, sh) == (dw, dh) {
                return unsupported("bilinear resize to the same size".into());
            }
            Ok((
                affine(
                    dw as f32 / sw as f32,
                    0.0,
                    0.0,
                    0.0,
                    dh as f32 / sh as f32,
                    0.0,
                ),
                Sampling::BiLinear,
            ))
        }
    }
}

/// The formats [`convert`] changes between: the three four-byte formats, each to each. A
/// blit names its source and its target format apart (`set_render_target` and the source's
/// image words), and the unit reads one and writes the other; that is all a conversion is.
pub const CONVERTIBLE: [Format; 3] = [Format::Rgba8888, Format::Bgra8888, Format::Rgbx8888];

/// A copy of `src` into `dst` of the same size, from one format to another: an identity blit,
/// point sampled. Among [`CONVERTIBLE`] any pair; otherwise only a format to itself (a copy).
/// A palette source is [`crate::Vglite::palette_bytes`]'s, and has no table here.
pub(crate) fn convert(src: Image, dst: Image) -> io::Result<()> {
    if (src.width, src.height) != (dst.width, dst.height) {
        return invalid(format!(
            "a {}x{} output for a {}x{} input; that is a resize",
            dst.width, dst.height, src.width, src.height
        ));
    }
    let pair = CONVERTIBLE.contains(&src.format) && CONVERTIBLE.contains(&dst.format);
    if !pair && (src.format != dst.format || src.format == Format::Index8) {
        return unsupported(format!(
            "{:?} to {:?}: not a conversion the unit is used for here",
            src.format, dst.format
        ));
    }
    Ok(())
}

/// The matrix of an unscaled crop of `rect` (left, top, width, height): a translation, point
/// sampled; what falls outside the target is clipped.
pub(crate) fn crop(rect: (u32, u32, u32, u32)) -> Matrix {
    affine(1.0, 0.0, -(rect.0 as f32), 0.0, 1.0, -(rect.1 as f32))
}

/// The matrix of a flip or a clockwise quarter turn of a `w` by `h` source, point sampled.
/// `(x, y)` on the source's pixel grid maps to the target's: a turn of 90 sends the top-left
/// corner to the top-right.
pub(crate) fn transform(t: Transform, w: u32, h: u32) -> Matrix {
    let (w, h) = (w as f32, h as f32);
    match t {
        Transform::FlipHorizontal => affine(-1.0, 0.0, w, 0.0, 1.0, 0.0),
        Transform::FlipVertical => affine(1.0, 0.0, 0.0, 0.0, -1.0, h),
        Transform::Rotate90 => affine(0.0, -1.0, h, 1.0, 0.0, 0.0),
        Transform::Rotate180 => affine(-1.0, 0.0, w, 0.0, -1.0, h),
        Transform::Rotate270 => affine(0.0, 1.0, 0.0, -1.0, 0.0, w),
    }
}

/// The VGLite blend (`vg_lite_blend_t`) for a mode. The K230's build sets the blit's
/// `in_premult` bit for every RGBA source (`gcFEATURE_VG_SRC_PREMULTIPLIED` 1), so the unit takes
/// straight alpha and premultiplies it itself. A premultiplied source would be multiplied twice:
/// refused, except for SRC, which copies.
pub(crate) fn blend(mode: BlendMode, premultiplied: bool) -> io::Result<u32> {
    Ok(match (mode, premultiplied) {
        (BlendMode::Src, _) => BLEND_NONE,
        (_, true) => {
            return unsupported(format!(
                "premultiplied {mode:?}: VGLite premultiplies the source itself"
            ));
        }
        (BlendMode::SrcOver, false) => BLEND_SRC_OVER,
        (BlendMode::DstOver, false) => BLEND_DST_OVER,
    })
}

/// `vg_lite_color_t` of RGBA bytes: R in the low byte (`0xAABBGGRR`).
pub(crate) fn color(rgba: [u8; 4]) -> u32 {
    u32::from_le_bytes(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &Matrix, x: f32, y: f32) -> (f32, f32) {
        (
            m.m[0][0] * x + m.m[0][1] * y + m.m[0][2],
            m.m[1][0] * x + m.m[1][1] * y + m.m[1][2],
        )
    }

    #[test]
    fn turns_send_the_corners_where_the_reference_does() {
        // The source's top-left pixel (its centre at 0.5, 0.5) lands on the reference's pixel.
        let (w, h) = (4.0, 2.0);
        let cases = [
            (Transform::FlipHorizontal, (w - 0.5, 0.5)),
            (Transform::FlipVertical, (0.5, h - 0.5)),
            (Transform::Rotate90, (h - 0.5, 0.5)),
            (Transform::Rotate180, (w - 0.5, h - 0.5)),
            (Transform::Rotate270, (0.5, w - 0.5)),
        ];
        for (t, want) in cases {
            assert_eq!(apply(&transform(t, 4, 2), 0.5, 0.5), want, "{t:?}");
        }
        assert_eq!(apply(&crop((3, 1, 2, 2)), 3.5, 1.5), (0.5, 0.5));
    }

    #[test]
    fn area_is_a_half_phase_2x_bilinear_and_nothing_else() {
        let img = |w, h| Image::packed(Format::L8, w, h);
        let (m, sampling) = resize(img(64, 32), img(32, 16), Filter::Area).unwrap();
        assert_eq!(sampling, Sampling::BiLinear);
        // Output centre (i + 0.5) samples the source at 2i + 1, minus the half-pixel shift:
        // between source pixels 2i and 2i + 1.
        assert_eq!(m.m[0][0], 0.5);
        assert!(resize(img(65, 33), img(32, 16), Filter::Area).is_ok());
        for bad in [(16, 8), (64, 32), (33, 16)] {
            let err = resize(img(64, 32), img(bad.0, bad.1), Filter::Area).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::Unsupported, "{bad:?}");
        }
        assert!(resize(img(64, 32), img(100, 50), Filter::Bilinear).is_ok());
        assert!(resize(img(64, 32), img(64, 32), Filter::Bilinear).is_err());
        let rgba = Image::packed(Format::Rgba8888, 32, 16);
        assert!(resize(img(64, 32), rgba, Filter::Bilinear).is_err());
    }

    #[test]
    fn layouts_blends_and_colours() {
        let img = Image::packed(Format::Rgba8888, 16, 2);
        assert!(img.check(128).is_ok() && img.check(127).is_err());
        assert!(
            Image::packed(Format::L8, MAX_SIDE + 1, 1)
                .check(usize::MAX)
                .is_err()
        );
        assert!(
            Image::packed(Format::L8, 1, MAX_SIDE + 1)
                .check(usize::MAX)
                .is_err()
        );
        assert!(
            Image::packed(Format::Rgba8888, u32::MAX, 1)
                .check(usize::MAX)
                .is_err()
        );
        assert_eq!(Image { stride: 80, ..img }.frame_len(), 80 + 64);
        // `vg_lite_allocate` rounds the row to 16 pixels: 5 RGBA pixels take 64 bytes.
        assert_eq!(Image::packed(Format::Rgba8888, 5, 2).staged_stride(), 64);
        assert_eq!(Image::packed(Format::L8, 5, 2).staged_stride(), 16);
        assert_eq!(blend(BlendMode::Src, true).unwrap(), BLEND_NONE);
        assert!(blend(BlendMode::DstOver, true).is_err());
        assert_eq!(color([1, 2, 3, 4]), 0x0403_0201);
    }

    #[test]
    fn inverse_and_point_mapping_follow_the_library() {
        let half = affine(0.5, 0.0, 0.0, 0.0, 0.5, 0.0);
        let inv = inverse(&half).unwrap();
        assert_eq!(inv.m[0][0], 2.0);
        assert_eq!(inv.m[1][1], 2.0);
        assert!(inverse(&affine(0.0, 0.0, 0.0, 0.0, 0.0, 0.0)).is_none());
        // (x * 2) + 0.5 truncated: the centre of a 2x2 block lands on pixel 1.
        assert_eq!(map_point(&inv, 0.5, 0.5, Sampling::Point), Some((1, 1)));
        // A point behind the projection plane is refused.
        let behind = Matrix {
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]],
        };
        assert_eq!(map_point(&behind, 1.0, 1.0, Sampling::Point), None);
    }
}
