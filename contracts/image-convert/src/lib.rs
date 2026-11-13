//! The `image.convert/1` contract: a fixed-function block that changes an image's pixel format,
//! given as jobs on a ring.
//!
//! Lemnos does not read a pixel. A consumer opens the stream named by [`ImageConvert::job_stream`].
//! Its buffers hold the images, and its ring carries the jobs. A job is one [`ConvertJob`], encoded
//! with this crate's codec and posted on the stream's job queue. No socket is used per job.
//!
//! Completion is a frame on the same stream. Its buffer is the job's output, its `identity` is the
//! job's, and its length is the bytes written to the output. A length of 0 means the job failed,
//! and is counted in [`Counters::errors`]. Every job the block reads gets exactly one completion.
//! A job that cannot be decoded is a broken consumer: the block faults the stream, as it does for
//! any malformed ring, and completes nothing more on it.
//!
//! The two buffers of a job must be distinct. The consumer may reuse the input once that job's
//! completion is popped. The output buffer is released with the usual frame return.
//!
//! A conversion keeps the size: input and output have the same width and height. Resizing is the
//! scaler's job, so a caller that needs both runs the two blocks in turn.
//!
//! Units: `width` and `height` in pixels, `stride` and bytes in bytes, `fourcc` the little-endian
//! four-character code of the pixel format.

#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

/// Longest stream name [`ImageConvert::job_stream`] answers, in bytes.
pub const MAX_STREAM_NAME: usize = 32;

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
const IMAGE_BYTES: usize = 4 + 4 * 5;

/// One conversion the block makes.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConversionPair {
    /// The input format.
    pub from: u32,
    /// The output format.
    pub to: u32,
}

/// One conversion, posted on the job stream.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConvertJob {
    /// The caller's name for the job. Carried to the completion, never read.
    pub identity: u64,
    pub input: ImageRef,
    pub output: ImageRef,
}

/// Largest encoded [`ConvertJob`], in bytes. The job queue must hold jobs this large.
pub const JOB_BYTES: usize = 4 + 8 + 2 * IMAGE_BYTES;

/// What the block takes.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConvertInfo {
    /// The largest width and height of an image in a job.
    pub max_width: u32,
    pub max_height: u32,
}

/// Counted since the job stream was opened.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// Jobs completed with an output.
    pub jobs: u64,
    /// Jobs completed as failed: a size, conversion or buffer the block refused.
    pub errors: u64,
}

#[contract("image.convert", version = 1, kind = "compute")]
pub trait ImageConvert {
    #[method(1)]
    fn info(&mut self) -> Result<ConvertInfo>;
    /// How many conversions [`conversion`](Self::conversion) can return.
    #[method(2)]
    fn conversion_count(&mut self) -> Result<u16>;
    /// A conversion the block makes. `NOT_FOUND` past the last.
    #[method(3)]
    fn conversion(&mut self, index: u16) -> Result<ConversionPair>;
    /// The name to open the job stream by, as UTF-8 into `into`, returning its length. `into`
    /// holds at least [`MAX_STREAM_NAME`] bytes.
    #[method(4)]
    fn job_stream(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    #[method(5)]
    fn counters(&mut self) -> Result<Counters>;
}
