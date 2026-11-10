//! [`Vglite`]: the GPU, opened when it is needed and reopened after a failure, and one call per
//! operation.
//!
//! Each operation is one submit: staging buffers for its source and target, its stream (the
//! target, the operation, the flush and the end; see `command.rs`), then a wait, then the result
//! copied out. The open follows `vg_lite_init` and the flow `vg_lite_finish` of `VGLite/vg_lite.c`
//! (Vivante, MIT; the notice is in `command.rs`).
//!
//! # Failures
//!
//! What the caller asked wrongly, or what the unit cannot do, is [`RunError::Refused`], and the
//! GPU stays open. Anything else (opening, the chip check, a kernel request that fails another
//! way) closes the GPU and starts a backoff: until it is due, every call answers
//! [`RunError::BackingOff`] without touching the device, and the first call after it reopens.
//! The delay doubles from 250 ms, is capped at 8 s, and clears when an operation runs. The
//! driver never gives up and never turns itself off; [`Vglite::stop`] closes it, and the next
//! call opens it again.
//!
//! A failed wait is different: the stream was submitted and the GPU may still be reading and
//! writing its staging buffers and command buffer. Those buffers and the context are leaked, not
//! freed, so the kernel cannot hand that memory to anything else while the GPU may write it; the
//! GPU is then reopened as after any failure.
//!
//! # Allocation
//!
//! Opening allocates (the context storage, the stream's words). An operation on an open GPU
//! allocates nothing on the heap once the line outline's buffers have grown to its size: the
//! staging buffers are kernel memory, and the stream reuses its words.

use std::io;
use std::path::PathBuf;

use lemnos_driver_vglite_sys::{Context, Device, Identification, RunError as SysRunError};

use crate::command::{self, Op};
use crate::plan::{
    self, BLEND_NONE, BlendMode, Filter, Format, IDENTITY, Image, Matrix, Sampling, Transform,
    check_output, check_rect, invalid, unsupported,
};
use crate::stroke::{Cap, Join, Outliner, Subpath};
use crate::surface::{Frame, Input, Output, Surface, Transfer};

/// `VG_LITE_COMMAND_BUFFER_SIZE`: the bytes of each command buffer (128 KiB).
const COMMAND_BUFFER_BYTES: u32 = 128 << 10;
/// The tessellation buffer `vg_lite_init(256, 256)` asks for.
const TESS: i32 = 256;
/// The chip identification the vendor's build for this unit expects (`vg_lite_options.h`:
/// CHIPID 0x265, REVISION 0x1003, CID 0x417; unverified on board).
const CHIP_ID: u32 = 0x265;
const REVISION: u32 = 0x1003;
const CID: u32 = 0x417;
/// First delay before a reopen.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The delay before reopen attempt `attempt` (from 0), doubling to the 8 s cap.
pub const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// The most points [`Vglite::stroke_bytes`] takes. Its outline must also fit one command
/// buffer: a few thousand segments at most.
pub const MAX_STROKE_POINTS: usize = 4096;

/// How [`Vglite::stroke_bytes`] strokes a polyline (`vg_lite_set_stroke`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stroke {
    /// The full width, positive and finite.
    pub width: f32,
    /// The open ends; ignored when `closed`.
    pub cap: Cap,
    /// The corners.
    pub join: Join,
    /// The miter limit, as a ratio of the width; below 1 counts as 1.
    pub miter_limit: f32,
    /// Whether the last point joins back to the first (no caps).
    pub closed: bool,
}

/// Why an operation did not run.
#[derive(Debug)]
pub enum RunError {
    /// The operation was refused: the caller's arguments, or something the unit cannot do
    /// (`Unsupported`). The GPU stays open.
    Refused(io::Error),
    /// The GPU failed; it is closed and reopens on the first call from `retry_at_ns`.
    Device {
        /// What failed.
        error: io::Error,
        /// When the next call may reopen it, on the caller's clock.
        retry_at_ns: u64,
    },
    /// An earlier failure's backoff has not run out. The device was not touched.
    BackingOff {
        /// When the next call may reopen it.
        retry_at_ns: u64,
    },
}

impl RunError {
    /// The kind of error underneath; `WouldBlock` while backing off.
    pub fn kind(&self) -> io::ErrorKind {
        match self {
            Self::Refused(error) | Self::Device { error, .. } => error.kind(),
            Self::BackingOff { .. } => io::ErrorKind::WouldBlock,
        }
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(e) => write!(f, "refused: {e}"),
            Self::Device { error, retry_at_ns } => {
                write!(f, "the GPU failed ({error}); retrying at {retry_at_ns} ns")
            }
            Self::BackingOff { retry_at_ns } => write!(f, "backing off until {retry_at_ns} ns"),
        }
    }
}

impl std::error::Error for RunError {}

/// Where the GPU is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not open, and free to open on the next call.
    Closed,
    /// Open.
    Open,
    /// Closed after a failure; the next call at or after `retry_at_ns` reopens it.
    BackingOff {
        /// When.
        retry_at_ns: u64,
        /// Failures in a row.
        failures: u32,
    },
}

/// Where a step of an operation went wrong, which decides what the failure does.
enum Fault {
    /// The caller's arguments or buffers, or something the unit cannot do.
    Caller(io::Error),
    /// A kernel request.
    Device(io::Error),
    /// The wait after a submit.
    Wait(io::Error),
}

impl From<io::Error> for Fault {
    fn from(error: io::Error) -> Self {
        Self::Caller(error)
    }
}

/// A kernel request's error: the caller's when the kernel or a check refused the arguments,
/// the device's otherwise.
fn kernel_fault(error: io::Error) -> Fault {
    match error.kind() {
        io::ErrorKind::InvalidInput | io::ErrorKind::OutOfMemory | io::ErrorKind::Unsupported => {
            Fault::Caller(error)
        }
        _ => Fault::Device(error),
    }
}

/// The GPU on its device node and one call per operation. Untested on board: see the crate
/// docs.
pub struct Vglite {
    path: PathBuf,
    gpu: Option<Context>,
    failures: u32,
    retry_at_ns: u64,
    outliner: Outliner,
}

impl std::fmt::Debug for Vglite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vglite")
            .field("path", &self.path)
            .field("state", &self.state())
            .finish()
    }
}

impl Vglite {
    /// The GPU at `path` ([`crate::DEVICE`] on the vendor's kernels).
    /// Nothing is opened until the first operation.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            gpu: None,
            failures: 0,
            retry_at_ns: 0,
            outliner: Outliner::default(),
        }
    }

    /// Where the GPU is.
    pub fn state(&self) -> State {
        if self.gpu.is_some() {
            State::Open
        } else if self.failures > 0 {
            State::BackingOff {
                retry_at_ns: self.retry_at_ns,
                failures: self.failures,
            }
        } else {
            State::Closed
        }
    }

    /// Closes the GPU. The next operation opens it again, with no backoff.
    pub fn stop(&mut self) {
        self.gpu = None;
        self.failures = 0;
        self.retry_at_ns = 0;
    }

    /// The open context, opening it first when it is closed and the backoff allows.
    fn context(&mut self, now_ns: u64) -> Result<&mut Context, RunError> {
        if self.gpu.is_none() {
            if self.failures > 0 && now_ns < self.retry_at_ns {
                return Err(RunError::BackingOff {
                    retry_at_ns: self.retry_at_ns,
                });
            }
            match open(&self.path) {
                Ok(context) => self.gpu = Some(context),
                Err(error) => return Err(self.fail(now_ns, error)),
            }
        }
        // Set just above when it was empty.
        self.gpu
            .as_mut()
            .ok_or_else(|| RunError::Refused(io::Error::other("no context")))
    }

    /// Closes the GPU and schedules the next attempt.
    fn fail(&mut self, now_ns: u64, error: io::Error) -> RunError {
        self.gpu = None;
        let retry_at_ns = now_ns.saturating_add(backoff_ns(self.failures));
        self.failures = self.failures.saturating_add(1);
        self.retry_at_ns = retry_at_ns;
        RunError::Device { error, retry_at_ns }
    }

    /// One operation: the staging surfaces, the stream built over them (the stream checks every
    /// address and extent), the stream run and waited for, and the result copied out. An
    /// operation that draws nothing submits nothing and leaves the output alone.
    fn run(
        &mut self,
        now_ns: u64,
        src: Option<(Image, Input<'_>)>,
        dst: Image,
        mut out: Output<'_>,
        keep: bool,
        op: Op<'_>,
    ) -> Result<Transfer, RunError> {
        let context = self.context(now_ns)?;
        match operation(context, src, dst, &mut out, keep, op) {
            Ok(transfer) => {
                self.failures = 0;
                Ok(transfer)
            }
            Err(Fault::Caller(error)) => Err(RunError::Refused(error)),
            Err(Fault::Device(error)) => Err(self.fail(now_ns, error)),
            Err(Fault::Wait(error)) => {
                // The GPU may still run: leak the context with its command buffers, as the
                // surfaces were leaked in `operation`.
                if let Some(context) = self.gpu.take() {
                    std::mem::forget(context);
                }
                Err(self.fail(now_ns, error))
            }
        }
    }

    /// A blit of the whole source through `matrix`, with `blend` and `sampling`.
    #[allow(clippy::too_many_arguments)]
    fn blit(
        &mut self,
        now_ns: u64,
        src: (Image, Input<'_>),
        dst: Image,
        out: Output<'_>,
        keep: bool,
        matrix: Matrix,
        blend: u32,
        sampling: Sampling,
    ) -> Result<Transfer, RunError> {
        self.run(
            now_ns,
            Some(src),
            dst,
            out,
            keep,
            Op::Blit {
                matrix,
                blend,
                sampling,
            },
        )
    }

    fn resize(
        &mut self,
        now_ns: u64,
        src: Image,
        input: Input<'_>,
        dst: Image,
        out: Output<'_>,
        filter: Filter,
    ) -> Result<Transfer, RunError> {
        let (matrix, sampling) = plan::resize(src, dst, filter).map_err(RunError::Refused)?;
        // An area downscale reads whole 2x2 blocks: a trailing odd row or column is cropped.
        let src = match filter {
            Filter::Area => Image {
                width: dst.width * 2,
                height: dst.height * 2,
                ..src
            },
            Filter::Bilinear => src,
        };
        self.blit(
            now_ns,
            (src, input),
            dst,
            out,
            false,
            matrix,
            BLEND_NONE,
            sampling,
        )
    }

    /// Resize between byte buffers of one format, to `dst`'s size: [`Filter::Bilinear`] either
    /// way, [`Filter::Area`] by exactly 2 (a half-phase bilinear blit; anything else is
    /// `Unsupported`).
    pub fn resize_bytes(
        &mut self,
        now_ns: u64,
        src: Image,
        from: &[u8],
        dst: Image,
        to: &mut [u8],
        filter: Filter,
    ) -> Result<Transfer, RunError> {
        checked(src, from.len())?;
        checked(dst, to.len())?;
        self.resize(
            now_ns,
            src,
            Input::Bytes(from),
            dst,
            Output::Bytes(to),
            filter,
        )
    }

    /// [`Vglite::resize_bytes`] between dma-bufs.
    pub fn resize_dmabuf(
        &mut self,
        now_ns: u64,
        src: Frame<'_>,
        dst: Frame<'_>,
        filter: Filter,
    ) -> Result<Transfer, RunError> {
        self.resize(
            now_ns,
            src.image,
            Input::Frame(src),
            dst.image,
            Output::Frame(dst),
            filter,
        )
    }

    fn crop(
        &mut self,
        now_ns: u64,
        src: Image,
        input: Input<'_>,
        rect: (u32, u32, u32, u32),
        dst: Image,
        out: Output<'_>,
    ) -> Result<Transfer, RunError> {
        check_rect(rect, src.width, src.height).map_err(RunError::Refused)?;
        check_output(dst, (rect.2, rect.3), src.format).map_err(RunError::Refused)?;
        self.blit(
            now_ns,
            (src, input),
            dst,
            out,
            false,
            plan::crop(rect),
            BLEND_NONE,
            Sampling::Point,
        )
    }

    /// `rect` (left, top, width, height) of `src`, unscaled, into `dst` of the rectangle's size:
    /// a translated, point-sampled blit. A full-frame rectangle is a copy.
    pub fn crop_bytes(
        &mut self,
        now_ns: u64,
        src: Image,
        from: &[u8],
        rect: (u32, u32, u32, u32),
        dst: Image,
        to: &mut [u8],
    ) -> Result<Transfer, RunError> {
        checked(src, from.len())?;
        checked(dst, to.len())?;
        self.crop(
            now_ns,
            src,
            Input::Bytes(from),
            rect,
            dst,
            Output::Bytes(to),
        )
    }

    /// [`Vglite::crop_bytes`] between dma-bufs.
    pub fn crop_dmabuf(
        &mut self,
        now_ns: u64,
        src: Frame<'_>,
        rect: (u32, u32, u32, u32),
        dst: Frame<'_>,
    ) -> Result<Transfer, RunError> {
        self.crop(
            now_ns,
            src.image,
            Input::Frame(src),
            rect,
            dst.image,
            Output::Frame(dst),
        )
    }

    fn transform(
        &mut self,
        now_ns: u64,
        src: Image,
        input: Input<'_>,
        dst: Image,
        out: Output<'_>,
        t: Transform,
    ) -> Result<Transfer, RunError> {
        check_output(dst, t.output_size(src.width, src.height), src.format)
            .map_err(RunError::Refused)?;
        self.blit(
            now_ns,
            (src, input),
            dst,
            out,
            false,
            plan::transform(t, src.width, src.height),
            BLEND_NONE,
            Sampling::Point,
        )
    }

    /// A flip or a clockwise quarter turn into `dst` of the result's size: a mirror or rotation
    /// matrix, point sampled (the vendor's build for this unit has no mirror shortcut).
    pub fn transform_bytes(
        &mut self,
        now_ns: u64,
        src: Image,
        from: &[u8],
        dst: Image,
        to: &mut [u8],
        t: Transform,
    ) -> Result<Transfer, RunError> {
        checked(src, from.len())?;
        checked(dst, to.len())?;
        self.transform(now_ns, src, Input::Bytes(from), dst, Output::Bytes(to), t)
    }

    /// [`Vglite::transform_bytes`] between dma-bufs.
    pub fn transform_dmabuf(
        &mut self,
        now_ns: u64,
        src: Frame<'_>,
        dst: Frame<'_>,
        t: Transform,
    ) -> Result<Transfer, RunError> {
        self.transform(
            now_ns,
            src.image,
            Input::Frame(src),
            dst.image,
            Output::Frame(dst),
            t,
        )
    }

    fn fill(
        &mut self,
        now_ns: u64,
        image: Image,
        out: Output<'_>,
        rect: (u32, u32, u32, u32),
        rgba: [u8; 4],
    ) -> Result<Transfer, RunError> {
        check_rect(rect, image.width, image.height).map_err(RunError::Refused)?;
        if matches!(image.format, Format::L8 | Format::Index8) {
            return unsupported(format!(
                "fill of {:?}: VGLite converts the colour to luma with a truncating float sum; \
                 fill a mask as A8",
                image.format
            ))
            .map_err(RunError::Refused);
        }
        self.run(
            now_ns,
            None,
            image,
            out,
            true,
            Op::Clear {
                rect,
                color: plan::color(rgba),
            },
        )
    }

    /// Set `rect` (left, top, width, height) to `rgba` (`vg_lite_clear`). An A8 mask takes the
    /// alpha byte. L8 is `Unsupported`.
    pub fn fill_bytes(
        &mut self,
        now_ns: u64,
        image: Image,
        buf: &mut [u8],
        rect: (u32, u32, u32, u32),
        rgba: [u8; 4],
    ) -> Result<Transfer, RunError> {
        checked(image, buf.len())?;
        self.fill(now_ns, image, Output::Bytes(buf), rect, rgba)
    }

    /// [`Vglite::fill_bytes`] on a dma-buf.
    pub fn fill_dmabuf(
        &mut self,
        now_ns: u64,
        frame: Frame<'_>,
        rect: (u32, u32, u32, u32),
        rgba: [u8; 4],
    ) -> Result<Transfer, RunError> {
        self.fill(now_ns, frame.image, Output::Frame(frame), rect, rgba)
    }

    #[allow(clippy::too_many_arguments)]
    fn blend(
        &mut self,
        now_ns: u64,
        fg: Image,
        input: Input<'_>,
        dst: Image,
        out: Output<'_>,
        mode: BlendMode,
        premultiplied: bool,
    ) -> Result<Transfer, RunError> {
        check_output(dst, (fg.width, fg.height), fg.format).map_err(RunError::Refused)?;
        if !matches!(fg.format, Format::Rgba8888 | Format::Bgra8888) {
            return unsupported(format!("blend of {:?}: RGBA or BGRA only", fg.format))
                .map_err(RunError::Refused);
        }
        let blend = plan::blend(mode, premultiplied).map_err(RunError::Refused)?;
        self.blit(
            now_ns,
            (fg, input),
            dst,
            out,
            true,
            IDENTITY,
            blend,
            Sampling::Point,
        )
    }

    /// `fg` over (or under) `dst` of the same size and format, in place: SRC, SRC_OVER or
    /// DST_OVER with straight alpha. Premultiplied SRC_OVER and DST_OVER are `Unsupported`.
    #[allow(clippy::too_many_arguments)]
    pub fn blend_bytes(
        &mut self,
        now_ns: u64,
        fg: Image,
        fg_bytes: &[u8],
        dst: Image,
        dst_bytes: &mut [u8],
        mode: BlendMode,
        premultiplied: bool,
    ) -> Result<Transfer, RunError> {
        checked(fg, fg_bytes.len())?;
        checked(dst, dst_bytes.len())?;
        self.blend(
            now_ns,
            fg,
            Input::Bytes(fg_bytes),
            dst,
            Output::Bytes(dst_bytes),
            mode,
            premultiplied,
        )
    }

    /// [`Vglite::blend_bytes`] between dma-bufs.
    pub fn blend_dmabuf(
        &mut self,
        now_ns: u64,
        fg: Frame<'_>,
        dst: Frame<'_>,
        mode: BlendMode,
        premultiplied: bool,
    ) -> Result<Transfer, RunError> {
        self.blend(
            now_ns,
            fg.image,
            Input::Frame(fg),
            dst.image,
            Output::Frame(dst),
            mode,
            premultiplied,
        )
    }

    fn palette(
        &mut self,
        now_ns: u64,
        indices: Image,
        input: Input<'_>,
        lut: &[[u8; 4]; 256],
        dst: Image,
        out: Output<'_>,
    ) -> Result<Transfer, RunError> {
        if indices.format != Format::Index8 {
            return invalid(format!(
                "palette indices as {:?}, not Index8",
                indices.format
            ))
            .map_err(RunError::Refused);
        }
        check_output(dst, (indices.width, indices.height), Format::Rgba8888)
            .map_err(RunError::Refused)?;
        let colours: [u32; 256] = std::array::from_fn(|i| plan::color(lut[i]));
        self.run(
            now_ns,
            Some((indices, input)),
            dst,
            out,
            false,
            Op::Palette { colours: &colours },
        )
    }

    /// 8-bit indices to RGBA8 through 256 entries (an `INDEX_8` source and the colour table),
    /// tightly packed byte buffers.
    pub fn palette_bytes(
        &mut self,
        now_ns: u64,
        (width, height): (u32, u32),
        indices: &[u8],
        lut: &[[u8; 4]; 256],
        dst: &mut [u8],
    ) -> Result<Transfer, RunError> {
        let (s, d) = (
            Image::packed(Format::Index8, width, height),
            Image::packed(Format::Rgba8888, width, height),
        );
        checked(s, indices.len())?;
        checked(d, dst.len())?;
        self.palette(now_ns, s, Input::Bytes(indices), lut, d, Output::Bytes(dst))
    }

    /// [`Vglite::palette_bytes`] between dma-bufs (`indices` as [`Format::Index8`]).
    pub fn palette_dmabuf(
        &mut self,
        now_ns: u64,
        indices: Frame<'_>,
        lut: &[[u8; 4]; 256],
        dst: Frame<'_>,
    ) -> Result<Transfer, RunError> {
        self.palette(
            now_ns,
            indices.image,
            Input::Frame(indices),
            lut,
            dst.image,
            Output::Frame(dst),
        )
    }

    fn line(
        &mut self,
        now_ns: u64,
        image: Image,
        out: Output<'_>,
        (from, to): ((i32, i32), (i32, i32)),
        rgba: [u8; 4],
    ) -> Result<Transfer, RunError> {
        if matches!(image.format, Format::L8 | Format::Index8) {
            return unsupported(format!(
                "line on {:?}: VGLite converts the colour to luma with a truncating float sum",
                image.format
            ))
            .map_err(RunError::Refused);
        }
        // Pixel (x, y) covers [x, x + 1) by [y, y + 1), so its centre is at (x + 0.5, y + 0.5).
        // A width-1 stroke centred there covers exactly that pixel row or column when the line
        // is axis-aligned, and a square cap extends each end by half a pixel, so both ends are
        // included. The draw is not antialiased (quality LOW): a pixel is set when its centre is
        // inside the stroke. That covers every Bresenham pixel, and on other slopes it can cover
        // more (a neighbour whose centre is within half a width of the line): not proven equal
        // to Bresenham, see `tests/lines.rs`.
        const LIMIT: i32 = 1 << 20;
        let bad = |p: (i32, i32)| p.0.abs() > LIMIT || p.1.abs() > LIMIT;
        if bad(from) || bad(to) {
            return invalid(format!("line {from:?} to {to:?}: coordinates beyond 2^20"))
                .map_err(RunError::Refused);
        }
        let points = [
            (from.0 as f32 + 0.5, from.1 as f32 + 0.5),
            (to.0 as f32 + 0.5, to.1 as f32 + 0.5),
        ];
        // A miter limit of 4, which does not matter for two points.
        let stroke = Stroke {
            width: 1.0,
            cap: Cap::Square,
            join: Join::Miter,
            miter_limit: 4.0,
            closed: false,
        };
        self.draw_outline(now_ns, image, out, &points, stroke, Subpath::Open, rgba)
    }

    /// The stroke of the polyline through `points` (pixel coordinates: pixel (x, y) covers
    /// [x, x + 1) by [y, y + 1)), drawn in `rgba` without antialiasing over the rest of the
    /// image, which is kept: `vg_lite_update_stroke`'s outline, then `vg_lite_draw`'s stroke.
    /// At most [`MAX_STROKE_POINTS`] points, each within 2^20 of the origin; the outline must fit
    /// one command buffer (`OutOfMemory` otherwise). Round caps and joins are `Unsupported`, as
    /// is L8, as for fills.
    pub fn stroke_bytes(
        &mut self,
        now_ns: u64,
        image: Image,
        buf: &mut [u8],
        points: &[(f32, f32)],
        stroke: Stroke,
        rgba: [u8; 4],
    ) -> Result<Transfer, RunError> {
        checked(image, buf.len())?;
        self.stroke(now_ns, image, Output::Bytes(buf), points, stroke, rgba)
    }

    /// [`Vglite::stroke_bytes`] on a dma-buf.
    pub fn stroke_dmabuf(
        &mut self,
        now_ns: u64,
        frame: Frame<'_>,
        points: &[(f32, f32)],
        stroke: Stroke,
        rgba: [u8; 4],
    ) -> Result<Transfer, RunError> {
        self.stroke(
            now_ns,
            frame.image,
            Output::Frame(frame),
            points,
            stroke,
            rgba,
        )
    }

    fn stroke(
        &mut self,
        now_ns: u64,
        image: Image,
        out: Output<'_>,
        points: &[(f32, f32)],
        stroke: Stroke,
        rgba: [u8; 4],
    ) -> Result<Transfer, RunError> {
        if matches!(image.format, Format::L8 | Format::Index8) {
            return unsupported(format!(
                "stroke on {:?}: VGLite converts the colour to luma with a truncating float sum",
                image.format
            ))
            .map_err(RunError::Refused);
        }
        if points.len() > MAX_STROKE_POINTS {
            return invalid(format!(
                "{} points; a stroke takes at most {MAX_STROKE_POINTS}",
                points.len()
            ))
            .map_err(RunError::Refused);
        }
        const LIMIT: f32 = (1 << 20) as f32;
        if !points
            .iter()
            .all(|&(x, y)| x.abs() <= LIMIT && y.abs() <= LIMIT)
        {
            return invalid("stroke points beyond 2^20, or not finite".into())
                .map_err(RunError::Refused);
        }
        let subpath = if stroke.closed {
            Subpath::Closed
        } else {
            Subpath::Open
        };
        self.draw_outline(now_ns, image, out, points, stroke, subpath, rgba)
    }

    /// The outline of `points` as `stroke`, drawn over `image`.
    #[allow(clippy::too_many_arguments)]
    fn draw_outline(
        &mut self,
        now_ns: u64,
        image: Image,
        out: Output<'_>,
        points: &[(f32, f32)],
        stroke: Stroke,
        subpath: Subpath,
        rgba: [u8; 4],
    ) -> Result<Transfer, RunError> {
        let mut outliner = std::mem::take(&mut self.outliner);
        let result = match outliner.outline(
            points,
            stroke.width,
            stroke.cap,
            stroke.join,
            stroke.miter_limit,
            subpath,
        ) {
            Ok(outline) => self.run(
                now_ns,
                None,
                image,
                out,
                true,
                Op::Stroke {
                    outline,
                    colour: plan::color(rgba),
                },
            ),
            Err(error) => Err(RunError::Refused(error)),
        };
        self.outliner = outliner;
        result
    }

    /// A one-pixel line from `from` to `to`, both ends included, in `rgba`: a width-1 stroke
    /// with square caps, drawn without antialiasing through the GPU's path draw, over the rest
    /// of the image, which is kept. Points beyond the image are clipped by the GPU (unverified on
    /// board); L8 is `Unsupported`, as for fills. Not proven equal to Bresenham on board
    /// (`tests/lines.rs`).
    pub fn line_bytes(
        &mut self,
        now_ns: u64,
        image: Image,
        buf: &mut [u8],
        from: (i32, i32),
        to: (i32, i32),
        rgba: [u8; 4],
    ) -> Result<Transfer, RunError> {
        checked(image, buf.len())?;
        self.line(now_ns, image, Output::Bytes(buf), (from, to), rgba)
    }

    /// [`Vglite::line_bytes`] on a dma-buf.
    pub fn line_dmabuf(
        &mut self,
        now_ns: u64,
        frame: Frame<'_>,
        from: (i32, i32),
        to: (i32, i32),
        rgba: [u8; 4],
    ) -> Result<Transfer, RunError> {
        self.line(now_ns, frame.image, Output::Frame(frame), (from, to), rgba)
    }
}

/// `image` is one VGLite takes and `len` bytes hold it.
fn checked(image: Image, len: usize) -> Result<(), RunError> {
    image.check(len).map_err(RunError::Refused)
}

/// One operation on an open context (see [`Vglite::run`]).
fn operation(
    context: &mut Context,
    src: Option<(Image, Input<'_>)>,
    dst: Image,
    out: &mut Output<'_>,
    keep: bool,
    op: Op<'_>,
) -> Result<Transfer, Fault> {
    let device = context.device().clone();
    let mut transfer = Transfer::default();
    // The images were checked against byte slices by the public calls; a frame's image is
    // checked against its dma-buf when it is mapped. Both are checked here before anything is
    // allocated for them.
    if let Some((image, _)) = &src {
        image.check(usize::MAX)?;
    }
    dst.check(usize::MAX)?;
    let source = match &src {
        Some((image, input)) => {
            let mut surface = Surface::staging(&device, *image).map_err(kernel_fault)?;
            surface.upload(input, &mut transfer)?;
            Some(surface)
        }
        None => None,
    };
    let mut target = Surface::staging(&device, dst).map_err(kernel_fault)?;
    if keep {
        target.upload(&out.as_input(), &mut transfer)?;
    }
    let mut stream = context.stream();
    let drew = command::encode(
        op,
        &mut stream,
        target.bound(),
        source.as_ref().map(Surface::bound),
    )
    .and_then(|drew| {
        if drew {
            stream.finish()?;
        }
        Ok(drew)
    });
    match drew {
        Ok(true) => {}
        Ok(false) => {
            context.recycle(stream);
            return Ok(transfer);
        }
        Err(error) => {
            context.recycle(stream);
            return Err(Fault::Caller(error));
        }
    }
    match context.run(stream) {
        Ok(()) => {}
        Err(SysRunError::NotRun(error)) => return Err(kernel_fault(error)),
        Err(SysRunError::Wait(error)) => {
            // The GPU may still read and write these: never free them.
            std::mem::forget(target.into_memory());
            if let Some(source) = source {
                std::mem::forget(source.into_memory());
            }
            return Err(Fault::Wait(error));
        }
    }
    target.sync_after_gpu();
    target.download(out, &mut transfer)?;
    Ok(transfer)
}

/// Open the device, initialize the context, check the chip, and run the set-up stream.
fn open(path: &std::path::Path) -> io::Result<Context> {
    let device = Device::open(path)?;
    let mut context = device.initialize(COMMAND_BUFFER_BYTES, TESS, TESS)?;
    let id = device.identification(Identification::ChipId)?;
    let revision = device.identification(Identification::Revision)?;
    let cid = device.identification(Identification::Cid)?;
    if (id, revision, cid) != (CHIP_ID, REVISION, CID) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("not the expected VGLite: id {id:#x}, revision {revision:#x}, cid {cid:#x}"),
        ));
    }
    let mut stream = context.stream();
    command::init(&mut stream)?;
    stream.finish()?;
    match context.run(stream) {
        Ok(()) => Ok(context),
        Err(SysRunError::NotRun(error)) => Err(error),
        Err(SysRunError::Wait(error)) => {
            // The set-up stream names no buffer of ours, but the command buffer is the
            // context's: leak it rather than release it under a running GPU.
            std::mem::forget(context);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backoff_doubles_from_a_quarter_second_to_eight() {
        let delays: Vec<u64> = (0..9).map(backoff_ns).collect();
        assert_eq!(
            delays,
            [250, 500, 1000, 2000, 4000, 8000, 8000, 8000, 8000].map(|ms| ms * 1_000_000)
        );
        assert_eq!(backoff_ns(u32::MAX), 8_000_000_000);
    }

    #[test]
    fn a_missing_device_backs_off_and_never_gives_up() {
        let dir = std::env::temp_dir().join(format!("vglite-missing-{}", std::process::id()));
        let mut gpu = Vglite::new(dir.join("vg_lite"));
        assert_eq!(gpu.state(), State::Closed);
        let image = Image::packed(Format::Rgba8888, 4, 4);
        let mut buf = vec![0u8; image.frame_len()];
        let mut fill =
            |gpu: &mut Vglite, now| gpu.fill_bytes(now, image, &mut buf, (0, 0, 4, 4), [1; 4]);
        // The first call tries to open and fails; the next one within the delay is not tried.
        match fill(&mut gpu, 0) {
            Err(RunError::Device { error, retry_at_ns }) => {
                assert_eq!(error.kind(), io::ErrorKind::NotFound);
                assert_eq!(retry_at_ns, 250_000_000);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            fill(&mut gpu, 100),
            Err(RunError::BackingOff {
                retry_at_ns: 250_000_000
            })
        ));
        // Each failure doubles the delay, to 8 s, and it stays there.
        let mut now = 250_000_000;
        let mut last = 0;
        for _ in 0..10 {
            match fill(&mut gpu, now) {
                Err(RunError::Device { retry_at_ns, .. }) => {
                    last = retry_at_ns - now;
                    now = retry_at_ns;
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(last, 8_000_000_000);
        assert!(matches!(
            gpu.state(),
            State::BackingOff { failures: 11, .. }
        ));
        // Stopping clears the backoff: the next call tries at once.
        gpu.stop();
        assert_eq!(gpu.state(), State::Closed);
        assert!(matches!(
            fill(&mut gpu, now + 1),
            Err(RunError::Device { .. })
        ));
    }

    #[test]
    fn what_the_caller_asked_wrongly_never_touches_the_device() {
        // A path that cannot be opened: had any of these reached it, the answer would be a
        // device failure.
        let mut gpu = Vglite::new("/nonexistent/vg_lite");
        let rgba = Image::packed(Format::Rgba8888, 4, 4);
        let mut buf = vec![0u8; rgba.frame_len()];
        let refused = |r: Result<Transfer, RunError>| match r {
            Err(RunError::Refused(e)) => e.kind(),
            other => panic!("{other:?}"),
        };
        // A buffer too short, a rectangle outside, an L8 fill, a 3x area resize, a premultiplied
        // blend, a line past the coordinate limit, an image past the 16-bit sides.
        assert_eq!(
            refused(gpu.fill_bytes(0, rgba, &mut buf[1..], (0, 0, 1, 1), [0; 4])),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            refused(gpu.fill_bytes(0, rgba, &mut buf, (3, 0, 2, 1), [0; 4])),
            io::ErrorKind::InvalidInput
        );
        let l8 = Image::packed(Format::L8, 4, 4);
        let mut luma = vec![0u8; l8.frame_len()];
        assert_eq!(
            refused(gpu.fill_bytes(0, l8, &mut luma, (0, 0, 1, 1), [0; 4])),
            io::ErrorKind::Unsupported
        );
        let third = Image::packed(Format::Rgba8888, 1, 1);
        let mut small = vec![0u8; third.frame_len()];
        assert_eq!(
            refused(gpu.resize_bytes(0, rgba, &buf.clone(), third, &mut small, Filter::Area)),
            io::ErrorKind::Unsupported
        );
        let fg = buf.clone();
        assert_eq!(
            refused(gpu.blend_bytes(0, rgba, &fg, rgba, &mut buf, BlendMode::SrcOver, true)),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            refused(gpu.line_bytes(0, rgba, &mut buf, (0, 0), (1 << 21, 0), [0; 4])),
            io::ErrorKind::InvalidInput
        );
        let wide = Image::packed(Format::A8, 1 << 16, 1);
        let mut row = vec![0u8; 1 << 16];
        assert_eq!(
            refused(gpu.fill_bytes(0, wide, &mut row, (0, 0, 1, 1), [0; 4])),
            io::ErrorKind::InvalidInput
        );
        // Strokes: too many points, a point not finite or too far, a round cap or join, a
        // width that is not positive.
        let stroke = Stroke {
            width: 1.0,
            cap: Cap::Butt,
            join: Join::Miter,
            miter_limit: 4.0,
            closed: false,
        };
        let many = vec![(1.0, 1.0); MAX_STROKE_POINTS + 1];
        assert_eq!(
            refused(gpu.stroke_bytes(0, rgba, &mut buf, &many, stroke, [0; 4])),
            io::ErrorKind::InvalidInput
        );
        for points in [[(0.0, 0.0), (f32::NAN, 1.0)], [(0.0, 0.0), (2e6, 1.0)]] {
            assert_eq!(
                refused(gpu.stroke_bytes(0, rgba, &mut buf, &points, stroke, [0; 4])),
                io::ErrorKind::InvalidInput
            );
        }
        let two = [(0.5, 0.5), (3.5, 2.5)];
        for bad in [
            Stroke {
                cap: Cap::Round,
                ..stroke
            },
            Stroke {
                join: Join::Round,
                ..stroke
            },
        ] {
            assert_eq!(
                refused(gpu.stroke_bytes(0, rgba, &mut buf, &two, bad, [0; 4])),
                io::ErrorKind::Unsupported
            );
        }
        assert_eq!(
            refused(gpu.stroke_bytes(
                0,
                rgba,
                &mut buf,
                &two,
                Stroke {
                    width: 0.0,
                    ..stroke
                },
                [0; 4]
            )),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(gpu.state(), State::Closed);
    }
}
