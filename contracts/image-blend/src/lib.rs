//! The `image.blend/1` contract: a fixed-function block that composites one image over another,
//! given as jobs on a ring.
//!
//! Lemnos does not read a pixel. A consumer opens the stream named by [`ImageBlend::job_stream`].
//! Its buffers hold the images, and its ring carries the jobs. A job is one [`BlendJob`], encoded
//! with this crate's codec and posted on the stream's job queue. No socket is used per job.
//!
//! Completion is a frame on the same stream. Its buffer is the job's output, its `identity` is the
//! job's, and its length is the bytes written to the output. A length of 0 means the job failed,
//! and is counted in [`Counters::errors`]. Every job the block reads gets exactly one completion.
//! A job that cannot be decoded is a broken consumer: the block faults the stream, as it does for
//! any malformed ring, and completes nothing more on it.
//!
//! The three buffers of a job must be distinct. The block reads the foreground and background and
//! writes the output; the consumer may reuse an input once that job's completion is popped. The
//! output buffer is released with the usual frame return.
//!
//! Units: `width` and `height` in pixels, `stride` and bytes in bytes, `fourcc` the little-endian
//! four-character code of the pixel format. The three images of a job have the same size and
//! format.

#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

/// Longest stream name [`ImageBlend::job_stream`] answers, in bytes.
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

/// How the foreground is combined with the background (Porter-Duff, premultiplied).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum BlendOp {
    /// The foreground replaces the background.
    #[default]
    Src = 0,
    /// The foreground over the background.
    SrcOver = 1,
    /// The background over the foreground.
    DstOver = 2,
}

wire_enum!(BlendOp { Src = 0, SrcOver = 1, DstOver = 2 });

/// One composite, posted on the job stream.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlendJob {
    /// The caller's name for the job. Carried to the completion, never read.
    pub identity: u64,
    pub op: BlendOp,
    pub foreground: ImageRef,
    pub background: ImageRef,
    pub output: ImageRef,
}

/// Largest encoded [`BlendJob`], in bytes. The job queue must hold jobs this large.
pub const JOB_BYTES: usize = 8 + 1 + 3 * IMAGE_BYTES;

/// What the block takes.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlendInfo {
    /// The largest width and height of the images in a job.
    pub max_width: u32,
    pub max_height: u32,
}

/// Counted since the job stream was opened.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// Jobs completed with an output.
    pub jobs: u64,
    /// Jobs completed as failed: a size, format, operation or buffer the block refused.
    pub errors: u64,
}

#[contract("image.blend", version = 1)]
pub trait ImageBlend {
    #[method(1)]
    fn info(&mut self) -> Result<BlendInfo>;
    /// How many formats [`format`](Self::format) can return.
    #[method(2)]
    fn format_count(&mut self) -> Result<u16>;
    /// A format all three images of a job may use. `NOT_FOUND` past the last.
    #[method(3)]
    fn format(&mut self, index: u16) -> Result<u32>;
    /// The name to open the job stream by, as UTF-8 into `into`, returning its length. `into`
    /// holds at least [`MAX_STREAM_NAME`] bytes.
    #[method(4)]
    fn job_stream(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    #[method(5)]
    fn counters(&mut self) -> Result<Counters>;
}
