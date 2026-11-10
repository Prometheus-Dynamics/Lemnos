//! The `image.scaler/1` contract: a fixed-function block that resizes images, given as jobs on a
//! ring.
//!
//! Lemnos does not read a pixel. A consumer opens the stream named by [`ImageScaler::job_stream`].
//! Its buffers hold the images, and its ring carries the jobs. A job is one [`ScaleJob`], encoded
//! with this crate's codec and posted on the stream's job queue. No socket is used per job.
//!
//! Completion is a frame on the same stream. Its buffer is the job's output, its `identity` is the
//! job's, and its length is the bytes written to the output. A length of 0 means the job failed,
//! and is counted in [`Counters::errors`]. Every job the block reads gets exactly one completion.
//! A job that cannot be decoded is a broken consumer: the block faults the stream, as it does for
//! any malformed ring, and completes nothing more on it.
//!
//! The input buffer belongs to the block from the moment its job is posted until that job's
//! completion is popped. After that the consumer may reuse it. The output buffer is released with
//! the usual frame return.
//!
//! Units: `width` and `height` in pixels, `stride` and bytes in bytes, `fourcc` the little-endian
//! four-character code of the pixel format. Both sides of a job use the same formats; only the size
//! changes.

#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

/// Longest stream name [`ImageScaler::job_stream`] answers, in bytes.
pub const MAX_STREAM_NAME: usize = 32;

macro_rules! wire_enum {
    ($name:ident { $($variant:ident = $value:literal),* $(,)? }) => {
        impl Encode for $name {
            const SHAPE: lemnos_core::Shape = lemnos_core::Shape::U8;
            fn encode(&self, out: &mut lemnos_core::Writer<'_>) -> Result<()> {
                (*self as u8).encode(out)
            }
        }

        impl Decode<'_> for $name {
            fn decode(from: &mut lemnos_core::Reader<'_>) -> Result<Self> {
                match u8::decode(from)? {
                    $($value => Ok(Self::$variant),)*
                    _ => Err(lemnos_core::Status::INVALID_ARGUMENT),
                }
            }
        }
    };
}

/// An image in one buffer of the job stream.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImageRef {
    /// A buffer on the job stream.
    pub buffer: u32,
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    /// Bytes per line.
    pub stride: u32,
}

/// Encoded size of an [`ImageRef`].
const IMAGE_BYTES: usize = 4 * 5;

/// How a resize chooses each output pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Filter {
    /// Each output pixel is the mean of its block of input pixels.
    #[default]
    Area = 0,
    /// Bilinear interpolation.
    Bilinear = 1,
}

wire_enum!(Filter { Area = 0, Bilinear = 1 });

/// One resize, posted on the job stream.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScaleJob {
    /// The caller's name for the job. Carried to the completion, never read.
    pub identity: u64,
    pub input: ImageRef,
    pub output: ImageRef,
    pub filter: Filter,
}

/// Largest encoded [`ScaleJob`], in bytes. The job queue must hold jobs this large.
pub const JOB_BYTES: usize = 8 + 2 * IMAGE_BYTES + 1;

/// What the block takes.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScalerInfo {
    /// The largest width and height an image in a job may have, input or output.
    pub max_width: u32,
    pub max_height: u32,
}

/// Counted since the job stream was opened.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// Jobs completed with an output.
    pub jobs: u64,
    /// Jobs completed as failed: a size, format, filter or buffer the block refused.
    pub errors: u64,
}

#[contract("image.scaler", version = 1)]
pub trait ImageScaler {
    #[method(1)]
    fn info(&mut self) -> Result<ScalerInfo>;
    /// How many formats [`format`](Self::format) can return.
    #[method(2)]
    fn format_count(&mut self) -> Result<u16>;
    /// A format both the input and the output of a job may use. `NOT_FOUND` past the last.
    #[method(3)]
    fn format(&mut self, index: u16) -> Result<u32>;
    /// The name to open the job stream by, as UTF-8 into `into`, returning its length. `into`
    /// holds at least [`MAX_STREAM_NAME`] bytes.
    #[method(4)]
    fn job_stream(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    #[method(5)]
    fn counters(&mut self) -> Result<Counters>;
}
