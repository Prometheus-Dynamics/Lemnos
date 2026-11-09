//! Rockchip's `rkvpss` offline scaler, the `rkvpss-offline` video node: dma-buf to dma-buf crop,
//! scale and format conversion on up to six output channels.
//!
//! The structures and request numbers are transcribed from `include/uapi/linux/rk-vpss-config.h`
//! of rockchip-linux/kernel `develop-6.1` (`(GPL-2.0+ WITH Linux-syscall-note) OR MIT`), with the
//! C compiler's sizes and offsets on LP64 pinned by the tests below. The limits are those of that
//! header and of `vpss_offline_v20.c`, the RV1126B's path.
//!
//! The uAPI needs care in three places. Its structures carry `struct dma_buf *` members, which
//! the driver takes as kernel pointers in place of the descriptor when they are not null. The
//! driver checks a dma-buf only against `stride * height`, while it places the chroma plane by
//! `ver_stride`. And it computes in `int`. So a caller cannot fill in a request here: a [`Job`]
//! is built only from [`Surface`]s, whose frame was checked against the dma-buf's real size. The
//! pointer members and `ver_stride` are always zero, and sizes are bounded well inside `int`. The
//! job borrows the surfaces until it is dropped.
//!
//! Written from the driver's source, untested on board.

use std::io;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::ptr;

use crate::dma_buf;
use crate::ioctl::{ioc, ioctl_ptr, ior, iow};

/// `BASE_VIDIOC_PRIVATE`.
const PRIVATE: u8 = 192;

/// `RKVPSS_CMD_MODULE_SEL`: hands channels to offline mode.
pub const MODULE_SEL: u32 = iow::<ModuleSel>(b'V', PRIVATE + 50);
/// `RKVPSS_CMD_FRAME_HANDLE`: runs one frame and waits for it.
pub const FRAME_HANDLE: u32 = iow::<FrameCfg>(b'V', PRIVATE + 51);
/// `RKVPSS_CMD_MODULE_GET`.
pub const MODULE_GET: u32 = ior::<ModuleSel>(b'V', PRIVATE + 54);
/// `RKVPSS_CMD_BUF_DEL_BY_FILE`: detaches every dma-buf this open file used.
pub const BUF_DEL_BY_FILE: u32 = ioc(0, b'V', PRIVATE + 57, 0);

/// The number of output channels.
pub const CHANNELS: usize = 6;
/// The smallest width or height of an image.
pub const MIN_SIZE: u32 = 32;
/// The largest width of an image on the RV1126B.
pub const MAX_WIDTH: u32 = 4096;
/// The largest height of an image on the RV1126B.
pub const MAX_HEIGHT: u32 = 3072;
/// The largest stride taken here: four bytes for each pixel of the widest image.
pub const MAX_STRIDE: u32 = 4 * MAX_WIDTH;
/// The largest scaling ratio, up or down, in either direction.
pub const MAX_RATIO: u32 = 32;
/// The largest output width of a scaling channel from CH2 up.
pub const MAX_SCALED_WIDTH: u32 = 1920;
/// The largest output height of a scaling channel from CH2 up.
pub const MAX_SCALED_HEIGHT: u32 = 1080;

/// `struct rkvpss_module_sel`.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModuleSel {
    /// Mirror and privacy-mask block in offline mode.
    pub mirror_cmsc_en: u8,
    /// Each channel in offline mode.
    pub ch_en: [u8; CHANNELS],
}

/// `struct rkvpss_input_cfg`. Built only by this module.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InputCfg {
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
    /// Bytes per line.
    pub stride: i32,
    /// Lines between the two planes of a two-plane format; always zero, meaning `height`.
    pub ver_stride: i32,
    /// V4L2 fourcc.
    pub format: i32,
    /// The dma-buf descriptor.
    pub buf_fd: i32,
    /// Quarter turns, for tiled input only; zero.
    pub rotate: i32,
    /// The RV1126B's raster quarter turn; zero.
    pub rotate_90: i32,
    /// `struct dma_buf *`, a kernel pointer; always zero.
    pub dmabuf: usize,
}

/// `struct rkvpss_output_cfg`. Built only by this module.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub struct OutputCfg {
    /// Nonzero when the channel runs.
    pub enable: i32,
    /// Left of the crop in the input.
    pub crop_h_offs: i32,
    /// Top of the crop in the input.
    pub crop_v_offs: i32,
    /// Width of the crop.
    pub crop_width: i32,
    /// Height of the crop.
    pub crop_height: i32,
    /// Output width.
    pub scl_width: i32,
    /// Output height.
    pub scl_height: i32,
    /// Bytes per line.
    pub stride: i32,
    /// V4L2 fourcc.
    pub format: i32,
    /// Nonzero flips vertically.
    pub flip: i32,
    /// The dma-buf descriptor.
    pub buf_fd: i32,
    /// Nonzero picks the average scaler; zero.
    pub avg_scl_down: i32,
    /// Alpha written to 32-bit RGB output.
    pub alpha: u32,
    /// `struct rkvpss_cmsc_cfg`, privacy masks; unused and zero.
    pub cmsc: [u8; 340],
    /// `struct rkvpss_aspt_cfg`, background fill; unused and zero.
    pub aspt: [u8; 20],
    /// `struct dma_buf *`, a kernel pointer; always zero.
    pub dmabuf: usize,
    /// `struct rkvpss_wrap_cfg`, the encoder ring; unused and zero.
    pub wrap: [u8; 12],
}

impl OutputCfg {
    const OFF: Self = Self {
        enable: 0,
        crop_h_offs: 0,
        crop_v_offs: 0,
        crop_width: 0,
        crop_height: 0,
        scl_width: 0,
        scl_height: 0,
        stride: 0,
        format: 0,
        flip: 0,
        buf_fd: -1,
        avg_scl_down: 0,
        alpha: 0,
        cmsc: [0; 340],
        aspt: [0; 20],
        dmabuf: 0,
        wrap: [0; 12],
    };
}

/// `struct rkvpss_frame_cfg`. Built only by this module.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub struct FrameCfg {
    /// Stream id for the driver's statistics, 0 to 127; zero.
    pub dev_id: i32,
    /// Frame number for the driver's statistics; zero.
    pub sequence: i32,
    /// Nonzero mirrors, with the mirror block offline; zero.
    pub mirror: i32,
    /// The input image.
    pub input: InputCfg,
    /// The output channels.
    pub output: [OutputCfg; CHANNELS],
    /// `struct rkvpss_sharp_cfg`; unused and zero.
    pub sharp: [u8; 4],
}

/// An output channel. Each has its own scaler; only CH1 writes RGB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// CH0.
    Ch0,
    /// CH1, which also writes RGB.
    Ch1,
    /// CH2.
    Ch2,
    /// CH3.
    Ch3,
    /// CH4.
    Ch4,
    /// CH5.
    Ch5,
}

impl Channel {
    fn index(self) -> usize {
        self as usize
    }

    /// This channel's bit in a channel mask.
    #[must_use]
    pub fn bit(self) -> u8 {
        1 << self.index()
    }
}

/// A raster pixel format the block reads or writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// `NV12`.
    Nv12,
    /// `NV21`.
    Nv21,
    /// `NV16`.
    Nv16,
    /// `NV61`.
    Nv61,
    /// `GREY`: output only.
    Grey,
    /// `UYVY`.
    Uyvy,
    /// `VYUY`.
    Vyuy,
    /// `YUYV`: input only.
    Yuyv,
    /// `YVYU`: input only.
    Yvyu,
    /// `RGB565`.
    Rgb565,
    /// `RGB565X`.
    Rgb565x,
    /// `RGB24`.
    Rgb24,
    /// `BGR24`.
    Bgr24,
    /// `XRGB32`.
    Xrgb32,
    /// `XBGR32`.
    Xbgr32,
    /// `RGBX32`: input only.
    Rgbx32,
    /// `BGRX32`: input only.
    Bgrx32,
}

impl PixelFormat {
    /// The V4L2 fourcc.
    #[must_use]
    pub const fn fourcc(self) -> u32 {
        u32::from_le_bytes(*match self {
            Self::Nv12 => b"NV12",
            Self::Nv21 => b"NV21",
            Self::Nv16 => b"NV16",
            Self::Nv61 => b"NV61",
            Self::Grey => b"GREY",
            Self::Uyvy => b"UYVY",
            Self::Vyuy => b"VYUY",
            Self::Yuyv => b"YUYV",
            Self::Yvyu => b"YVYU",
            Self::Rgb565 => b"RGBP",
            Self::Rgb565x => b"RGBR",
            Self::Rgb24 => b"RGB3",
            Self::Bgr24 => b"BGR3",
            Self::Xrgb32 => b"BX24",
            Self::Xbgr32 => b"XR24",
            Self::Rgbx32 => b"XB24",
            Self::Bgrx32 => b"RX24",
        })
    }

    /// The bytes of a pixel in the first plane, and so the smallest stride of one pixel.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Nv12 | Self::Nv21 | Self::Nv16 | Self::Nv61 | Self::Grey => 1,
            Self::Uyvy | Self::Vyuy | Self::Yuyv | Self::Yvyu | Self::Rgb565 | Self::Rgb565x => 2,
            Self::Rgb24 | Self::Bgr24 => 3,
            Self::Xrgb32 | Self::Xbgr32 | Self::Rgbx32 | Self::Bgrx32 => 4,
        }
    }

    /// The bytes of a frame with this stride and height.
    #[must_use]
    pub const fn frame_len(self, stride: u32, height: u32) -> u64 {
        let luma = stride as u64 * height as u64;
        match self {
            Self::Nv12 | Self::Nv21 => luma * 3 / 2,
            Self::Nv16 | Self::Nv61 => luma * 2,
            _ => luma,
        }
    }

    const fn even_width(self) -> bool {
        matches!(
            self,
            Self::Nv12
                | Self::Nv21
                | Self::Nv16
                | Self::Nv61
                | Self::Uyvy
                | Self::Vyuy
                | Self::Yuyv
                | Self::Yvyu
        )
    }

    const fn even_height(self) -> bool {
        matches!(self, Self::Nv12 | Self::Nv21)
    }

    /// Whether the block reads this format.
    #[must_use]
    pub const fn is_input(self) -> bool {
        !matches!(self, Self::Grey)
    }

    /// Whether `channel` writes this format.
    #[must_use]
    pub fn is_output_of(self, channel: Channel) -> bool {
        match self {
            Self::Nv12
            | Self::Nv21
            | Self::Nv16
            | Self::Nv61
            | Self::Grey
            | Self::Uyvy
            | Self::Vyuy => true,
            Self::Rgb565
            | Self::Rgb565x
            | Self::Rgb24
            | Self::Bgr24
            | Self::Xrgb32
            | Self::Xbgr32 => channel == Channel::Ch1,
            Self::Yuyv | Self::Yvyu | Self::Rgbx32 | Self::Bgrx32 => false,
        }
    }

    /// Whether the format needs the write path's single chroma swap: on (`Some(true)`), off
    /// (`Some(false)`), or either.
    const fn chroma_swap(self) -> Option<bool> {
        match self {
            Self::Vyuy | Self::Nv21 | Self::Nv61 => Some(true),
            Self::Uyvy | Self::Nv12 | Self::Nv16 => Some(false),
            _ => None,
        }
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

/// A dma-buf holding one frame, checked against the dma-buf's size.
#[derive(Clone, Copy, Debug)]
pub struct Surface<'a> {
    fd: BorrowedFd<'a>,
    format: PixelFormat,
    width: u32,
    height: u32,
    stride: u32,
}

impl<'a> Surface<'a> {
    /// A surface over the dma-buf `fd`, or an error if its size is outside the block's range, its
    /// stride cannot hold a line or is not a multiple of four, or the frame is larger than the
    /// dma-buf. The planes of a two-plane format follow each other with no gap.
    pub fn new(
        fd: BorrowedFd<'a>,
        format: PixelFormat,
        width: u32,
        height: u32,
        stride: u32,
    ) -> io::Result<Self> {
        Self::with_len(fd, dma_buf::size(fd)?, format, width, height, stride)
    }

    fn with_len(
        fd: BorrowedFd<'a>,
        len: u64,
        format: PixelFormat,
        width: u32,
        height: u32,
        stride: u32,
    ) -> io::Result<Self> {
        if !(MIN_SIZE..=MAX_WIDTH).contains(&width) || !(MIN_SIZE..=MAX_HEIGHT).contains(&height) {
            return Err(invalid(format!("size {width}x{height} is out of range")));
        }
        if (format.even_width() && !width.is_multiple_of(2))
            || (format.even_height() && !height.is_multiple_of(2))
        {
            return Err(invalid(format!(
                "{format:?} needs an even size, got {width}x{height}"
            )));
        }
        if stride < width * format.bytes_per_pixel()
            || stride > MAX_STRIDE
            || !stride.is_multiple_of(4)
        {
            return Err(invalid(format!(
                "stride {stride} does not fit a {width}-wide {format:?}"
            )));
        }
        let need = format.frame_len(stride, height);
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
            stride,
        })
    }
}

/// A rectangle of the input image. Every member must be even.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crop {
    /// Left.
    pub x: u32,
    /// Top.
    pub y: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

impl Crop {
    /// The whole of an image of this size, rounded down to even.
    #[must_use]
    pub fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width: width & !1,
            height: height & !1,
        }
    }
}

/// One output of a [`Job`].
#[derive(Clone, Copy, Debug)]
pub struct Output<'a> {
    /// The channel that writes it.
    pub channel: Channel,
    /// The part of the input it shows.
    pub crop: Crop,
    /// Where it goes; its size is the size the crop is scaled to.
    pub target: Surface<'a>,
    /// Flips it vertically.
    pub flip: bool,
}

/// A checked request. It borrows the descriptors it names.
#[derive(Debug)]
pub struct Job<'a> {
    cfg: FrameCfg,
    mask: u8,
    _surfaces: PhantomData<BorrowedFd<'a>>,
}

fn scales(from: u32, to: u32) -> bool {
    from.max(to) <= from.min(to) * MAX_RATIO
}

fn int(value: u32) -> i32 {
    // Every value here is bounded by the checks above: a size, a stride or a fourcc, whose bytes
    // are ASCII.
    value.cast_signed()
}

impl<'a> Job<'a> {
    /// A job reading `source` and writing `outputs`, or an error if the block would refuse it,
    /// or would silently do something else.
    pub fn new(source: Surface<'a>, outputs: &[Output<'a>]) -> io::Result<Self> {
        if !source.format.is_input() {
            return Err(invalid(format!("{:?} is not an input", source.format)));
        }
        if outputs.is_empty() {
            return Err(invalid("a job needs an output"));
        }
        let mut cfg = FrameCfg {
            dev_id: 0,
            sequence: 0,
            mirror: 0,
            input: InputCfg {
                width: int(source.width),
                height: int(source.height),
                stride: int(source.stride),
                ver_stride: 0,
                format: int(source.format.fourcc()),
                buf_fd: source.fd.as_raw_fd(),
                rotate: 0,
                rotate_90: 0,
                dmabuf: 0,
            },
            output: [OutputCfg::OFF; CHANNELS],
            sharp: [0; 4],
        };
        let mut mask = 0;
        let mut swap = None;
        for output in outputs {
            let bit = output.channel.bit();
            if mask & bit != 0 {
                return Err(invalid(format!("{:?} used twice", output.channel)));
            }
            mask |= bit;
            check_output(&source, output)?;
            if let Some(wanted) = output.target.format.chroma_swap()
                && *swap.get_or_insert(wanted) != wanted
            {
                return Err(invalid("the outputs need different chroma orders"));
            }
            cfg.output[output.channel.index()] = output_cfg(output);
        }
        Ok(Self {
            cfg,
            mask,
            _surfaces: PhantomData,
        })
    }

    /// The channels this job writes, one bit per [`Channel::bit`].
    #[must_use]
    pub fn channels(&self) -> u8 {
        self.mask
    }
}

fn check_output(source: &Surface<'_>, output: &Output<'_>) -> io::Result<()> {
    let Output {
        channel,
        crop,
        target,
        ..
    } = *output;
    if !target.format.is_output_of(channel) {
        return Err(invalid(format!(
            "{channel:?} does not write {:?}",
            target.format
        )));
    }
    if !target.width.is_multiple_of(4) {
        return Err(invalid(format!(
            "output width {} is not a multiple of four",
            target.width
        )));
    }
    if crop.width == 0
        || crop.height == 0
        || (crop.x | crop.y | crop.width | crop.height) & 1 != 0
        || u64::from(crop.x) + u64::from(crop.width) > u64::from(source.width)
        || u64::from(crop.y) + u64::from(crop.height) > u64::from(source.height)
    {
        return Err(invalid(format!(
            "crop {crop:?} is odd, empty or outside the {}x{} input",
            source.width, source.height
        )));
    }
    if !scales(crop.width, target.width) || !scales(crop.height, target.height) {
        return Err(invalid(format!(
            "{}x{} to {}x{} is beyond 1:{MAX_RATIO}",
            crop.width, crop.height, target.width, target.height
        )));
    }
    let scaled = crop.width != target.width || crop.height != target.height;
    if channel.index() >= Channel::Ch2.index()
        && scaled
        && (target.width > MAX_SCALED_WIDTH || target.height > MAX_SCALED_HEIGHT)
    {
        return Err(invalid(format!(
            "{channel:?} scales to at most {MAX_SCALED_WIDTH}x{MAX_SCALED_HEIGHT}"
        )));
    }
    Ok(())
}

fn output_cfg(output: &Output<'_>) -> OutputCfg {
    let Output {
        crop, target, flip, ..
    } = *output;
    OutputCfg {
        enable: 1,
        crop_h_offs: int(crop.x),
        crop_v_offs: int(crop.y),
        crop_width: int(crop.width),
        crop_height: int(crop.height),
        scl_width: int(target.width),
        scl_height: int(target.height),
        stride: int(target.stride),
        format: int(target.format.fourcc()),
        flip: i32::from(flip),
        buf_fd: target.fd.as_raw_fd(),
        alpha: 0xff,
        ..OutputCfg::OFF
    }
}

fn module_get(fd: BorrowedFd<'_>) -> io::Result<ModuleSel> {
    let mut sel = ModuleSel::default();
    // SAFETY: `RKVPSS_CMD_MODULE_GET` writes a `struct rkvpss_module_sel`, which `sel` is.
    unsafe { ioctl_ptr(fd, MODULE_GET, ptr::from_mut(&mut sel))? };
    Ok(sel)
}

/// The channels in offline mode, one bit per [`Channel::bit`].
pub fn offline_channels(fd: BorrowedFd<'_>) -> io::Result<u8> {
    let channels = module_get(fd)?.ch_en;
    Ok(channels
        .iter()
        .enumerate()
        .fold(0, |mask, (i, &on)| mask | (u8::from(on != 0) << i)))
}

/// Adds the channels in `mask` to offline mode, leaving the others as they are. The driver
/// refuses this while the block's camera path is running, and drops the selection when the last
/// open file closes.
pub fn select_offline(fd: BorrowedFd<'_>, mask: u8) -> io::Result<()> {
    let mut sel = module_get(fd)?;
    for (i, on) in sel.ch_en.iter_mut().enumerate() {
        if mask & (1 << i) != 0 {
            *on = 1;
        }
    }
    // SAFETY: `RKVPSS_CMD_MODULE_SEL` reads a `struct rkvpss_module_sel`, which `sel` is.
    unsafe { ioctl_ptr(fd, MODULE_SEL, ptr::from_mut(&mut sel))? };
    Ok(())
}

/// Runs `job` and waits for it. Every channel it writes must be in offline mode, because the
/// driver quietly skips one that is not. The driver gives up with `EAGAIN` after 500 ms, and the
/// outputs of a job that failed that way hold nothing useful.
pub fn run(fd: BorrowedFd<'_>, job: &mut Job<'_>) -> io::Result<()> {
    let missing = job.mask & !offline_channels(fd)?;
    if missing != 0 {
        return Err(invalid(format!(
            "channels {missing:#b} are not in offline mode"
        )));
    }
    // SAFETY: `RKVPSS_CMD_FRAME_HANDLE` reads a `struct rkvpss_frame_cfg`, which `job.cfg` is. Its
    // descriptors are ones the job borrows, its frames were checked against their dma-bufs' sizes
    // when it was built, and its kernel-pointer members are zero, so the block touches only those
    // dma-bufs.
    unsafe { ioctl_ptr(fd, FRAME_HANDLE, ptr::from_mut(&mut job.cfg))? };
    Ok(())
}

/// Detaches every dma-buf this open file used. The driver keeps each one attached between frames,
/// and so holds its memory, until this call or the file's close.
pub fn release_buffers(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: `RKVPSS_CMD_BUF_DEL_BY_FILE` takes no argument, and the driver accepts a null one.
    unsafe { ioctl_ptr(fd, BUF_DEL_BY_FILE, ptr::null_mut::<u8>())? };
    Ok(())
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests;
