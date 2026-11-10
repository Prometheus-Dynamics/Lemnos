//! The engine's OSD and border modes: images blended into a frame and rectangles drawn on it, in
//! place. This module builds the `nonai-2d` driver's controls for them, checked; [`Device::draw`]
//! hands them to the node.
//!
//! [`Device::draw`]: crate::Device::draw
//!
//! # What reaches the engine, and what checks it
//!
//! The SDK driver's controls (`V4L2_CID_USER_NONAI2D_OSD_ATTR`, `..._BORDER_ATTR`,
//! `..._MODE`) carry no address the engine is given:
//!
//! - An OSD image travels as a user pointer in the control. The kernel copies `width * height *
//!   bytes per pixel` bytes from it into memory it allocates itself (`dma_alloc_coherent`) and
//!   gives the engine that memory's address. A [`Region`] borrows the image and is refused if
//!   the image is shorter than that copy, so the kernel reads only bytes the caller lent.
//! - The frame drawn on is the capture buffer, a dma-buf the kernel's `vb2_dma_contig` imports
//!   and checks against the format's plane sizes; the engine is given its own address.
//! - What is left is geometry, and the kernel checks none of it: where a region or border lies
//!   in the frame. The engine writes the frame in place at those coordinates with the frame's
//!   stride, so a rectangle past the frame's edge would be written past the buffer. Every
//!   rectangle here is checked to lie inside the frame, a border with its line width of margin
//!   on every side (whether the engine draws the line inside, across or outside the rectangle,
//!   it stays in the frame).
//!
//! And three faults of the kernel driver are kept clear of (a fourth cannot be, below):
//!
//! - It programs region `i` when any region at `i` or above is valid (`osd_valid >> i`), so a
//!   gap would program a region it released, at address 0. Valid regions here are always the
//!   first ones, with no gap.
//! - It never clears a border once set on an open node (`border_valid` only grows), and the same
//!   test programs every border below the highest set. Borders here are always set from index 0,
//!   and [`Device::draw`] refuses fewer borders than the node already has; reopening clears them.
//! - In place, it finds the frame's layout by pixel format and falls back to NV12 for one it does
//!   not know, which for `ABGR32` and `XBGR32` would write a second plane at address 0. Those
//!   frames are refused ([`OverlayError::FrameFormat`]).
//!
//! What cannot be kept clear of from here: every open of the node shares the kernel's one channel
//! state (`find_dev_nun` never marks a channel taken), so another process that opens the node
//! resets the mode, regions and borders, and its close frees the OSD images the engine reads.
//! What that process draws is its own and unchecked. The node must be used by this driver alone;
//! the kernel does not enforce it.
//!
//! Interface facts from `k230_linux_sdk` at `fc6cc2151d54cb307be47f81815023f7ffb0eae6`,
//! `buildroot-overlay/package/nonai2d/src/`: `nonai2d-v4l2-controls.h` (the control ids, modes,
//! orders and attribute structures), `nonai_2d_dev.c` (`nonai_2d_s_ctrl`, the format table,
//! `nonai_2d_buf_queue`, `device_run`, `nonai_2d_job_ready`, `isr_2d`), `nonai_2d.c`
//! (`calc_video_line_stride`, `calc_osd_line_stride`, `get_video_fmt`, `nonai_2d_cfg_osd_attr`,
//! `nonai_2d_cfg_border_attr`, `nonai_2d_cfg_hw`) and `nonai_2d_reg.c` (`nonai_2d_main_cfg`).
//! No code copied.

use core::fmt;

use crate::layout::{Layout, PixelFormat};

/// `V4L2_CID_USER_BASE`.
const CID_USER_BASE: u32 = 0x0098_0900;
/// `V4L2_CID_USER_NONAI2D_BASE`.
const CID_NONAI2D_BASE: u32 = CID_USER_BASE + 0x1200;
/// `V4L2_CID_USER_NONAI2D_MODE`: an integer, one of [`Mode`].
pub const CID_MODE: u32 = CID_NONAI2D_BASE;
/// `V4L2_CID_USER_NONAI2D_COLOR_GAMUT`. Not used.
pub const CID_COLOR_GAMUT: u32 = CID_NONAI2D_BASE + 1;
/// `V4L2_CID_USER_NONAI2D_COEFF`. Not used.
pub const CID_COEFF: u32 = CID_NONAI2D_BASE + 2;
/// `V4L2_CID_USER_NONAI2D_OSD_ATTR`: an array of [`REGIONS`] OSD attributes.
pub const CID_OSD_ATTR: u32 = CID_NONAI2D_BASE + 3;
/// `V4L2_CID_USER_NONAI2D_BORDER_ATTR`: one border attribute.
pub const CID_BORDER_ATTR: u32 = CID_NONAI2D_BASE + 4;

/// OSD regions the engine has (`NONAI2D_OSD_REGION_NUM`).
pub const REGIONS: usize = 8;
/// Borders the engine has (`NONAI2D_BORDER_NUM`).
pub const BORDERS: usize = 32;
/// Bytes of one `nonai2d_osd_attr` in the kernel's 64-bit layout.
pub const REGION_BYTES: usize = 40;
/// Bytes of the OSD control: every region's attributes.
pub const OSD_CONTROL_BYTES: usize = REGION_BYTES * REGIONS;
/// Bytes of one `nonai2d_border_attr`, the border control.
pub const BORDER_BYTES: usize = 16;
/// Colours are `(R << 16) | (G << 8) | B`.
pub const MAX_COLOR: u32 = 0x00FF_FFFF;

/// What the engine does with the next frame (`enum nonai2d_mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Converts between formats: what [`Device::convert`](crate::Device::convert) runs.
    Convert = 0,
    /// Blends the OSD regions into the frame.
    Osd = 1,
    /// Draws the borders on the frame.
    Border = 2,
    /// Both, OSD first.
    OsdBorder = 3,
}

/// An OSD image's pixel format: those the driver's table gives an OSD format of their own.
///
/// `ABGR32`, `XBGR32` and `BGR24` are accepted by the driver but given the format of `ARGB32`,
/// `XRGB32` and `RGB24`, and the planar formats an OSD format of none (the line stride is then
/// left unset), so they are not offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsdFormat {
    /// `BA24` (`ARGB32`).
    Argb32,
    /// `BX24` (`XRGB32`).
    Xrgb32,
    /// `RGB3` (`RGB24`).
    Rgb24,
    /// `AR12` (`ARGB444`).
    Argb444,
    /// `AR15` (`ARGB555`).
    Argb555,
    /// `RGBP` (`RGB565`).
    Rgb565,
}

impl OsdFormat {
    /// The V4L2 fourcc the control names it by.
    #[must_use]
    pub const fn fourcc(self) -> u32 {
        let code = match self {
            Self::Argb32 => b"BA24",
            Self::Xrgb32 => b"BX24",
            Self::Rgb24 => b"RGB3",
            Self::Argb444 => b"AR12",
            Self::Argb555 => b"AR15",
            Self::Rgb565 => b"RGBP",
        };
        u32::from_le_bytes(*code)
    }

    /// Bytes a pixel takes: the driver's `DIV_ROUND_UP(depth, 8)`, which is also the engine's
    /// line stride per pixel for the format.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Argb32 | Self::Xrgb32 => 4,
            Self::Rgb24 => 3,
            Self::Argb444 | Self::Argb555 | Self::Rgb565 => 2,
        }
    }
}

/// The order a region's layers are stacked in, bottom to top (`nonai2d_osd_order`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    VideoOsd = 0,
    OsdVideo = 1,
    VideoBackground = 2,
    BackgroundVideo = 3,
    VideoBackgroundOsd = 4,
    VideoOsdBackground = 5,
    BackgroundVideoOsd = 6,
    BackgroundOsdVideo = 7,
    OsdVideoBackground = 8,
    OsdBackgroundVideo = 9,
}

/// A rectangle in a frame, in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// One OSD region: an image placed in the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region<'a> {
    /// Where in the frame; the image is this size.
    pub rect: Rect,
    pub format: OsdFormat,
    /// The image, `rect.width * rect.height` pixels of `format`, row after row with no padding.
    /// Borrowed until the control is set; the kernel copies it.
    pub image: &'a [u8],
    pub osd_alpha: u8,
    pub video_alpha: u8,
    pub background_alpha: u8,
    /// The region's background colour.
    pub background: u32,
    pub order: Order,
}

/// One border: a rectangle's outline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Border {
    pub rect: Rect,
    /// The line's width in pixels.
    pub line_width: u16,
    pub color: u32,
}

/// Why an overlay, a region or a border is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayError {
    /// The frame's format is one the driver mistakes in place (`ABGR32`, `XBGR32`).
    FrameFormat(PixelFormat),
    /// A rectangle with no area.
    Empty,
    /// A coordinate or size that is odd: the 4:2:0 frames' chroma has half the resolution.
    Odd,
    /// A rectangle not inside the frame, with its margin.
    Outside,
    /// An image shorter than its rectangle at its format.
    Image { have: usize, need: usize },
    /// A border line of width 0, or wider than half the rectangle.
    Line,
    /// A colour past 24 bits.
    Color,
    /// All [`REGIONS`] or [`BORDERS`] are taken.
    Full,
}

impl fmt::Display for OverlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameFormat(format) => {
                write!(
                    f,
                    "the driver draws on {format:?} frames with the wrong layout"
                )
            }
            Self::Empty => f.write_str("a rectangle with no area"),
            Self::Odd => f.write_str("rectangles take even coordinates and sizes"),
            Self::Outside => f.write_str("a rectangle not inside the frame"),
            Self::Image { have, need } => {
                write!(f, "an image of {have} bytes for a region of {need}")
            }
            Self::Line => f.write_str("a border line of width 0 or over half the rectangle"),
            Self::Color => f.write_str("a colour past 0xffffff"),
            Self::Full => f.write_str("every region or border is taken"),
        }
    }
}

impl core::error::Error for OverlayError {}

/// Checks that `rect` lies inside `layout`'s frame with `margin` pixels to spare on each side.
fn inside(rect: Rect, layout: Layout, margin: u16) -> Result<(), OverlayError> {
    if rect.width == 0 || rect.height == 0 {
        return Err(OverlayError::Empty);
    }
    if [rect.x, rect.y, rect.width, rect.height, margin]
        .iter()
        .any(|v| v % 2 != 0)
    {
        return Err(OverlayError::Odd);
    }
    let (width, height) = layout.size();
    let (x, y, w, h, m) = (
        u32::from(rect.x),
        u32::from(rect.y),
        u32::from(rect.width),
        u32::from(rect.height),
        u32::from(margin),
    );
    // At most 4 * 0xffff: no overflow.
    if x < m || y < m || x + w + m > width || y + h + m > height {
        return Err(OverlayError::Outside);
    }
    Ok(())
}

impl Region<'_> {
    /// The bytes the kernel copies for this region.
    #[must_use]
    pub fn image_bytes(&self) -> usize {
        usize::from(self.rect.width)
            * usize::from(self.rect.height)
            * self.format.bytes_per_pixel() as usize
    }

    fn check(&self, layout: Layout) -> Result<(), OverlayError> {
        inside(self.rect, layout, 0)?;
        let need = self.image_bytes();
        if self.image.len() < need {
            return Err(OverlayError::Image {
                have: self.image.len(),
                need,
            });
        }
        if self.background > MAX_COLOR {
            return Err(OverlayError::Color);
        }
        Ok(())
    }

    /// The region's `nonai2d_osd_attr` at `index`, valid.
    fn encode(&self, index: usize, out: &mut [u8]) {
        let r = self.rect;
        // The index fits: below REGIONS.
        out[0] = index as u8;
        out[1] = 1;
        out[2..4].copy_from_slice(&r.width.to_le_bytes());
        out[4..6].copy_from_slice(&r.height.to_le_bytes());
        out[6..8].copy_from_slice(&r.x.to_le_bytes());
        out[8..10].copy_from_slice(&r.y.to_le_bytes());
        // The pointer the kernel copies the image from. It escapes to the kernel, which reads
        // `image_bytes()` from it while the control is set, and `check` made sure the image
        // holds that many. The kernel's pointer field is 64 bits whatever ours are.
        let data = self.image.as_ptr().expose_provenance() as u64;
        out[16..24].copy_from_slice(&data.to_le_bytes());
        out[24] = self.background_alpha;
        out[25] = self.osd_alpha;
        out[26] = self.video_alpha;
        out[28..32].copy_from_slice(&(self.order as u32).to_le_bytes());
        out[32..36].copy_from_slice(&self.background.to_le_bytes());
        out[36..40].copy_from_slice(&self.format.fourcc().to_le_bytes());
    }
}

impl Border {
    fn check(&self, layout: Layout) -> Result<(), OverlayError> {
        let line = self.line_width;
        if line == 0 || u32::from(line) * 2 > u32::from(self.rect.width.min(self.rect.height)) {
            return Err(OverlayError::Line);
        }
        inside(self.rect, layout, line)?;
        if self.color > MAX_COLOR {
            return Err(OverlayError::Color);
        }
        Ok(())
    }

    /// The border's `nonai2d_border_attr` at `index`.
    fn encode(&self, index: usize) -> [u8; BORDER_BYTES] {
        let r = self.rect;
        let mut out = [0u8; BORDER_BYTES];
        // The index fits: below BORDERS.
        out[0] = index as u8;
        out[2..4].copy_from_slice(&r.width.to_le_bytes());
        out[4..6].copy_from_slice(&r.height.to_le_bytes());
        out[6..8].copy_from_slice(&self.line_width.to_le_bytes());
        out[8..12].copy_from_slice(&self.color.to_le_bytes());
        out[12..14].copy_from_slice(&r.x.to_le_bytes());
        out[14..16].copy_from_slice(&r.y.to_le_bytes());
        out
    }
}

/// The regions and borders to draw on frames of one layout, each checked against it.
#[derive(Clone, Copy, Debug)]
pub struct Overlay<'a> {
    layout: Layout,
    regions: [Option<Region<'a>>; REGIONS],
    region_count: usize,
    borders: [Border; BORDERS],
    border_count: usize,
}

impl<'a> Overlay<'a> {
    /// Nothing yet, to draw on frames of `layout`. Refused for a frame format the driver
    /// mistakes in place.
    pub fn new(layout: Layout) -> Result<Self, OverlayError> {
        let format = layout.format();
        if matches!(format, PixelFormat::Abgr32 | PixelFormat::Xbgr32) {
            return Err(OverlayError::FrameFormat(format));
        }
        Ok(Self {
            layout,
            regions: [None; REGIONS],
            region_count: 0,
            borders: [Border::default(); BORDERS],
            border_count: 0,
        })
    }

    /// The layout the overlay is for.
    #[must_use]
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Adds `region` after the others, checked.
    pub fn add_region(&mut self, region: Region<'a>) -> Result<(), OverlayError> {
        region.check(self.layout)?;
        let slot = self
            .regions
            .get_mut(self.region_count)
            .ok_or(OverlayError::Full)?;
        *slot = Some(region);
        self.region_count += 1;
        Ok(())
    }

    /// Adds `border` after the others, checked.
    pub fn add_border(&mut self, border: Border) -> Result<(), OverlayError> {
        border.check(self.layout)?;
        let slot = self
            .borders
            .get_mut(self.border_count)
            .ok_or(OverlayError::Full)?;
        *slot = border;
        self.border_count += 1;
        Ok(())
    }

    /// The regions, in order.
    pub fn regions(&self) -> impl Iterator<Item = &Region<'a>> {
        self.regions.iter().flatten()
    }

    /// The borders, in order.
    #[must_use]
    pub fn borders(&self) -> &[Border] {
        &self.borders[..self.border_count]
    }

    /// The mode that draws it, or `None` when there is nothing to draw.
    #[must_use]
    pub fn mode(&self) -> Option<Mode> {
        match (self.region_count > 0, self.border_count > 0) {
            (true, true) => Some(Mode::OsdBorder),
            (true, false) => Some(Mode::Osd),
            (false, true) => Some(Mode::Border),
            (false, false) => None,
        }
    }

    /// The OSD control: the regions valid from index 0 on with no gap, the rest invalid (which
    /// makes the kernel release their memory). Its pointers are this overlay's images, so it is
    /// set while the overlay is borrowed.
    #[must_use]
    pub fn osd_control(&self) -> [u8; OSD_CONTROL_BYTES] {
        let mut out = [0u8; OSD_CONTROL_BYTES];
        for (index, chunk) in out.as_chunks_mut::<REGION_BYTES>().0.iter_mut().enumerate() {
            match &self.regions[index] {
                Some(region) => region.encode(index, chunk),
                // Invalid: only the index.
                None => chunk[0] = index as u8,
            }
        }
        out
    }

    /// The border control for border `index`, or `None` past the last.
    #[must_use]
    pub fn border_control(&self, index: usize) -> Option<[u8; BORDER_BYTES]> {
        self.borders().get(index).map(|b| b.encode(index))
    }
}

#[cfg(test)]
mod tests;
