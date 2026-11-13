//! The `camera.isp/1` contract: an image signal processor, as bytes and streams.
//!
//! Lemnos does no 3A and holds no tuning. Configuration and statistics buffers carry layouts only
//! the ISP and its caller understand: [`IspInfo`] names each with a number and a size, and a caller
//! that knows the layout fills it. The numbers mean something per driver.
//!
//! A job is one [`IspJob`], encoded with this crate's codec, posted on the config stream's ring.
//! Its buffers are indices into the streams named here; the config buffer holds a filled layout.
//! Frames published on the statistics and output streams carry the job's `identity`, so a caller
//! ties results to the frame it asked for. Lemnos carries the identity and never reads it.
//!
//! A job the ISP cannot run (a buffer it doesn't hold, a layout it doesn't take, no room) is
//! counted in [`Counters::dropped`] and nothing is published for it. Bytes that cannot be decoded
//! as an [`IspJob`] are a broken consumer: the ISP faults the config stream, as it does for any
//! malformed ring, and runs nothing more that was posted on it.
//!
//! Units: sizes in pixels and lines, `stride` and `size` and `bytes` in bytes, `fourcc` the
//! little-endian four-character code of the pixel format.
//!
//! # Layouts
//!
//! A consumer sizes and checks its buffers from the contract alone. [`CameraIsp::input_layout`]
//! is the layout the ISP reads a raw frame in, from `set_input`; [`CameraIsp::output_layout`] (or
//! the answer of `set_output`) is the layout it writes one output's image in. A frame in an input
//! buffer is `size` bytes at most, laid out at `stride` bytes a line; an output buffer is written
//! the same way. A layout is `None` until the input or output it describes is set, and after an
//! output is cleared. A driver's layouts are fixed while stopped: `set_input` and `set_output`
//! change them, and the job names the layout it was built for.
//!
//! # Configuration buffers
//!
//! The config stream's buffers hold configurations, and each job names one. The rules, which every
//! driver of this contract follows:
//!
//! 1. The consumer writes a configuration into config buffer `n` before it posts the job that names
//!    `n`, and does not write `n` again until that job has completed (a frame, a drop or an error
//!    is published for it). That is the stream rule for every buffer; nothing here relaxes it.
//! 2. The driver reads config buffer `n` once, when it takes the job, copies it into its own memory,
//!    checks the copy, and programs the block from the copy. It never reads the buffer again and
//!    never keeps a pointer into it. A write to `n` after the take changes nothing the job runs.
//! 3. A config buffer belongs to the driver from the moment a job naming it is accepted until that job
//!    completes (a frame, a drop or an error is published for it), or until `stop`, whichever is
//!    first. After that the consumer may rewrite it and post another job naming it. `stop` completes
//!    or drops every outstanding accepted job and returns every config buffer, so after `stop` returns
//!    the consumer may rewrite any of them. The completion (or `stop`) is the return: there is no
//!    message of the driver's own for it.
//! 4. A job is accepted when the driver takes it off the config stream's ring. When it is accepted,
//!    a job that names a config buffer an earlier accepted job still holds is dropped and counted in
//!    [`Counters::dropped`], as for every other buffer, and nothing is published for it. A driver
//!    that accepts one job at a time, waiting for each to complete before it takes the next, never
//!    drops a job this way: its jobs wait on the ring instead. Either way the earlier job is not
//!    disturbed.
//! 5. A write to a config buffer that a job holds cannot change that job. The driver takes its copy at
//!    accept, before it posts anything, and programs the block from the copy only. A write that
//!    overlaps the take (rule 1 broken) is not refused: the copy is checked like any other, so the job
//!    runs with what the driver copied or is dropped if the copy fails a check. It never runs unchecked,
//!    and the consumer learns what ran from its completion only.

#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

pub use lemnos_contract_camera_receiver::{BayerOrder, MAX_STREAM_NAME};

/// Most outputs a job can address.
pub const MAX_OUTPUTS: usize = 4;
/// In an [`IspJob`], no buffer in this slot.
pub const NO_BUFFER: u32 = u32::MAX;

/// What this ISP takes.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IspInfo {
    /// Outputs, numbered from 0. At most [`MAX_OUTPUTS`].
    pub max_outputs: u8,
    pub max_width: u32,
    pub max_height: u32,
    /// Configuration layouts, numbered from 0.
    pub config_layouts: u8,
    /// The layout of a statistics buffer, a number that means something per driver.
    pub statistics_layout: u32,
    pub statistics_bytes: u32,
}

/// One configuration layout.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigLayout {
    /// A number that means something per driver, put in [`IspJob::layout`].
    pub layout: u32,
    /// Size of one configuration in this layout.
    pub bytes: u32,
}

/// The raw frames the ISP takes.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IspInput {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub bayer: BayerOrder,
}

/// One output's image.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IspOutput {
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    /// Bytes to leave past the image in each buffer, for importers that need them.
    pub pad_bytes: u32,
}

/// The layout of one image in a buffer: an input frame ([`CameraIsp::input_layout`]) or an
/// output's image ([`CameraIsp::set_output`], [`CameraIsp::output_layout`]).
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutputLayout {
    /// Bytes per line in a buffer, as the block reads or writes them: padding included.
    pub stride: u32,
    /// Bytes per image at most: every plane of the image, and the `pad_bytes` an output asked for.
    pub size: u32,
}

/// One unit of work posted on the config stream's ring.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IspJob {
    /// The caller's name for the frame. Carried, never read.
    pub identity: u64,
    /// From [`ConfigLayout::layout`].
    pub layout: u32,
    /// A buffer on the config stream, filled with that layout.
    pub config_buffer: u32,
    /// A buffer on the input stream, or [`NO_BUFFER`] when the frame comes straight from the
    /// receiver.
    pub input_buffer: u32,
    /// For each output, a buffer on its stream, or [`NO_BUFFER`] to skip it this job.
    pub outputs: [u32; MAX_OUTPUTS],
}

/// Counted since the last `start`.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// Jobs finished.
    pub jobs: u64,
    /// Jobs refused or not run.
    pub dropped: u64,
    /// Jobs the block reported as failed.
    pub errors: u64,
}

#[contract("camera.isp", version = 1, kind = "camera")]
pub trait CameraIsp {
    #[method(1)]
    fn info(&mut self) -> Result<IspInfo>;
    /// `NOT_FOUND` past the last layout.
    #[method(2)]
    fn config_layout(&mut self, index: u8) -> Result<ConfigLayout>;
    /// Sets the raw input, stopped. `BUSY` while streaming.
    #[method(3)]
    fn set_input(&mut self, input: IspInput) -> Result<()>;
    /// Sets one output, stopped. `NOT_FOUND` past the last output. `INVALID_ARGUMENT` for a format
    /// or size the output can't make. `BUSY` while streaming.
    #[method(4)]
    fn set_output(&mut self, output: u8, image: IspOutput) -> Result<OutputLayout>;
    /// Turns one output off. `BUSY` while streaming.
    #[method(5)]
    fn clear_output(&mut self, output: u8) -> Result<()>;
    /// The name of the stream jobs are posted on, whose buffers hold configuration. This and the
    /// three stream methods below write UTF-8 into `into` and return its length; `into` holds at
    /// least [`MAX_STREAM_NAME`] bytes.
    #[method(6)]
    fn config_stream(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// The stream of statistics buffers.
    #[method(7)]
    fn statistics_stream(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// The stream of raw frames the caller gives this ISP. `NOT_SUPPORTED` when frames come
    /// straight from the receiver.
    #[method(8)]
    fn input_stream(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// The stream of one output's images. `NOT_FOUND` past the last output.
    #[method(9)]
    fn output_stream(&mut self, output: u8, #[out] into: &mut [u8]) -> Result<usize>;
    #[method(10)]
    fn start(&mut self) -> Result<()>;
    #[method(11)]
    fn stop(&mut self) -> Result<()>;
    #[method(12)]
    fn counters(&mut self) -> Result<Counters>;
    /// The layout a raw frame takes in an input buffer, from `set_input`: `stride` and `size`. `None`
    /// before `set_input`. `NOT_SUPPORTED` when the frame comes straight from the receiver, which
    /// owns its layout (see `input_stream`).
    #[added]
    #[method(13)]
    fn input_layout(&mut self) -> Result<Option<OutputLayout>>;
    /// The layout of one output's image, from `set_output`: `stride` and `size`. `None` before the
    /// output is set and after `clear_output`. `NOT_FOUND` past the last output.
    #[added]
    #[method(14)]
    fn output_layout(&mut self, output: u8) -> Result<Option<OutputLayout>>;
}
