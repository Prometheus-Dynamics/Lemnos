//! Jobs, built only from frames checked against the buffers the caller holds.
//!
//! The kernel driver checks nothing of a job: it adds a dma-buf's bus address to the offsets it
//! is given (to a chroma offset only when it is nonzero, so a zero one stays address 0), takes a
//! negative descriptor to mean the offsets are physical addresses, and lets the block read and
//! write `stride * (rows - 1) + row` bytes from each plane. So a [`Frame`] is checked against the
//! real size of its dma-buf for the video size it holds, its chroma offsets must be nonzero, and a
//! [`Job`] is made only from frames and a [`Config`] that agree. Every frame slot of a request
//! names a checked frame, even the slots the chosen mode leaves unused.

use std::io;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, BorrowedFd};

use crate::uapi;

/// The largest width or height: the block's size register holds 11 bits of the size less one.
pub const MAX_SIZE: u32 = 2048;

/// The largest stride in bytes: the block reads a field at twice the stride, from a 16-bit pitch
/// register.
pub const MAX_STRIDE: u32 = 0x7fff;

/// A pixel format the block takes: 8-bit YUV, two or three planes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Y plane, interleaved CbCr at half height.
    Nv12,
    /// Y plane, interleaved CrCb at half height.
    Nv21,
    /// Y plane, interleaved CbCr at full height.
    Nv16,
    /// Y plane, interleaved CrCb at full height.
    Nv61,
    /// Y, Cb and Cr planes, chroma at half width and height.
    Yuv420,
    /// Y, Cr and Cb planes, chroma at half width and height.
    Yvu420,
    /// Y, Cb and Cr planes, chroma at half width.
    Yuv422,
    /// Y, Cr and Cb planes, chroma at half width.
    Yvu422,
}

impl Format {
    /// The DRM code.
    pub const fn fourcc(self) -> u32 {
        match self {
            Self::Nv12 => uapi::fourcc::NV12,
            Self::Nv21 => uapi::fourcc::NV21,
            Self::Nv16 => uapi::fourcc::NV16,
            Self::Nv61 => uapi::fourcc::NV61,
            Self::Yuv420 => uapi::fourcc::YUV420,
            Self::Yvu420 => uapi::fourcc::YVU420,
            Self::Yuv422 => uapi::fourcc::YUV422,
            Self::Yvu422 => uapi::fourcc::YVU422,
        }
    }

    /// Whether the chroma is in two planes of its own.
    pub const fn planar(self) -> bool {
        matches!(
            self,
            Self::Yuv420 | Self::Yvu420 | Self::Yuv422 | Self::Yvu422
        )
    }

    /// Chroma rows for `height` luma rows.
    const fn chroma_rows(self, height: u32) -> u32 {
        match self {
            Self::Nv12 | Self::Nv21 | Self::Yuv420 | Self::Yvu420 => height / 2,
            _ => height,
        }
    }

    /// Bytes in one chroma row (of one chroma plane) for `width` pixels.
    const fn chroma_row_bytes(self, width: u32) -> u32 {
        if self.planar() { width / 2 } else { width }
    }
}

/// Where a frame's planes are in its buffer, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Layout {
    /// Offset of the first luma row.
    pub luma_offset: u64,
    /// Bytes from one luma row to the next.
    pub luma_stride: u32,
    /// Offset of the chroma plane (the Cb plane of a planar format, by the format's order the
    /// first chroma plane). Never 0: the kernel leaves a zero chroma offset unrelocated.
    pub chroma_offset: u64,
    /// Offset of the second chroma plane of a planar format; 0 for a two-plane format.
    pub chroma2_offset: u64,
    /// Bytes from one chroma row to the next, the same for both chroma planes.
    pub chroma_stride: u32,
}

/// Why a frame or job was refused. Nothing is allocated for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    /// The width or height isn't a multiple of 4 from 4 to [`MAX_SIZE`].
    Size,
    /// A stride is shorter than its row or longer than [`MAX_STRIDE`].
    Stride,
    /// A chroma offset is 0, or a two-plane format names a second chroma plane.
    ChromaOffset,
    /// A plane runs past the end of the buffer.
    OutOfBuffer,
    /// The descriptor isn't one the kernel would take as a dma-buf.
    Descriptor,
    /// A frame's size isn't the job's.
    FrameSize,
    /// The number of input or output frames isn't the mode's.
    FrameCount,
    /// With noise reduction, every output must be planar.
    NotPlanar,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Size => "width and height must be multiples of 4 from 4 to 2048",
            Self::Stride => "a stride is shorter than its row or too long",
            Self::ChromaOffset => "a chroma offset is 0 or names a plane the format lacks",
            Self::OutOfBuffer => "a plane runs past the end of its buffer",
            Self::Descriptor => "not a descriptor",
            Self::FrameSize => "a frame's size is not the job's",
            Self::FrameCount => "the number of frames is not the mode's",
            Self::NotPlanar => "noise reduction needs planar outputs",
        })
    }
}

impl std::error::Error for Invalid {}

fn check_size(width: u32, height: u32) -> Result<(), Invalid> {
    let ok = |n: u32| (4..=MAX_SIZE).contains(&n) && n.is_multiple_of(4);
    if ok(width) && ok(height) {
        Ok(())
    } else {
        Err(Invalid::Size)
    }
}

/// The plane at `offset` with `rows` rows of `row` bytes, `stride` apart, must end inside `len`.
fn check_plane(len: u64, offset: u64, stride: u32, rows: u32, row: u32) -> Result<(), Invalid> {
    if stride < row || stride > MAX_STRIDE {
        return Err(Invalid::Stride);
    }
    let end = u64::from(stride)
        .checked_mul(u64::from(rows).saturating_sub(1))
        .and_then(|n| n.checked_add(u64::from(row)))
        .and_then(|n| n.checked_add(offset))
        .ok_or(Invalid::OutOfBuffer)?;
    if end > len {
        return Err(Invalid::OutOfBuffer);
    }
    Ok(())
}

/// An image in a dma-buf the caller holds, checked against the buffer's real size.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    fd: BorrowedFd<'a>,
    format: Format,
    width: u32,
    height: u32,
    layout: Layout,
}

impl<'a> Frame<'a> {
    /// A `width` x `height` frame of `format` laid out as `layout` in the dma-buf `fd`, whose
    /// size is asked of the kernel.
    #[cfg(target_os = "linux")]
    pub fn new(
        fd: BorrowedFd<'a>,
        format: Format,
        width: u32,
        height: u32,
        layout: Layout,
    ) -> Result<Self, FrameError> {
        let len = lemnos_linux_sys::dma_buf::size(fd).map_err(FrameError::Size)?;
        Self::with_len(fd, len, format, width, height, layout).map_err(FrameError::Invalid)
    }

    /// As [`Self::new`], for a buffer of `len` bytes. Not public: a `len` larger than the buffer's
    /// own would let the block run past its end. [`Self::new`] asks the kernel.
    pub(crate) fn with_len(
        fd: BorrowedFd<'a>,
        len: u64,
        format: Format,
        width: u32,
        height: u32,
        layout: Layout,
    ) -> Result<Self, Invalid> {
        check_size(width, height)?;
        if fd.as_raw_fd() < 0 {
            return Err(Invalid::Descriptor);
        }
        check_plane(len, layout.luma_offset, layout.luma_stride, height, width)?;
        let rows = format.chroma_rows(height);
        let row = format.chroma_row_bytes(width);
        if layout.chroma_offset == 0 || (format.planar() == (layout.chroma2_offset == 0)) {
            return Err(Invalid::ChromaOffset);
        }
        check_plane(len, layout.chroma_offset, layout.chroma_stride, rows, row)?;
        if format.planar() {
            check_plane(len, layout.chroma2_offset, layout.chroma_stride, rows, row)?;
        }
        Ok(Self {
            fd,
            format,
            width,
            height,
            layout,
        })
    }

    /// The frame's format.
    pub fn format(&self) -> Format {
        self.format
    }

    /// The frame's width and height.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn fb(&self) -> uapi::Fb {
        uapi::Fb {
            format: self.format.fourcc(),
            dma_buf_fd: self.fd.as_raw_fd(),
            buf: uapi::Buf {
                y_addr: self.layout.luma_offset,
                cb_addr: self.layout.chroma_offset,
                cr_addr: self.layout.chroma2_offset,
                ystride: self.layout.luma_stride,
                cstride: self.layout.chroma_stride,
            },
            size: uapi::Size {
                width: self.width,
                height: self.height,
            },
        }
    }
}

/// Why [`Frame::new`] refused.
#[derive(Debug)]
pub enum FrameError {
    /// The descriptor's size couldn't be read.
    Size(io::Error),
    /// The frame doesn't fit, or is malformed.
    Invalid(Invalid),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Size(e) => write!(f, "the buffer's size: {e}"),
            Self::Invalid(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for FrameError {}

/// How strongly noise reduction smooths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// `DI_TNR_LEVEL_HIGH`.
    High,
    /// `DI_TNR_LEVEL_MIDDLE`.
    Middle,
    /// `DI_TNR_LEVEL_LOW`.
    Low,
}

/// Temporal noise reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tnr {
    /// Adaptive (`DI_TNR_MODE_ADAPTIVE`) or fixed (`DI_TNR_MODE_FIX`).
    pub adaptive: bool,
    /// How strongly.
    pub level: Level,
}

/// What a job does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Weaves the two fields of one input into one output.
    Weave,
    /// Line-doubles one field of one input into one output.
    Bob,
    /// Motion-adaptive, from the previous and current inputs into one output, with noise
    /// reduction into a further output when `tnr` is set.
    Motion {
        /// Film-mode detection.
        film_detect: bool,
        /// Noise reduction.
        tnr: Option<Tnr>,
    },
    /// Motion-adaptive at field rate, from three inputs into two outputs, with noise reduction
    /// into a further output when `tnr` is set.
    MotionFieldRate {
        /// Film-mode detection.
        film_detect: bool,
        /// Noise reduction.
        tnr: Option<Tnr>,
    },
    /// Noise reduction only, from the previous and current inputs into one output.
    NoiseOnly(Tnr),
}

impl Mode {
    /// (inputs, deinterlaced outputs, noise-reduced outputs) a job of this mode takes.
    pub const fn frames(self) -> (usize, usize, usize) {
        match self {
            Self::Weave | Self::Bob => (1, 1, 0),
            Self::Motion { tnr, .. } => (2, 1, tnr.is_some() as usize),
            Self::MotionFieldRate { tnr, .. } => (3, 2, tnr.is_some() as usize),
            Self::NoiseOnly(_) => (2, 0, 1),
        }
    }

    fn tnr(self) -> Option<Tnr> {
        match self {
            Self::Weave | Self::Bob => None,
            Self::Motion { tnr, .. } | Self::MotionFieldRate { tnr, .. } => tnr,
            Self::NoiseOnly(tnr) => Some(tnr),
        }
    }
}

/// The settings a run of jobs shares; changing them resets the kernel's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    width: u32,
    height: u32,
    mode: Mode,
}

/// The kernel's settings for a [`Config`], in the order they are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// `DI_IOC_SET_VIDEO_SIZE`.
    pub size: uapi::Size,
    /// `DI_IOC_SET_VIDEO_CROP`: the whole frame.
    pub crop: uapi::Rect,
    /// `DI_IOC_SET_DIT_MODE`.
    pub dit: uapi::DitMode,
    /// `DI_IOC_SET_TNR_MODE`.
    pub tnr: uapi::TnrMode,
    /// `DI_IOC_SET_FMD_ENABLE`.
    pub fmd: uapi::FmdEnable,
}

impl Config {
    /// Jobs of `mode` on `width` x `height` frames.
    pub fn new(width: u32, height: u32, mode: Mode) -> Result<Self, Invalid> {
        check_size(width, height)?;
        Ok(Self {
            width,
            height,
            mode,
        })
    }

    /// The mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The kernel's settings, which pick the same mode in its check.
    pub fn settings(&self) -> Settings {
        use uapi::{intp, out, tnr};
        let (intp_mode, out_frame_mode, film_detect) = match self.mode {
            Mode::Weave => (intp::WEAVE, out::FRAMES_1, false),
            Mode::Bob => (intp::BOB, out::FRAMES_1, false),
            Mode::Motion { film_detect, .. } => (intp::MOTION, out::FRAMES_1, film_detect),
            Mode::MotionFieldRate { film_detect, .. } => (intp::MOTION, out::FRAMES_2, film_detect),
            Mode::NoiseOnly(_) => (intp::INVALID, out::FRAMES_0, false),
        };
        let tnr_mode = match self.mode.tnr() {
            None => uapi::TnrMode::default(),
            Some(t) => uapi::TnrMode {
                mode: if t.adaptive {
                    tnr::MODE_ADAPTIVE
                } else {
                    tnr::MODE_FIX
                },
                level: match t.level {
                    Level::High => tnr::LEVEL_HIGH,
                    Level::Middle => tnr::LEVEL_MIDDLE,
                    Level::Low => tnr::LEVEL_LOW,
                },
            },
        };
        Settings {
            size: uapi::Size {
                width: self.width,
                height: self.height,
            },
            crop: uapi::Rect {
                left: 0,
                top: 0,
                right: self.width,
                bottom: self.height,
            },
            dit: uapi::DitMode {
                intp_mode,
                out_frame_mode,
            },
            tnr: tnr_mode,
            fmd: uapi::FmdEnable {
                en: u32::from(film_detect),
            },
        }
    }
}

/// Which field a job starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// The top field (`base_field` 0).
    Top,
    /// The bottom field (`base_field` 1).
    Bottom,
}

/// One job: a request built from checked frames, which it borrows so their descriptors stay open
/// until it is gone.
#[derive(Debug, Clone, Copy)]
pub struct Job<'a> {
    config: Config,
    arg: uapi::ProcessFbArg,
    frames: PhantomData<BorrowedFd<'a>>,
}

impl<'a> Job<'a> {
    /// A job of `config` from `inputs` (oldest first) into `outputs` (deinterlaced, then the
    /// noise-reduced one), as many of each as [`Mode::frames`] says, all of the config's size.
    pub fn new(
        config: Config,
        top_field_first: bool,
        base_field: Field,
        inputs: &[Frame<'a>],
        outputs: &[Frame<'a>],
    ) -> Result<Self, Invalid> {
        let (n_in, n_dit, n_tnr) = config.mode.frames();
        if inputs.len() != n_in || outputs.len() != n_dit + n_tnr {
            return Err(Invalid::FrameCount);
        }
        let size = (config.width, config.height);
        if inputs.iter().chain(outputs).any(|f| f.size() != size) {
            return Err(Invalid::FrameSize);
        }
        if n_tnr > 0 && outputs.iter().any(|f| !f.format.planar()) {
            return Err(Invalid::NotPlanar);
        }
        // Every slot names a checked frame; the mode decides which the kernel uses.
        let first = inputs[0].fb();
        let input = |i: usize| inputs.get(i).map_or(first, Frame::fb);
        let output = |i: usize| outputs.get(i).map_or(first, Frame::fb);
        let mut arg = uapi::ProcessFbArg {
            is_interlace: 1,
            is_pulldown: 0,
            top_field_first: u8::from(top_field_first),
            base_field: match base_field {
                Field::Top => 0,
                Field::Bottom => 1,
            },
            in_fb0: input(0),
            in_fb1: input(1),
            in_fb2: input(2),
            out_dit_fb0: first,
            out_dit_fb1: first,
            out_tnr_fb0: first,
        };
        if n_dit > 0 {
            arg.out_dit_fb0 = output(0);
        }
        if n_dit > 1 {
            arg.out_dit_fb1 = output(1);
        }
        if n_tnr > 0 {
            arg.out_tnr_fb0 = output(n_dit);
        }
        Ok(Self {
            config,
            arg,
            frames: PhantomData,
        })
    }

    /// The job's config.
    pub fn config(&self) -> Config {
        self.config
    }

    /// A copy of the request, for the call that issues it.
    pub fn request(&self) -> uapi::ProcessFbArg {
        self.arg
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
