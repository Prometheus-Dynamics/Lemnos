//! The image contracts' jobs (`image.scaler/1`, `image.convert/1`, `image.blend/1`): their
//! checks, and the CPU route that runs them.
//!
//! [`Jobs`] takes one decoded job and the bytes of the buffers it names, checks it, runs it and
//! answers the bytes written, or 0 for a job it refuses, as the contracts' completions carry
//! them. It keeps the contracts' counters. Its working rows are sized for
//! [`Settings::max_width`] up front, so a job allocates nothing.
//!
//! What a job names, and what bounds it: each image's buffer is the slice the caller hands in
//! with the job, so a job can only reach memory the caller holds. Within a slice, an image's
//! extent (its stride times its rows, over every plane of its format) must fit, with
//! overflow-free arithmetic, and its stride must hold a row; its width and height must lie in
//! `1..=` the configured maximum. A job whose buffers are not distinct is refused, as the
//! contracts require, and so is one whose sides differ where its contract says they match: a
//! conversion keeps the size, a resize keeps the format, a blend's three images share size and
//! format.
//!
//! The job streams (see the crate docs) check a job here first, as a [`Plan`], and then choose
//! the route that runs it.

use lemnos_contract_image_blend::{self as blend, BlendJob, BlendOp as JobBlendOp};
use lemnos_contract_image_convert::{self as convert, ConvertJob};
use lemnos_contract_image_scaler::{self as scaler, Filter as JobFilter, ScaleJob};

use crate::cpu::{self, BlendOp, Matrix, Sampling, Scratch};
use crate::{BLEND_FORMATS, Filter, Layout, PixelFormat};

/// How jobs are run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The largest width of an image in a job, in pixels.
    pub max_width: u32,
    /// The largest height of an image in a job, in pixels.
    pub max_height: u32,
    /// The matrix where RGB and YUV meet in a conversion.
    pub matrix: Matrix,
    /// The bilinear phase of a resize.
    pub sampling: Sampling,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_width: 8192,
            max_height: 8192,
            matrix: Matrix::Bt601Limited,
            sampling: Sampling::Centres,
        }
    }
}

/// One side of a job, as the contracts give it.
#[derive(Clone, Copy)]
struct Image {
    buffer: u32,
    width: u32,
    height: u32,
    fourcc: u32,
    stride: u32,
}

impl From<scaler::ImageRef> for Image {
    fn from(i: scaler::ImageRef) -> Self {
        Self {
            buffer: i.buffer,
            width: i.width,
            height: i.height,
            fourcc: i.fourcc,
            stride: i.stride,
        }
    }
}

impl From<convert::ImageRef> for Image {
    fn from(i: convert::ImageRef) -> Self {
        Self {
            buffer: i.buffer,
            width: i.width,
            height: i.height,
            fourcc: i.fourcc,
            stride: i.stride,
        }
    }
}

impl From<blend::ImageRef> for Image {
    fn from(i: blend::ImageRef) -> Self {
        Self {
            buffer: i.buffer,
            width: i.width,
            height: i.height,
            fourcc: i.fourcc,
            stride: i.stride,
        }
    }
}

/// The counters of one contract: jobs completed with an output, and jobs refused.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub jobs: u64,
    pub errors: u64,
}

impl Counts {
    fn count(&mut self, written: Option<u32>) -> u32 {
        match written {
            Some(n) if n > 0 => {
                self.jobs = self.jobs.saturating_add(1);
                n
            }
            _ => {
                self.errors = self.errors.saturating_add(1);
                0
            }
        }
    }
}

/// Which contract a job came on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Contract {
    Scale,
    Convert,
    Blend,
}

/// What a job does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Work {
    Scale(Filter),
    Convert,
    Blend(BlendOp),
}

/// One image of a checked job: its buffer and its layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Side {
    pub buffer: u32,
    pub layout: Layout,
}

/// A job checked as far as the job alone allows: formats known, sizes in the limits, strides
/// that hold a row, distinct buffers, and the sides its contract says match. What it still
/// needs is buffers that hold each extent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub work: Work,
    /// The input; for a blend, the foreground.
    pub input: Side,
    /// A blend's background.
    pub second: Option<Side>,
    pub output: Side,
}

impl Plan {
    /// The images it reads, in order.
    pub fn inputs(&self) -> impl Iterator<Item = Side> {
        core::iter::once(self.input).chain(self.second)
    }

    /// Every image it names.
    pub fn sides(&self) -> impl Iterator<Item = Side> {
        self.inputs().chain(core::iter::once(self.output))
    }
}

/// Copies of a job's images in the driver's own memory, for the routes that work on bytes: the
/// consumer shares a stream's buffers and can change them at any time, so an input is read
/// once into here and only this copy is used. Each grows to the largest image it has held and
/// is kept, so jobs of sizes seen before allocate nothing. An image only lands here once its
/// extent was found to fit its stream buffer, so they never outgrow what the stream holds.
#[derive(Debug, Default)]
pub(crate) struct Frames {
    pub input: Vec<u8>,
    pub second: Vec<u8>,
    pub output: Vec<u8>,
}

/// The first `len` bytes of `frame`, grown to hold them.
pub(crate) fn sized(frame: &mut Vec<u8>, len: usize) -> &mut [u8] {
    if frame.len() < len {
        frame.resize(len, 0);
    }
    &mut frame[..len]
}

/// Runs jobs, and counts them.
#[derive(Debug)]
pub struct Jobs {
    settings: Settings,
    scratch: Scratch,
    scale: Counts,
    convert: Counts,
    blend: Counts,
}

impl Jobs {
    /// Jobs up to `settings`' sizes, with working rows for the widest.
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            scratch: Scratch::with_width(settings.max_width as usize),
            scale: Counts::default(),
            convert: Counts::default(),
            blend: Counts::default(),
        }
    }

    /// The settings jobs run with.
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Counted resizes.
    pub fn scale_counts(&self) -> Counts {
        self.scale
    }

    /// Counted conversions.
    pub fn convert_counts(&self) -> Counts {
        self.convert
    }

    /// Counted blends.
    pub fn blend_counts(&self) -> Counts {
        self.blend
    }

    /// Counts one job of `contract` that wrote `written` bytes, or failed (`None` or 0).
    pub(crate) fn count(&mut self, contract: Contract, written: Option<u32>) -> u32 {
        match contract {
            Contract::Scale => self.scale.count(written),
            Contract::Convert => self.convert.count(written),
            Contract::Blend => self.blend.count(written),
        }
    }

    /// The layout of `image`: a known format, a size inside the limits, and a stride that
    /// holds a row.
    fn layout(&self, image: Image) -> Option<Layout> {
        let format = PixelFormat::from_fourcc(image.fourcc)?;
        let sized = (1..=self.settings.max_width).contains(&image.width)
            && (1..=self.settings.max_height).contains(&image.height);
        let row = u64::from(format.bytes()) * u64::from(image.width);
        let layout = Layout {
            format,
            width: image.width,
            height: image.height,
            stride: image.stride,
        };
        (sized && u64::from(image.stride) >= row).then_some(layout)
    }

    fn side(&self, image: Image) -> Option<Side> {
        Some(Side {
            buffer: image.buffer,
            layout: self.layout(image)?,
        })
    }

    /// Whether `layout`'s extent fits a buffer of `len` bytes.
    pub(crate) fn fits(layout: Layout, len: usize) -> bool {
        layout.frame_len() <= len
    }

    /// Bytes an output of `layout` holds, as a completion carries them.
    pub(crate) fn written(layout: Layout) -> Option<u32> {
        u32::try_from(layout.frame_len()).ok()
    }

    /// `job` checked as a resize.
    pub(crate) fn plan_scale(&self, job: &ScaleJob) -> Option<Plan> {
        let (i, o) = (Image::from(job.input), Image::from(job.output));
        if i.buffer == o.buffer || i.fourcc != o.fourcc {
            return None;
        }
        let filter = match job.filter {
            JobFilter::Area => Filter::Area,
            JobFilter::Bilinear => Filter::Bilinear,
        };
        Some(Plan {
            work: Work::Scale(filter),
            input: self.side(i)?,
            second: None,
            output: self.side(o)?,
        })
    }

    /// `job` checked as a conversion.
    pub(crate) fn plan_convert(&self, job: &ConvertJob) -> Option<Plan> {
        let (i, o) = (Image::from(job.input), Image::from(job.output));
        if i.buffer == o.buffer || (i.width, i.height) != (o.width, o.height) {
            return None;
        }
        Some(Plan {
            work: Work::Convert,
            input: self.side(i)?,
            second: None,
            output: self.side(o)?,
        })
    }

    /// `job` checked as a blend.
    pub(crate) fn plan_blend(&self, job: &BlendJob) -> Option<Plan> {
        let (f, b, o) = (
            Image::from(job.foreground),
            Image::from(job.background),
            Image::from(job.output),
        );
        if f.buffer == b.buffer || f.buffer == o.buffer || b.buffer == o.buffer {
            return None;
        }
        let shape = |i: Image| (i.width, i.height, i.fourcc);
        if shape(f) != shape(b) || shape(f) != shape(o) {
            return None;
        }
        let op = match job.op {
            JobBlendOp::Src => BlendOp::Src,
            JobBlendOp::SrcOver => BlendOp::SrcOver,
            JobBlendOp::DstOver => BlendOp::DstOver,
        };
        let plan = Plan {
            work: Work::Blend(op),
            input: self.side(f)?,
            second: Some(self.side(b)?),
            output: self.side(o)?,
        };
        BLEND_FORMATS
            .contains(&plan.output.layout.format)
            .then_some(plan)
    }

    /// Runs `plan` on the CPU: `input` (and `second`, for a blend) into `output`. The bytes
    /// written, or `None` when a buffer can't hold its image or the work is refused.
    pub(crate) fn run_cpu(
        &mut self,
        plan: &Plan,
        input: &[u8],
        second: &[u8],
        output: &mut [u8],
    ) -> Option<u32> {
        let (src, dst) = (plan.input.layout, plan.output.layout);
        if !Self::fits(src, input.len()) || !Self::fits(dst, output.len()) {
            return None;
        }
        match plan.work {
            Work::Scale(filter) => {
                let sampling = self.settings.sampling;
                cpu::resize(src, input, dst, output, filter, sampling, &mut self.scratch).ok()?;
            }
            Work::Convert => {
                let matrix = self.settings.matrix;
                cpu::convert(src, input, dst, output, matrix, &mut self.scratch).ok()?;
            }
            Work::Blend(op) => {
                let bg = plan.second?.layout;
                if !Self::fits(bg, second.len()) {
                    return None;
                }
                cpu::blend(op, src, input, bg, second, dst, output).ok()?;
            }
        }
        Self::written(dst)
    }

    /// Resizes `input` into `output`, the buffers `job` names. Returns the bytes written, or 0
    /// for a job refused.
    pub fn scale(&mut self, job: &ScaleJob, input: &[u8], output: &mut [u8]) -> u32 {
        let written = self
            .plan_scale(job)
            .and_then(|plan| self.run_cpu(&plan, input, &[], output));
        self.scale.count(written)
    }

    /// Converts `input` into `output`, the buffers `job` names. Returns the bytes written, or 0
    /// for a job refused.
    pub fn convert(&mut self, job: &ConvertJob, input: &[u8], output: &mut [u8]) -> u32 {
        let written = self
            .plan_convert(job)
            .and_then(|plan| self.run_cpu(&plan, input, &[], output));
        self.convert.count(written)
    }

    /// Composites `foreground` and `background` into `output`, the buffers `job` names.
    /// Returns the bytes written, or 0 for a job refused.
    pub fn blend(
        &mut self,
        job: &BlendJob,
        foreground: &[u8],
        background: &[u8],
        output: &mut [u8],
    ) -> u32 {
        let written = self
            .plan_blend(job)
            .and_then(|plan| self.run_cpu(&plan, foreground, background, output));
        self.blend.count(written)
    }
}
