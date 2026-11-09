//! The `camera.sensor/1` contract: an image sensor's modes and controls, in the sensor's own units.
//! Calibration, 3A and tuning belong to the caller. Lemnos carries the codes without reading them.
//!
//! Units:
//! - Sizes and crops in pixels and lines. A [`Rect`] is in pixel-array coordinates.
//! - Exposure in units of `1 / 2^exposure_fraction_bits` lines, so a sensor without fractional
//!   exposure has 0 fraction bits and counts whole lines.
//! - Frame length (VTS) in lines, line length (HTS) in pixel clocks.
//! - Gains are the sensor's register codes.
//! - `line_time_ps` in picoseconds, `max_fps_milli` in thousandths of a frame per second,
//!   `black_level` in codes at the mode's bit depth.
//! - `FrameValues::identity` is opaque: Lemnos carries it and never reads it.
//!
//! The order of calls is `select_mode`, `configure` the receiver with that mode's `link`, the
//! receiver's `start`, then `start` here. The first `drop_frames` frames after `start` are not
//! usable.

#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

pub use lemnos_contract_camera_receiver::{BayerOrder, LinkConfig};

/// A rectangle of the pixel array.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The codes a gain takes. All zero: the sensor has no such gain.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GainRange {
    pub min: u32,
    pub max: u32,
    /// The code for a gain of one.
    pub unity: u32,
    pub step: u32,
}

/// The sensor as a whole.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SensorInfo {
    /// Modes, numbered from 0.
    pub modes: u16,
    pub array_width: u32,
    pub array_height: u32,
    /// Test pattern codes, numbered from 1. 0 is always no pattern.
    pub test_patterns: u16,
}

/// One way the sensor can run.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mode {
    /// Size, bit depth and everything the receiver needs.
    pub link: LinkConfig,
    /// The colour order with no flips. `set_flips` answers the order for other flips.
    pub bayer: BayerOrder,
    pub max_fps_milli: u32,
    pub line_time_ps: u32,
    /// Line length (HTS).
    pub line_length: u32,
    /// Frame length (VTS) limits.
    pub frame_length_min: u32,
    pub frame_length_max: u32,
    /// Smallest exposure.
    pub exposure_min: u32,
    /// Exposure may reach the frame length less this many whole lines.
    pub exposure_margin: u32,
    pub exposure_fraction_bits: u8,
    pub analogue_gain: GainRange,
    pub digital_gain: GainRange,
    /// The part of the array this mode reads.
    pub crop: Rect,
    /// Array pixels combined into one output pixel across and down. 1 for none.
    pub binning_x: u8,
    pub binning_y: u8,
    pub black_level: u16,
    /// Frames to discard after `start`.
    pub drop_frames: u8,
    /// Frames from a value being written to the first frame it shows on.
    pub exposure_delay: u8,
    pub gain_delay: u8,
    pub frame_length_delay: u8,
}

/// Values to change. `None` leaves one as it is.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Controls {
    pub exposure: Option<u32>,
    pub analogue_gain: Option<u32>,
    pub digital_gain: Option<u32>,
    pub frame_length: Option<u32>,
}

/// Controls for one frame and the caller's name for that frame.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameValues {
    pub identity: u64,
    pub controls: Controls,
}

#[contract("camera.sensor", version = 1)]
pub trait CameraSensor {
    #[method(1)]
    fn info(&mut self) -> Result<SensorInfo>;
    /// `NOT_FOUND` past the last mode.
    #[method(2)]
    fn mode(&mut self, index: u16) -> Result<Mode>;
    /// `NOT_FOUND` past the last mode. `BUSY` while streaming.
    #[method(3)]
    fn select_mode(&mut self, index: u16) -> Result<()>;
    /// The selected mode, or `None` before one is selected.
    #[method(4)]
    fn selected(&mut self) -> Result<Option<u16>>;
    /// Starts the frames. `INVALID_ARGUMENT` before a mode is selected.
    #[method(5)]
    fn start(&mut self) -> Result<()>;
    #[method(6)]
    fn stop(&mut self) -> Result<()>;
    /// Writes `controls` at once. `INVALID_ARGUMENT` for a code outside the selected mode's limits.
    #[method(7)]
    fn set_controls(&mut self, controls: Controls) -> Result<()>;
    /// Posts values to take effect together on one frame, with group hold where the sensor has
    /// it, for cadence-rate exposure control. `BUSY` while earlier values wait to be written.
    #[method(8)]
    fn post_values(&mut self, values: FrameValues) -> Result<()>;
    /// The values last written by `post_values`, or `None` before any.
    #[method(9)]
    fn applied(&mut self) -> Result<Option<FrameValues>>;
    /// Flips the readout and answers the colour order that results.
    #[method(10)]
    fn set_flips(&mut self, horizontal: bool, vertical: bool) -> Result<BayerOrder>;
    /// 0 turns the pattern off. `INVALID_ARGUMENT` past [`SensorInfo::test_patterns`].
    #[method(11)]
    fn set_test_pattern(&mut self, code: u16) -> Result<()>;
}
