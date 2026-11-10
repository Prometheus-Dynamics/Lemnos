//! Formats and plane layouts as the `nonai-2d` driver computes them
//! (`nonai_2d_fill_plane_layout` in `nonai_2d_dev.c`; see the crate docs for the source).
//!
//! The driver works the strides and plane sizes out from the width and height alone and ignores
//! any it is given, so a layout here is only a format and a size: a row is never padded.

use core::fmt;
use core::ops::Deref;

/// The largest width the driver keeps (`MAX_WIDTH`); a wider one it clamps.
pub const MAX_WIDTH: u32 = 1920;
/// The largest height the driver keeps (`MAX_HEIGHT`); a taller one it clamps.
pub const MAX_HEIGHT: u32 = 1080;
/// The most memory planes a frame has (the driver's `addr[3]`).
pub const MAX_PLANES: usize = 3;

/// A pixel format the driver converts. The comment gives the order in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PixelFormat {
    /// `BA24` (`ARGB32`): A, R, G, B.
    Argb32,
    /// `BX24` (`XRGB32`): X, R, G, B.
    Xrgb32,
    /// `AR24` (`ABGR32`): B, G, R, A.
    Abgr32,
    /// `XR24` (`XBGR32`): B, G, R, X.
    Xbgr32,
    /// `RGB3`: R, G, B.
    Rgb24,
    /// `BGR3`: B, G, R.
    Bgr24,
    /// `RGBP` (`RGB565`): 16 bits a pixel, little-endian, red in the top five.
    Rgb565,
    /// `NV12`, two planes: Y, then interleaved U, V.
    Nv12,
    /// `NV21`, two planes: Y, then interleaved V, U.
    Nv21,
    /// `YU12`, three planes: Y, U, V.
    Yuv420,
    /// `YV12`, three planes: Y, V, U.
    Yvu420,
}

impl PixelFormat {
    /// Every format, in the order above.
    pub const ALL: [Self; 11] = [
        Self::Argb32,
        Self::Xrgb32,
        Self::Abgr32,
        Self::Xbgr32,
        Self::Rgb24,
        Self::Bgr24,
        Self::Rgb565,
        Self::Nv12,
        Self::Nv21,
        Self::Yuv420,
        Self::Yvu420,
    ];

    /// The V4L2 fourcc.
    pub const fn fourcc(self) -> u32 {
        let code = match self {
            Self::Argb32 => b"BA24",
            Self::Xrgb32 => b"BX24",
            Self::Abgr32 => b"AR24",
            Self::Xbgr32 => b"XR24",
            Self::Rgb24 => b"RGB3",
            Self::Bgr24 => b"BGR3",
            Self::Rgb565 => b"RGBP",
            Self::Nv12 => b"NV12",
            Self::Nv21 => b"NV21",
            Self::Yuv420 => b"YU12",
            Self::Yvu420 => b"YV12",
        };
        u32::from_le_bytes(*code)
    }

    /// Bytes a pixel takes, for a format of one plane; `None` for the 4:2:0 formats.
    pub const fn packed_bytes(self) -> Option<u32> {
        match self {
            Self::Argb32 | Self::Xrgb32 | Self::Abgr32 | Self::Xbgr32 => Some(4),
            Self::Rgb24 | Self::Bgr24 => Some(3),
            Self::Rgb565 => Some(2),
            Self::Nv12 | Self::Nv21 | Self::Yuv420 | Self::Yvu420 => None,
        }
    }

    /// The memory planes a frame has, one dma-buf each.
    pub const fn plane_count(self) -> usize {
        match self {
            Self::Nv12 | Self::Nv21 => 2,
            Self::Yuv420 | Self::Yvu420 => 3,
            _ => 1,
        }
    }

    const fn is_subsampled(self) -> bool {
        self.packed_bytes().is_none()
    }
}

/// One plane of a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Plane {
    /// Bytes per line.
    pub stride: u32,
    /// Bytes in the plane.
    pub size: u32,
}

/// A frame's planes, held inline. Derefs to the planes as a slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Planes {
    len: usize,
    items: [Plane; MAX_PLANES],
}

impl Deref for Planes {
    type Target = [Plane];

    fn deref(&self) -> &[Plane] {
        &self.items[..self.len]
    }
}

/// Why a layout is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutError {
    /// The refused format.
    pub format: PixelFormat,
    /// The refused width.
    pub width: u32,
    /// The refused height.
    pub height: u32,
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} at {}x{} is not a size the engine takes (non-empty, at most {MAX_WIDTH}x{MAX_HEIGHT}, even for 4:2:0)",
            self.format, self.width, self.height
        )
    }
}

impl core::error::Error for LayoutError {}

/// A checked frame layout: one the driver keeps unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Layout {
    format: PixelFormat,
    width: u32,
    height: u32,
}

impl Layout {
    /// A layout, if the driver would keep it unchanged.
    pub const fn new(format: PixelFormat, width: u32, height: u32) -> Result<Self, LayoutError> {
        let fits = width >= 1 && width <= MAX_WIDTH && height >= 1 && height <= MAX_HEIGHT;
        let even = !format.is_subsampled() || (width.is_multiple_of(2) && height.is_multiple_of(2));
        if fits && even {
            Ok(Self {
                format,
                width,
                height,
            })
        } else {
            Err(LayoutError {
                format,
                width,
                height,
            })
        }
    }

    /// The pixel format.
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    /// The size in pixels.
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The frame's planes, one dma-buf each. No product overflows: the size is at most
    /// 1920x1080 at four bytes a pixel.
    pub fn planes(&self) -> Planes {
        let (w, h) = (self.width, self.height);
        let mut items = [Plane::default(); MAX_PLANES];
        let luma = Plane {
            stride: w,
            size: w * h,
        };
        let len = match self.format.packed_bytes() {
            Some(bytes) => {
                items[0] = Plane {
                    stride: w * bytes,
                    size: w * h * bytes,
                };
                1
            }
            None if self.format.plane_count() == 2 => {
                items[0] = luma;
                items[1] = Plane {
                    stride: w,
                    size: w * h / 2,
                };
                2
            }
            None => {
                let chroma = Plane {
                    stride: w / 2,
                    size: w * h / 4,
                };
                items = [luma, chroma, chroma];
                3
            }
        };
        Planes { len, items }
    }

    /// Bytes in the whole frame, every plane together.
    pub fn bytes(&self) -> u32 {
        self.planes().iter().map(|p| p.size).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planes(f: PixelFormat, w: u32, h: u32) -> Planes {
        Layout::new(f, w, h).expect("layout").planes()
    }

    const fn plane(stride: u32, size: u32) -> Plane {
        Plane { stride, size }
    }

    #[test]
    fn planes_follow_the_driver() {
        assert_eq!(*planes(PixelFormat::Argb32, 64, 8), [plane(256, 2048)]);
        assert_eq!(*planes(PixelFormat::Rgb24, 64, 8), [plane(192, 1536)]);
        assert_eq!(*planes(PixelFormat::Rgb565, 64, 8), [plane(128, 1024)]);
        assert_eq!(
            *planes(PixelFormat::Nv12, 64, 8),
            [plane(64, 512), plane(64, 256)]
        );
        assert_eq!(
            *planes(PixelFormat::Yvu420, 64, 8),
            [plane(64, 512), plane(32, 128), plane(32, 128)]
        );
        assert_eq!(
            Layout::new(PixelFormat::Nv21, 64, 8)
                .expect("layout")
                .bytes(),
            768
        );
    }

    #[test]
    fn every_format_has_its_plane_count_and_a_distinct_fourcc() {
        for (i, format) in PixelFormat::ALL.iter().enumerate() {
            let layout = Layout::new(*format, 16, 16).expect("layout");
            assert_eq!(layout.planes().len(), format.plane_count(), "{format:?}");
            assert!(
                PixelFormat::ALL[..i]
                    .iter()
                    .all(|other| other.fourcc() != format.fourcc())
            );
        }
    }

    #[test]
    fn the_largest_frame_does_not_overflow() {
        let layout = Layout::new(PixelFormat::Argb32, MAX_WIDTH, MAX_HEIGHT).expect("layout");
        assert_eq!(layout.bytes(), 1920 * 1080 * 4);
    }

    #[test]
    fn sizes_the_driver_would_change_are_refused() {
        assert!(Layout::new(PixelFormat::Rgb24, MAX_WIDTH, MAX_HEIGHT).is_ok());
        assert!(Layout::new(PixelFormat::Rgb24, MAX_WIDTH + 1, 8).is_err());
        assert!(Layout::new(PixelFormat::Rgb24, 8, MAX_HEIGHT + 1).is_err());
        assert!(Layout::new(PixelFormat::Rgb24, 0, 8).is_err());
        assert!(Layout::new(PixelFormat::Rgb24, 8, 0).is_err());
        assert!(Layout::new(PixelFormat::Nv12, 63, 8).is_err());
        assert!(Layout::new(PixelFormat::Yuv420, 64, 7).is_err());
        assert!(Layout::new(PixelFormat::Rgb24, 63, 7).is_ok());
        assert!(Layout::new(PixelFormat::Rgb24, u32::MAX, u32::MAX).is_err());
    }

    #[test]
    fn the_fourccs_are_the_kernels() {
        assert_eq!(PixelFormat::Nv12.fourcc(), 0x3231_564e);
        assert_eq!(PixelFormat::Abgr32.fourcc(), 0x3432_5241);
        assert_eq!(PixelFormat::Rgb565.fourcc(), 0x5042_4752);
    }
}
