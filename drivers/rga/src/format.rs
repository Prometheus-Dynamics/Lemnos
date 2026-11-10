//! The pixel formats, layouts and rectangles the routes take. Plain data with no OS calls,
//! readable on any target.

/// A pixel format (`V4L2_PIX_FMT_*`). The comment gives the byte order in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PixelFormat {
    /// `RGB3`: R, G, B.
    Rgb24,
    /// `BGR3`: B, G, R.
    Bgr24,
    /// `AR24` (`ABGR32`): B, G, R, A.
    Abgr32,
    /// `XR24` (`XBGR32`): B, G, R, X.
    Xbgr32,
    /// `GREY`: one luma plane. The mainline driver has no gray format.
    Grey,
    /// `NV12`: Y plane, then interleaved U, V at half resolution (one buffer).
    Nv12,
    /// `NV21`: as NV12 with V, U.
    Nv21,
    /// `YU12` (I420): Y, U, V planes (one buffer).
    Yuv420,
}

impl PixelFormat {
    /// Every format, in the order the image contracts list them.
    pub const ALL: [PixelFormat; 8] = [
        PixelFormat::Rgb24,
        PixelFormat::Bgr24,
        PixelFormat::Abgr32,
        PixelFormat::Xbgr32,
        PixelFormat::Grey,
        PixelFormat::Nv12,
        PixelFormat::Nv21,
        PixelFormat::Yuv420,
    ];

    /// The V4L2 fourcc, little-endian.
    pub fn fourcc(self) -> u32 {
        let code = match self {
            PixelFormat::Rgb24 => *b"RGB3",
            PixelFormat::Bgr24 => *b"BGR3",
            PixelFormat::Abgr32 => *b"AR24",
            PixelFormat::Xbgr32 => *b"XR24",
            PixelFormat::Grey => *b"GREY",
            PixelFormat::Nv12 => *b"NV12",
            PixelFormat::Nv21 => *b"NV21",
            PixelFormat::Yuv420 => *b"YU12",
        };
        u32::from_le_bytes(code)
    }

    /// The format of a fourcc, if it is one of these.
    pub fn from_fourcc(fourcc: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.fourcc() == fourcc)
    }

    /// Bytes per pixel of the first plane.
    pub fn bytes(self) -> u32 {
        match self {
            PixelFormat::Rgb24 | PixelFormat::Bgr24 => 3,
            PixelFormat::Abgr32 | PixelFormat::Xbgr32 => 4,
            PixelFormat::Grey | PixelFormat::Nv12 | PixelFormat::Nv21 | PixelFormat::Yuv420 => 1,
        }
    }

    /// Bytes of a `height`-row frame with `stride`-byte rows (all planes). Saturates at
    /// `usize::MAX`, so a size too large to hold is never mistaken for a small one.
    pub fn frame_len(self, stride: u32, height: u32) -> usize {
        let rows = match self {
            PixelFormat::Nv12 | PixelFormat::Nv21 | PixelFormat::Yuv420 => {
                u64::from(height) + u64::from(height.div_ceil(2))
            }
            _ => u64::from(height),
        };
        u64::from(stride)
            .checked_mul(rows)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(usize::MAX)
    }
}

/// One side of a conversion: pixel format, size and row stride.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Layout {
    pub format: PixelFormat,
    pub width: u32,
    pub height: u32,
    /// Bytes per row of the first plane.
    pub stride: u32,
}

impl Layout {
    /// Tightly packed rows. A row too long for a `u32` stride saturates, and every route
    /// refuses the layout.
    pub fn packed(format: PixelFormat, width: u32, height: u32) -> Self {
        Self {
            format,
            width,
            height,
            stride: width.saturating_mul(format.bytes()),
        }
    }

    /// Bytes of a frame in this layout (saturating, see [`PixelFormat::frame_len`]).
    pub fn frame_len(&self) -> usize {
        self.format.frame_len(self.stride, self.height)
    }
}

/// A source rectangle to convert from (the rest of the frame is ignored).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_lengths_count_every_plane_and_never_wrap() {
        assert_eq!(
            Layout::packed(PixelFormat::Nv12, 4, 3).frame_len(),
            4 * 3 + 4 * 2
        );
        assert_eq!(Layout::packed(PixelFormat::Abgr32, 4, 3).frame_len(), 48);
        let huge = PixelFormat::Yuv420.frame_len(u32::MAX, u32::MAX);
        assert!(huge >= usize::try_from(u64::from(u32::MAX)).unwrap());
        assert_eq!(
            Layout::packed(PixelFormat::Abgr32, u32::MAX, 1).stride,
            u32::MAX
        );
        for f in PixelFormat::ALL {
            assert_eq!(PixelFormat::from_fourcc(f.fourcc()), Some(f));
        }
        assert_eq!(PixelFormat::from_fourcc(0), None);
    }
}
