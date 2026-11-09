//! The Allwinner BSP's G2D 2D engine, `/dev/g2d`: the `G2D_CMD_*_H` requests and their structures.
//!
//! The structures are transcribed from `include/uapi/linux/sunxi-g2d.h` of the Radxa
//! `allwinner-bsp` tree, branch `cubie-aiot-v1.4.6` (`GPL-2.0-or-later WITH Linux-syscall-note`),
//! with the C compiler's sizes and offsets on LP64 pinned by the tests below.
//!
//! The kernel driver does not compare an image's size with its dma-buf, and an image can name a
//! physical address (`use_phy_addr`). So nothing here lets a caller fill in a request: a [`Job`]
//! is built only from [`Surface`]s, whose frame was checked against the dma-buf's real size, and
//! rectangles checked against the surface. `use_phy_addr` is always zero. The job borrows the
//! surfaces until it is dropped.
//!
//! Written from the driver's source, untested on board.

use std::io;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, BorrowedFd};

use crate::dma_buf;
use crate::ioctl::{ioc, ioctl_ptr};

/// `G2D_CMD_BITBLT_H`: copy, scale and convert, or turn and flip. Takes a [`BltH`].
pub const BITBLT_H: u32 = 0x55;
/// `G2D_CMD_FILLRECT_H`. Takes a [`FillrectH`].
pub const FILLRECT_H: u32 = 0x56;
/// `G2D_CMD_BLD_H`: Porter-Duff blending of two layers. Takes a [`Bld`].
pub const BLD_H: u32 = 0x57;
/// `G2D_CMD_QUERY_VERSION`.
pub const QUERY_VERSION: u32 = ioc(2, b'G', 0x9f, size_of::<HardwareVersion>());

/// The largest width or height of a rectangle. The rotator's size registers hold 13 bits of the
/// size less one (`ROT_ISIZE`), so a larger one would be cut short by the hardware.
pub const MAX_SIZE: u32 = 8192;

/// `g2d_rect`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawRect {
    /// Left.
    pub x: i32,
    /// Top.
    pub y: i32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
}

/// `g2d_image_enh`: one image of a request. Built only by this module.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageEnh {
    /// Nonzero when the image is a buffer; zero makes a layer a solid `color`.
    pub bbuff: i32,
    /// The fill colour.
    pub color: u32,
    /// `g2d_fmt_enh`.
    pub format: u32,
    /// Plane addresses; the kernel sets them from `fd`.
    pub laddr: [u32; 3],
    /// High address bits; zero.
    pub haddr: [u32; 3],
    /// The image's width in pixels, which is also its stride with `align` zero.
    pub width: u32,
    /// The image's height.
    pub height: u32,
    /// Stride alignment of each plane; zero means none.
    pub align: [u32; 3],
    /// The region of interest.
    pub clip_rect: RawRect,
    /// `resize` (`g2d_size`), zero here.
    pub resize: [u32; 2],
    /// `coor` (`g2d_coor`), zero here.
    pub coor: [u32; 2],
    /// `g2d_color_gmt`.
    pub gamut: u32,
    /// Nonzero when alpha is premultiplied.
    pub bpremul: i32,
    /// Plane alpha.
    pub alpha: u8,
    /// Padding of the C structure.
    pub pad: [u8; 3],
    /// `g2d_alpha_mode_enh`.
    pub mode: u32,
    /// The dma-buf descriptor.
    pub fd: i32,
    /// Always zero: the kernel maps `fd`.
    pub use_phy_addr: u32,
    /// `enum color_range`.
    pub color_range: u32,
}

/// `g2d_blt_h`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BltH {
    /// `g2d_blt_flags_h`.
    pub flag_h: u32,
    /// Source.
    pub src: ImageEnh,
    /// Destination.
    pub dst: ImageEnh,
}

/// `g2d_fillrect_h`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FillrectH {
    /// Destination, with its colour.
    pub dst: ImageEnh,
}

/// `g2d_ck`, the colour key; unused here and left zero.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ck {
    /// `match_rule` (a C `bool`) and its padding.
    pub match_rule: [u8; 4],
    /// Largest matching colour.
    pub max_color: u32,
    /// Smallest matching colour.
    pub min_color: u32,
}

/// `g2d_bld`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bld {
    /// `g2d_bld_cmd_flag`.
    pub bld_cmd: u32,
    /// Destination.
    pub dst: ImageEnh,
    /// Sources: the driver reads the first two.
    pub src: [ImageEnh; 4],
    /// Colour key.
    pub ck: Ck,
}

/// `struct g2d_hardware_version`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HardwareVersion {
    /// The engine's version.
    pub g2d_version: u32,
    /// The chip's version.
    pub chip_version: u32,
}

/// A pixel format, with its `g2d_fmt_enh` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
#[non_exhaustive]
pub enum PixelFormat {
    /// `G2D_FORMAT_ARGB8888`.
    Argb8888 = 0x00,
    /// `G2D_FORMAT_ABGR8888`.
    Abgr8888 = 0x01,
    /// `G2D_FORMAT_RGBA8888`.
    Rgba8888 = 0x02,
    /// `G2D_FORMAT_BGRA8888`.
    Bgra8888 = 0x03,
    /// `G2D_FORMAT_XRGB8888`.
    Xrgb8888 = 0x04,
    /// `G2D_FORMAT_XBGR8888`.
    Xbgr8888 = 0x05,
    /// `G2D_FORMAT_RGBX8888`.
    Rgbx8888 = 0x06,
    /// `G2D_FORMAT_BGRX8888`.
    Bgrx8888 = 0x07,
    /// `G2D_FORMAT_RGB888`.
    Rgb888 = 0x08,
    /// `G2D_FORMAT_BGR888`.
    Bgr888 = 0x09,
    /// `G2D_FORMAT_RGB565`.
    Rgb565 = 0x0a,
    /// `G2D_FORMAT_YUV420UVC_V1U1V0U0`: taken as NV12 from the header's naming of the 4:4:4 pair
    /// (`..V1U1V0U0` is NV24 there). Chroma order unverified on board.
    Nv12 = 0x28,
    /// `G2D_FORMAT_YUV420UVC_U1V1U0V0`: NV21, as for [`Self::Nv12`].
    Nv21 = 0x29,
    /// `G2D_FORMAT_YUV420_PLANAR`: Y, U, V planes (`g2d_set_info` puts U first).
    I420 = 0x2a,
    /// `G2D_FORMAT_Y8`.
    Gray8 = 0x30,
}

impl PixelFormat {
    /// Whether this is an RGB format (the blender's second layer must be one).
    pub fn is_rgb(self) -> bool {
        (self as u32) <= Self::Rgb565 as u32
    }

    /// Whether chroma is at half resolution, so positions and sizes must be even.
    fn is_subsampled(self) -> bool {
        matches!(self, Self::Nv12 | Self::Nv21 | Self::I420)
    }

    /// The bytes of a `width` by `height` frame with no stride padding, as the driver computes
    /// its plane addresses and pitches. Saturates, so an impossible size fits no dma-buf.
    pub fn frame_len(self, width: u32, height: u32) -> u64 {
        let pixels = u64::from(width) * u64::from(height);
        match self {
            Self::Argb8888
            | Self::Abgr8888
            | Self::Rgba8888
            | Self::Bgra8888
            | Self::Xrgb8888
            | Self::Xbgr8888
            | Self::Rgbx8888
            | Self::Bgrx8888 => pixels.saturating_mul(4),
            Self::Rgb888 | Self::Bgr888 => pixels.saturating_mul(3),
            Self::Rgb565 => pixels.saturating_mul(2),
            Self::Nv12 | Self::Nv21 | Self::I420 => pixels.saturating_add(pixels / 2),
            Self::Gray8 => pixels,
        }
    }
}

/// A rectangle of a surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    /// Left.
    pub x: u32,
    /// Top.
    pub y: u32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
}

/// The colour space of a conversion (`g2d_color_gmt`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Gamut {
    /// BT.601.
    Bt601 = 0,
    /// BT.709.
    Bt709 = 1,
    /// BT.2020.
    Bt2020 = 2,
}

/// The range of a YUV image (`enum color_range`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ColorRange {
    /// 0 to 255.
    Full = 0,
    /// 16 to 235.
    Limited = 1,
}

/// A quarter turn (`G2D_ROT_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Turn {
    /// None.
    None = 0x400,
    /// 90 degrees.
    Cw90 = 0x100,
    /// 180 degrees.
    Cw180 = 0x200,
    /// 270 degrees.
    Cw270 = 0x300,
}

/// A Porter-Duff blend (`g2d_bld_cmd_flag`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
#[non_exhaustive]
pub enum Blend {
    /// `G2D_BLD_SRCOVER`.
    SrcOver = 4,
    /// `G2D_BLD_DSTOVER`.
    DstOver = 5,
    /// `G2D_BLD_SRCIN`.
    SrcIn = 6,
    /// `G2D_BLD_DSTIN`.
    DstIn = 7,
    /// `G2D_BLD_SRCOUT`.
    SrcOut = 8,
    /// `G2D_BLD_DSTOUT`.
    DstOut = 9,
    /// `G2D_BLD_SRCATOP`.
    SrcAtop = 10,
    /// `G2D_BLD_DSTATOP`.
    DstAtop = 11,
    /// `G2D_BLD_XOR`.
    Xor = 12,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

/// A dma-buf holding one tightly packed frame, checked against the dma-buf's size.
#[derive(Clone, Copy, Debug)]
pub struct Surface<'a> {
    fd: BorrowedFd<'a>,
    format: PixelFormat,
    width: u32,
    height: u32,
}

impl<'a> Surface<'a> {
    /// A surface over the dma-buf `fd`, or an error if the frame is empty or larger than the
    /// dma-buf. Its stride is `width` pixels; a buffer with padded rows is described by a wider
    /// `width`.
    pub fn new(
        fd: BorrowedFd<'a>,
        format: PixelFormat,
        width: u32,
        height: u32,
    ) -> io::Result<Self> {
        Self::with_len(fd, dma_buf::size(fd)?, format, width, height)
    }

    fn with_len(
        fd: BorrowedFd<'a>,
        len: u64,
        format: PixelFormat,
        width: u32,
        height: u32,
    ) -> io::Result<Self> {
        if width == 0 || height == 0 {
            return Err(invalid("an empty surface"));
        }
        if format.is_subsampled() && (!width.is_multiple_of(2) || !height.is_multiple_of(2)) {
            return Err(invalid(format!(
                "{format:?} needs an even size, got {width}x{height}"
            )));
        }
        let need = format.frame_len(width, height);
        if need > len {
            return Err(invalid(format!(
                "a {width}x{height} {format:?} frame is {need} bytes, the dma-buf {len}"
            )));
        }
        Ok(Self {
            fd,
            format,
            width,
            height,
        })
    }

    /// The pixel format.
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// The whole frame as a rectangle.
    pub fn full(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.width,
            h: self.height,
        }
    }

    fn check(&self, r: Rect) -> io::Result<RawRect> {
        let inside = r.x.checked_add(r.w).is_some_and(|e| e <= self.width)
            && r.y.checked_add(r.h).is_some_and(|e| e <= self.height);
        if r.w == 0 || r.h == 0 || r.w > MAX_SIZE || r.h > MAX_SIZE || !inside {
            return Err(invalid(format!(
                "{r:?} does not fit a {}x{} surface",
                self.width, self.height
            )));
        }
        if self.format.is_subsampled() && !(r.x | r.y | r.w | r.h).is_multiple_of(2) {
            return Err(invalid(format!(
                "{:?} needs an even rectangle, got {r:?}",
                self.format
            )));
        }
        Ok(RawRect {
            x: r.x as i32,
            y: r.y as i32,
            w: r.w,
            h: r.h,
        })
    }

    fn image(&self, r: Rect, is_buffer: bool) -> io::Result<ImageEnh> {
        Ok(ImageEnh {
            bbuff: i32::from(is_buffer),
            format: self.format as u32,
            width: self.width,
            height: self.height,
            clip_rect: self.check(r)?,
            alpha: 0xff,
            fd: self.fd.as_raw_fd(),
            ..ImageEnh::default()
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Request {
    Blit(BltH),
    Fill(FillrectH),
    Blend(Box<Bld>),
}

/// One checked request. It borrows its surfaces, so their descriptors stay open until it is gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job<'a> {
    request: Request,
    surfaces: PhantomData<BorrowedFd<'a>>,
}

impl<'a> Job<'a> {
    fn new(request: Request) -> Self {
        Self {
            request,
            surfaces: PhantomData,
        }
    }

    /// Fills `rect` of `dst` with `color`, a raw value in the layer's fill register. Its byte
    /// layout is not given by the sources: unverified.
    pub fn fill(
        dst: &Surface<'a>,
        rect: Rect,
        color: u32,
        gamut: Gamut,
        range: ColorRange,
    ) -> io::Result<Self> {
        let mut dst = dst.image(rect, false)?;
        dst.color = color;
        dst.gamut = gamut as u32;
        dst.color_range = range as u32;
        Ok(Self::new(Request::Fill(FillrectH { dst })))
    }

    /// Copies `src_rect` of `src` to `dst_rect` of `dst`, scaling and converting the format as
    /// the two need (the mixer path).
    pub fn blit(
        src: &Surface<'a>,
        src_rect: Rect,
        dst: &Surface<'a>,
        dst_rect: Rect,
        gamut: Gamut,
        range: ColorRange,
    ) -> io::Result<Self> {
        let mut request = BltH {
            flag_h: 0,
            src: src.image(src_rect, true)?,
            dst: dst.image(dst_rect, true)?,
        };
        request.src.color_range = range as u32;
        request.dst.color_range = range as u32;
        request.dst.gamut = gamut as u32;
        Ok(Self::new(Request::Blit(request)))
    }

    /// Turns and flips `src_rect` of `src` into `dst_rect` of `dst` (the rotator path). The
    /// rotator neither scales nor converts: the formats must match and `dst_rect` must have
    /// `src_rect`'s size, swapped for a quarter turn.
    pub fn rotate(
        src: &Surface<'a>,
        src_rect: Rect,
        dst: &Surface<'a>,
        dst_rect: Rect,
        turn: Turn,
        flip_horizontal: bool,
        flip_vertical: bool,
    ) -> io::Result<Self> {
        if src.format != dst.format {
            return Err(invalid("the rotator does not convert formats"));
        }
        let want = match turn {
            Turn::Cw90 | Turn::Cw270 => (src_rect.h, src_rect.w),
            Turn::None | Turn::Cw180 => (src_rect.w, src_rect.h),
        };
        if (dst_rect.w, dst_rect.h) != want {
            return Err(invalid(format!(
                "the rotator does not scale: {dst_rect:?} from {src_rect:?} turned {turn:?}"
            )));
        }
        let mut flag_h = turn as u32;
        if flip_horizontal {
            flag_h |= 0x1000;
        }
        if flip_vertical {
            flag_h |= 0x2000;
        }
        Ok(Self::new(Request::Blit(BltH {
            flag_h,
            src: src.image(src_rect, true)?,
            dst: dst.image(dst_rect, true)?,
        })))
    }

    /// Blends `src_rect` of `src` with `background_rect` of `background` into `dst_rect` of `dst`.
    ///
    /// Both layers must be RGB and no larger than the destination rectangle. The driver also takes
    /// a YUV first layer, but its check of that layer's scaled size (`src->resize.h` tested for
    /// nonzero where zero is meant) refuses any usable size, so it is left out here.
    pub fn blend(
        src: &Surface<'a>,
        src_rect: Rect,
        background: &Surface<'a>,
        background_rect: Rect,
        dst: &Surface<'a>,
        dst_rect: Rect,
        mode: Blend,
    ) -> io::Result<Self> {
        if !src.format.is_rgb() || !background.format.is_rgb() {
            return Err(invalid("the blender's layers must be RGB here"));
        }
        if src_rect.w > dst_rect.w
            || src_rect.h > dst_rect.h
            || background_rect.w > dst_rect.w
            || background_rect.h > dst_rect.h
        {
            return Err(invalid(
                "a blended layer is larger than the destination rectangle",
            ));
        }
        let mut request = Box::new(Bld {
            bld_cmd: mode as u32,
            dst: dst.image(dst_rect, true)?,
            ..Bld::default()
        });
        request.src[0] = src.image(src_rect, true)?;
        request.src[1] = background.image(background_rect, true)?;
        Ok(Self::new(Request::Blend(request)))
    }
}

/// Runs `job` on the device at `fd` and waits for it.
pub fn run(fd: BorrowedFd<'_>, job: &mut Job<'_>) -> io::Result<()> {
    match &mut job.request {
        Request::Blit(r) => submit(fd, BITBLT_H, r),
        Request::Fill(r) => submit(fd, FILLRECT_H, r),
        Request::Blend(r) => submit(fd, BLD_H, &mut **r),
    }
}

/// Issues one of [`run`]'s requests, with the structure its number takes.
fn submit<T>(fd: BorrowedFd<'_>, nr: u32, arg: &mut T) -> io::Result<()> {
    // SAFETY: [`run`] passes each request number the structure it takes, live and exclusive for
    // the call. Its surfaces are descriptors the job borrows, whose frames and rectangles were
    // checked against the dma-buf's size when the job was built, and `use_phy_addr` is zero, so
    // the engine touches only those dma-bufs.
    unsafe { ioctl_ptr(fd, nr, std::ptr::from_mut(arg))? };
    Ok(())
}

/// The engine's and the chip's versions.
pub fn query_version(fd: BorrowedFd<'_>) -> io::Result<HardwareVersion> {
    let mut version = HardwareVersion::default();
    // SAFETY: `G2D_CMD_QUERY_VERSION` writes a `struct g2d_hardware_version`, which `version` is.
    unsafe { ioctl_ptr(fd, QUERY_VERSION, std::ptr::from_mut(&mut version))? };
    Ok(version)
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests;
