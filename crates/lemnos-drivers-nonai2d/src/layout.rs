//! Formats and plane layouts as the `nonai-2d` driver computes them
//! (`nonai_2d_fill_plane_layout`).

use std::fmt;

/// The largest width the driver keeps.
pub const MAX_WIDTH: u32 = 1920;
/// The largest height the driver keeps.
pub const MAX_HEIGHT: u32 = 1080;

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
    /// `RGBP` (`RGB565`).
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
    /// The V4L2 fourcc.
    pub fn fourcc(self) -> u32 {
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

    fn is_subsampled(self) -> bool {
        matches!(self, Self::Nv12 | Self::Nv21 | Self::Yuv420 | Self::Yvu420)
    }
}

/// One plane of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plane {
    /// Bytes per line.
    pub stride: u32,
    /// Bytes in the plane.
    pub size: u32,
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

impl std::error::Error for LayoutError {}

/// A checked frame layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    format: PixelFormat,
    width: u32,
    height: u32,
}

impl Layout {
    /// A layout, if the driver would keep it unchanged.
    pub fn new(format: PixelFormat, width: u32, height: u32) -> Result<Self, LayoutError> {
        let fits = (1..=MAX_WIDTH).contains(&width) && (1..=MAX_HEIGHT).contains(&height);
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
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// The size in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The frame's planes, one dma-buf each.
    pub fn planes(&self) -> Vec<Plane> {
        let (w, h) = (self.width, self.height);
        let packed = |bytes: u32| {
            vec![Plane {
                stride: w * bytes,
                size: w * h * bytes,
            }]
        };
        let luma = Plane {
            stride: w,
            size: w * h,
        };
        match self.format {
            PixelFormat::Argb32
            | PixelFormat::Xrgb32
            | PixelFormat::Abgr32
            | PixelFormat::Xbgr32 => packed(4),
            PixelFormat::Rgb24 | PixelFormat::Bgr24 => packed(3),
            PixelFormat::Rgb565 => packed(2),
            PixelFormat::Nv12 | PixelFormat::Nv21 => vec![
                luma,
                Plane {
                    stride: w,
                    size: w * h / 2,
                },
            ],
            PixelFormat::Yuv420 | PixelFormat::Yvu420 => {
                let chroma = Plane {
                    stride: w / 2,
                    size: w * h / 4,
                };
                vec![luma, chroma, chroma]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planes_follow_the_driver() {
        let planes = |f, w, h| Layout::new(f, w, h).expect("layout").planes();
        assert_eq!(
            planes(PixelFormat::Argb32, 64, 8),
            [Plane {
                stride: 256,
                size: 2048
            }]
        );
        assert_eq!(
            planes(PixelFormat::Nv12, 64, 8),
            [
                Plane {
                    stride: 64,
                    size: 512
                },
                Plane {
                    stride: 64,
                    size: 256
                }
            ]
        );
        assert_eq!(
            planes(PixelFormat::Yvu420, 64, 8),
            [
                Plane {
                    stride: 64,
                    size: 512
                },
                Plane {
                    stride: 32,
                    size: 128
                },
                Plane {
                    stride: 32,
                    size: 128
                }
            ]
        );
    }

    #[test]
    fn sizes_the_driver_would_change_are_refused() {
        assert!(Layout::new(PixelFormat::Rgb24, MAX_WIDTH, MAX_HEIGHT).is_ok());
        assert!(Layout::new(PixelFormat::Rgb24, MAX_WIDTH + 1, 8).is_err());
        assert!(Layout::new(PixelFormat::Rgb24, 8, MAX_HEIGHT + 1).is_err());
        assert!(Layout::new(PixelFormat::Rgb24, 0, 8).is_err());
        assert!(Layout::new(PixelFormat::Nv12, 63, 8).is_err());
        assert!(Layout::new(PixelFormat::Yuv420, 64, 7).is_err());
        assert!(Layout::new(PixelFormat::Rgb24, 63, 7).is_ok());
    }

    #[test]
    fn the_fourccs_are_the_kernels() {
        assert_eq!(PixelFormat::Nv12.fourcc(), 0x3231_564e);
        assert_eq!(PixelFormat::Abgr32.fourcc(), 0x3432_5241);
    }
}
