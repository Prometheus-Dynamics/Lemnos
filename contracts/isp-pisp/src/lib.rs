//! The `isp.pisp/1` contract: what only the PiSP ISP has, beside its `camera.isp/1`.
//!
//! The PiSP has a front end (statistics and raw output from the receiver) and a back end (image
//! outputs from memory). `camera.isp/1` numbers outputs [`OUTPUT_BACK_END_0`],
//! [`OUTPUT_BACK_END_1`], [`OUTPUT_FRONT_END_0`] and [`OUTPUT_FRONT_END_1`], and its
//! `camera.isp/1` config layouts include [`LAYOUT_BACK_END`] and [`LAYOUT_FRONT_END`]. Their bytes
//! are the block's own configuration structures. Tuning stays with the caller.
//!
//! `line` counts from the frame's first line. `hw_version` is the block's version register.

#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

/// `camera.isp/1` output number of the back end's first image output.
pub const OUTPUT_BACK_END_0: u8 = 0;
/// The back end's second image output.
pub const OUTPUT_BACK_END_1: u8 = 1;
/// The front end's first raw output.
pub const OUTPUT_FRONT_END_0: u8 = 2;
/// The front end's second raw output.
pub const OUTPUT_FRONT_END_1: u8 = 3;

/// `camera.isp/1` configuration layout: a back-end tiles configuration.
pub const LAYOUT_BACK_END: u32 = 1;
/// `camera.isp/1` configuration layout: a front-end configuration.
pub const LAYOUT_FRONT_END: u32 = 2;
/// `camera.isp/1` statistics layout: the front end's statistics.
pub const LAYOUT_STATISTICS: u32 = 3;

/// The block.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PispInfo {
    pub hw_version: u32,
    /// Back-end jobs that may be queued and not yet finished.
    pub queue_depth: u32,
    pub front_end_config_bytes: u32,
    pub back_end_config_bytes: u32,
    pub statistics_bytes: u32,
    /// Front end only, while streaming: frame starts at which the block had not yet taken the
    /// job queued for the frame before, since the last start. 0 for the back end and when
    /// stopped.
    pub stalled: u64,
}

#[contract("isp.pisp", version = 1, kind = "camera")]
pub trait IspPisp {
    #[method(1)]
    fn info(&mut self) -> Result<PispInfo>;
    /// Compresses a front-end raw output to the block's 8-bit raw format, or back to 16-bit.
    /// Stopped only: `BUSY` while streaming. `NOT_FOUND` for anything but a front-end output.
    #[method(2)]
    fn set_front_end_compression(&mut self, output: u8, compressed: bool) -> Result<()>;
    /// Sets up temporal denoise's two buffers, optionally for its 8-bit compressed average. Each
    /// job's configuration must match. Stopped only: `BUSY` while streaming.
    #[method(3)]
    fn enable_temporal_denoise(&mut self, compressed: bool) -> Result<()>;
    /// Starts the temporal denoise average again from the next frame.
    #[method(4)]
    fn reset_temporal_denoise(&mut self) -> Result<()>;
    /// Marks a frame's line `line` for an event as it lands, for callers that read a frame early.
    /// `NOT_SUPPORTED` without the event source.
    #[method(5)]
    fn set_line_event(&mut self, line: u32) -> Result<()>;
}
