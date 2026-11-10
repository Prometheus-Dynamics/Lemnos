//! The image contracts' jobs (`image.scaler/1`, `image.convert/1`, `image.blend/1`), run on the
//! CPU route.
//!
//! [`Jobs`] takes one decoded job and the bytes of the buffers it names, checks it, runs it and
//! answers the bytes written, or 0 for a job it refuses, as the contracts' completions carry
//! them. It keeps the contracts' counters. Nothing in it allocates once it is made: its working
//! rows are sized for [`Settings::max_width`] up front.
//!
//! What a job names, and what bounds it: each image's buffer is the slice the caller hands in
//! with the job, so a job can only reach memory the caller holds. Within a slice, an image's
//! extent (its stride times its rows, over every plane of its format) must fit, with
//! overflow-free arithmetic, and its stride must hold a row; its width and height must lie in
//! `1..=` the configured maximum. A job whose buffers are not distinct is refused, as the
//! contracts require.
//!
//! The hardware routes are not used here: a job's buffers are frame-stream buffers, and a unit
//! needs them as dma-bufs, which the stream endpoint does not hand a driver (see the crate docs).

use lemnos_contract_image_blend::{self as blend, BlendJob, BlendOp as JobBlendOp};
use lemnos_contract_image_convert::{self as convert, ConvertJob};
use lemnos_contract_image_scaler::{self as scaler, Filter as JobFilter, ScaleJob};

use crate::cpu::{self, BlendOp, Matrix, Sampling, Scratch};
use crate::{Filter, Layout, PixelFormat};

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

    /// The layout of `image`: a known format, a size inside the limits, a stride that holds a
    /// row, and an extent that fits a buffer of `len` bytes.
    fn layout(&self, image: Image, len: usize) -> Option<Layout> {
        let format = PixelFormat::from_fourcc(image.fourcc)?;
        let sized = (1..=self.settings.max_width).contains(&image.width)
            && (1..=self.settings.max_height).contains(&image.height);
        let layout = Layout {
            format,
            width: image.width,
            height: image.height,
            stride: image.stride,
        };
        let row = u64::from(format.bytes()) * u64::from(image.width);
        let fits = u64::from(image.stride) >= row && layout.frame_len() <= len;
        (sized && fits).then_some(layout)
    }

    /// Bytes an output of `layout` holds, as a completion carries them.
    fn written(layout: Layout) -> Option<u32> {
        u32::try_from(layout.frame_len()).ok()
    }

    /// Resizes `input` into `output`, the buffers `job` names. Returns the bytes written, or 0
    /// for a job refused.
    pub fn scale(&mut self, job: &ScaleJob, input: &[u8], output: &mut [u8]) -> u32 {
        let written = self.try_scale(job, input, output);
        self.scale.count(written)
    }

    fn try_scale(&mut self, job: &ScaleJob, input: &[u8], output: &mut [u8]) -> Option<u32> {
        let (i, o) = (Image::from(job.input), Image::from(job.output));
        if i.buffer == o.buffer || i.fourcc != o.fourcc {
            return None;
        }
        let (src, dst) = (self.layout(i, input.len())?, self.layout(o, output.len())?);
        let filter = match job.filter {
            JobFilter::Area => Filter::Area,
            JobFilter::Bilinear => Filter::Bilinear,
        };
        let sampling = self.settings.sampling;
        cpu::resize(src, input, dst, output, filter, sampling, &mut self.scratch).ok()?;
        Self::written(dst)
    }

    /// Converts `input` into `output`, the buffers `job` names. Returns the bytes written, or 0
    /// for a job refused.
    pub fn convert(&mut self, job: &ConvertJob, input: &[u8], output: &mut [u8]) -> u32 {
        let written = self.try_convert(job, input, output);
        self.convert.count(written)
    }

    fn try_convert(&mut self, job: &ConvertJob, input: &[u8], output: &mut [u8]) -> Option<u32> {
        let (i, o) = (Image::from(job.input), Image::from(job.output));
        if i.buffer == o.buffer {
            return None;
        }
        let (src, dst) = (self.layout(i, input.len())?, self.layout(o, output.len())?);
        let matrix = self.settings.matrix;
        cpu::convert(src, input, dst, output, matrix, &mut self.scratch).ok()?;
        Self::written(dst)
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
        let written = self.try_blend(job, foreground, background, output);
        self.blend.count(written)
    }

    fn try_blend(
        &mut self,
        job: &BlendJob,
        foreground: &[u8],
        background: &[u8],
        output: &mut [u8],
    ) -> Option<u32> {
        let (f, b, o) = (
            Image::from(job.foreground),
            Image::from(job.background),
            Image::from(job.output),
        );
        if f.buffer == b.buffer || f.buffer == o.buffer || b.buffer == o.buffer {
            return None;
        }
        let fg = self.layout(f, foreground.len())?;
        let bg = self.layout(b, background.len())?;
        let out = self.layout(o, output.len())?;
        let op = match job.op {
            JobBlendOp::Src => BlendOp::Src,
            JobBlendOp::SrcOver => BlendOp::SrcOver,
            JobBlendOp::DstOver => BlendOp::DstOver,
        };
        cpu::blend(op, fg, foreground, bg, background, out, output).ok()?;
        Self::written(out)
    }
}
