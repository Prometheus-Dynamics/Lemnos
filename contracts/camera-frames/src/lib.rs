//! The `camera.frames/1` contract: a camera that delivers finished frames itself, such as a USB
//! camera. It has no sensor, receiver or ISP of its own to speak to.
//!
//! The order of calls is `select_format`, `start`, frames, `stop`. Frames are read from the
//! stream named by `frame_stream`, opened through the client. Frames never cross this contract. A
//! frame's opaque `u64` identity is for the producer and consumer; Lemnos never reads it.
//!
//! Units: `width` and `height` in pixels, `fps_milli` in thousandths of a frame per second,
//! `max_frame_bytes` in bytes, and `fourcc` the little-endian four-character code of the format.

#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

/// Longest stream name `frame_stream` answers, in bytes.
pub const MAX_STREAM_NAME: usize = 32;

/// One format, size and rate the camera offers.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameFormat {
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub fps_milli: u32,
    /// Largest a frame can be, the size of a buffer for it.
    pub max_frame_bytes: u32,
}

/// Counted since the last `start`.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// Frames delivered.
    pub frames: u64,
    /// Frames lost for want of a buffer or to a stall.
    pub dropped: u64,
    /// Frames that arrived damaged.
    pub errors: u64,
}

#[contract("camera.frames", version = 1, kind = "camera")]
pub trait CameraFrames {
    /// How many formats [`format`](Self::format) can return.
    #[method(1)]
    fn format_count(&mut self) -> Result<u16>;
    /// `NOT_FOUND` past the last format.
    #[method(2)]
    fn format(&mut self, index: u16) -> Result<FrameFormat>;
    /// `NOT_FOUND` past the last format. `BUSY` while streaming.
    #[method(3)]
    fn select_format(&mut self, index: u16) -> Result<()>;
    /// The selected format, or `None` before one is selected.
    #[method(4)]
    fn selected(&mut self) -> Result<Option<u16>>;
    /// The name to open the frame stream by, as UTF-8 into `into`, returning its length. `into`
    /// holds at least [`MAX_STREAM_NAME`] bytes.
    #[method(5)]
    fn frame_stream(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// `INVALID_ARGUMENT` before a format is selected.
    #[method(6)]
    fn start(&mut self) -> Result<()>;
    #[method(7)]
    fn stop(&mut self) -> Result<()>;
    #[method(8)]
    fn counters(&mut self) -> Result<Counters>;
}
