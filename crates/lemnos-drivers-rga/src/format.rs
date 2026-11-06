//! The pixel formats, layouts and rectangles the RGA routes take. Plain data with no OS calls,
//! readable on any target.

/// A pixel format the engine's V4L2 route names (`V4L2_PIX_FMT_*`). The comment gives the byte
/// order in memory.
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
    /// The V4L2 fourcc.
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

    /// Bytes per pixel of the first plane.
    pub fn bytes(self) -> u32 {
        match self {
            PixelFormat::Rgb24 | PixelFormat::Bgr24 => 3,
            PixelFormat::Abgr32 | PixelFormat::Xbgr32 => 4,
            PixelFormat::Grey | PixelFormat::Nv12 | PixelFormat::Nv21 | PixelFormat::Yuv420 => 1,
        }
    }

    /// Bytes of a `height`-row frame with `stride`-byte rows (all planes).
    pub fn frame_len(self, stride: u32, height: u32) -> usize {
        let luma = stride as usize * height as usize;
        match self {
            PixelFormat::Nv12 | PixelFormat::Nv21 | PixelFormat::Yuv420 => {
                luma + stride as usize * height.div_ceil(2) as usize
            }
            _ => luma,
        }
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
    /// Tightly packed rows.
    pub fn packed(format: PixelFormat, width: u32, height: u32) -> Self {
        Self {
            format,
            width,
            height,
            stride: width * format.bytes(),
        }
    }

    /// Bytes of a frame in this layout.
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
