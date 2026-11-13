//! The `camera.sensor/1` contract: an image sensor's modes and controls, in the sensor's own units.
//! Calibration, 3A and tuning belong to the caller. Lemnos carries the codes without reading them.
//!
//! Units:
//! - Sizes and crops in pixels and lines. A [`Rect`] is in pixel-array coordinates.
//! - Exposure in units of `1 / 2^exposure_fraction_bits` lines, so a sensor without fractional
//!   exposure has 0 fraction bits and counts whole lines.
//! - Frame length (VTS) in lines, line length (HTS) in pixel clocks.
//! - Gains are the sensor's register codes. [`GainRange::law`] turns a code into a gain, in both
//!   directions; see [`GainLaw`].
//! - `line_time_ps` in picoseconds, `max_fps_milli` in thousandths of a frame per second,
//!   `black_level` in codes at the mode's bit depth.
//!
//! Timing. `post_values` makes one group of the controls it names. The group is launched at the
//! next vertical blanking after its last write, so a post whose launch completes during frame `F`
//! is latched at the start of frame `F + 1`. Each control then shows on frame `F + d`, where `d` is
//! that control's delay in the selected [`Mode`]: `exposure_delay`, `gain_delay` or
//! `frame_length_delay`. With the three delays equal (every sensor in the tree), a post's controls
//! show together. Lemnos never staggers: controls with unequal delays in one post show on
//! different frames, so a caller with unequal delays posts each control on its own, at the time
//! that lands it on the frame it wants. [`CameraSensor::post_values`] says where a post lands, and
//! `set_controls` writes at once, with no group and no delay.
//!
//! Frame identity. `FrameValues::identity` is opaque: Lemnos carries it and never reads it. It is
//! not put on any frame. The receivers in the tree publish identity 0 on every frame they make
//! (`rp1-csi2`, `sunxi-vin`, `k230-capture`), so a frame is named by its sequence number, counted
//! by its receiver from the receiver's start. Lemnos does not tell a caller which sequence a post
//! lands on: a sensor driver sees no frame boundary, only its own registers. A caller that needs
//! that mapping counts frames on the receiver's stream and checks it with the embedded data below.
//!
//! What `applied` answers. The last `FrameValues` a driver accepted and wrote, from `post_values`
//! (or a driver's own values stream). It is set when the group's write has completed, which is
//! before the frame the values land on, and it reports what was written, not what a frame shows.
//! `set_controls` does not change it. `None` before the first accepted post.
//!
//! Read-back. Where [`SensorInfo::embedded_data`] is [`EmbeddedData::Registers`], each frame
//! carries the register values in force for it. The receiver publishes them on its embedded stream
//! (`camera.receiver` `embedded_stream`), one buffer per frame with that frame's sequence, decoded
//! by the sensor's own library crate. A sensor with [`EmbeddedData::None`] carries nothing, and a
//! caller can only verify its posts from the frames' pixels, which Lemnos does not look at.
//!
//! Landing frame. A caller that needs the frame a post shows on asks for it with `post_values_at`,
//! and reads it back with `applied_landing`. The frame is named by the sequence the receiver's
//! frame stream gives it, which is the receiver's frame-start count from zero (see
//! `camera.receiver` `frame_starts`). The rule, with `F` the frame in which the group launch
//! completes:
//!
//! 1. Read the receiver's frame-start count before the group's writes (`before`).
//! 2. Write the group, its launch last.
//! 3. Read the count again (`after`).
//! 4. `F = after.count - 1`. If the two counts are equal, no frame started during the writes, so
//!    the launch is in `before.count - 1`, which is the same frame. If they differ, a frame started
//!    while the group was in flight, so the launch straddled a frame boundary and `F` is the later
//!    frame. Lemnos never answers the earlier one for a straddle.
//!
//! Each control posted in the group shows on `F + d`, where `d` is its delay in the mode. The
//! answer names every posted control's frame ([`Landing`]) and the latest of them. A caller
//! with no frames counted yet is refused before anything is written (`BUSY`). The answer is exact
//! unless a frame starts between the launch write returning and the second read; then the answer is
//! at most one frame late, never early. Lemnos answers `NOT_SUPPORTED` when there is no link to a
//! receiver or the receiver cannot count frame starts, and writes nothing in that case.
//!
//! The order of calls is `select_mode`, `configure` the receiver with that mode's `link`, the
//! receiver's `start`, then `start` here. The first `drop_frames` frames after `start` are not
//! usable.

#![no_std]

use lemnos_core::{Decode, Encode, Field, Reader, Result, Shape, Status, Writer, contract};

pub use lemnos_contract_camera_receiver::{BayerOrder, FrameStarts, LinkConfig};

/// Longest name [`CameraSensor::identity`] answers, in bytes. The same bound a receiver's `source`
/// answer has.
pub const MAX_IDENTITY_NAME: usize = lemnos_contract_camera_receiver::MAX_FEED_NAME;

/// A rectangle of the pixel array.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The law that turns a gain code into a gain. `u` is the code for a gain of one
/// ([`GainRange::unity`]), `c` a code, and the gain is the ratio the formula gives.
///
/// - [`GainLaw::Linear`]: gain = c / u. Codes are steps of a linear gain, such as `0x10` for one
///   and `0x20` for two.
/// - [`GainLaw::Reciprocal`] with `full` = N: gain = (N - u) / (N - c), which is 1 / (1 - c / N)
///   when `u` is 0. Codes approach N as the gain grows, so N must exceed every code.
/// - [`GainLaw::Decibel`] with `milli_db_per_code` = s: gain = 10^((c - u) × s / 20000). Each code
///   is s thousandths of a decibel.
///
/// Going the other way, a caller solves the formula for c. The laws are all monotonic in c, so a
/// gain between two codes is not a code: a caller picks the nearer one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GainLaw {
    #[default]
    Linear,
    Reciprocal {
        full: u32,
    },
    Decibel {
        milli_db_per_code: u32,
    },
}

impl GainLaw {
    /// The law as its wire tag and parameter. A linear law's parameter is 0.
    const fn parts(self) -> (u8, u32) {
        match self {
            Self::Linear => (0, 0),
            Self::Reciprocal { full } => (1, full),
            Self::Decibel { milli_db_per_code } => (2, milli_db_per_code),
        }
    }
}

const GAIN_LAW_FIELDS: &[Field] = &[
    Field {
        name: "law",
        shape: Shape::U8,
    },
    Field {
        name: "param",
        shape: Shape::U32,
    },
];

impl Encode for GainLaw {
    const SHAPE: Shape = Shape::Record(GAIN_LAW_FIELDS);
    fn encode(&self, out: &mut Writer<'_>) -> Result<()> {
        let (tag, param) = self.parts();
        tag.encode(out)?;
        param.encode(out)
    }
}

impl Decode<'_> for GainLaw {
    fn decode(from: &mut Reader<'_>) -> Result<Self> {
        let tag = u8::decode(from)?;
        let param = u32::decode(from)?;
        match (tag, param) {
            (0, 0) => Ok(Self::Linear),
            (1, full) if full > 0 => Ok(Self::Reciprocal { full }),
            (2, milli_db_per_code) if milli_db_per_code > 0 => {
                Ok(Self::Decibel { milli_db_per_code })
            }
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }
}

/// The codes a gain takes. All zero: the sensor has no such gain.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GainRange {
    pub min: u32,
    pub max: u32,
    /// The code for a gain of one.
    pub unity: u32,
    /// The smallest step between two codes.
    pub step: u32,
    /// How a code becomes a gain.
    pub law: GainLaw,
}

/// Whether a sensor carries its own register values in each frame, for a caller to check what a
/// frame shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum EmbeddedData {
    /// No embedded data: the receiver's embedded stream, if it has one, carries nothing.
    #[default]
    None = 0,
    /// The register values in force for each frame, in the sensor's own layout, which its library
    /// crate decodes. Carried on the receiver's embedded stream, one buffer per frame.
    Registers = 1,
}

impl Encode for EmbeddedData {
    const SHAPE: Shape = Shape::U8;
    fn encode(&self, out: &mut Writer<'_>) -> Result<()> {
        (*self as u8).encode(out)
    }
}

impl Decode<'_> for EmbeddedData {
    fn decode(from: &mut Reader<'_>) -> Result<Self> {
        match u8::decode(from)? {
            0 => Ok(Self::None),
            1 => Ok(Self::Registers),
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }
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
    /// Whether each frame carries the register values in force for it.
    pub embedded_data: EmbeddedData,
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
    /// Frames from a post's group latching to the first frame each control shows on, as in the
    /// module's timing rule: a control posted in frame `F` shows on frame `F + delay`.
    pub exposure_delay: u8,
    pub gain_delay: u8,
    pub frame_length_delay: u8,
}

/// The frames a post's controls first show on, as [`landing_of`] gives them. A control the post
/// does not name is `None`. `digital_gain` is `None` on every sensor in the tree, which has no
/// digital gain the contract can reach.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Landing {
    pub exposure: Option<u64>,
    pub analogue_gain: Option<u64>,
    pub digital_gain: Option<u64>,
    pub frame_length: Option<u64>,
    /// The latest of the frames above: the frame by which every value the post names is in force.
    /// When the post names no control, the frame its group launched in.
    pub latest: u64,
}

/// The last post accepted with a landing, and where it landed.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AppliedLanding {
    pub values: FrameValues,
    pub landing: Landing,
}

/// The frame a group launched in, from the receiver's frame-start counts read before and after
/// the group's writes. See the module's landing rule: the later frame when the counts differ.
///
/// `before` and `after` never decrease, so the later count is `after`. Both are taken so the rule
/// reads as written and a caller cannot skip the second read.
pub fn launch_frame(before: FrameStarts, after: FrameStarts) -> u64 {
    let counted = if before.count == after.count {
        before
    } else {
        after
    };
    counted.count.saturating_sub(1)
}

/// Where a post's controls land, for a launch in frame `frame` and the mode's delays
/// (`exposure_delay`, `gain_delay`, `frame_length_delay`). A control the post does not name has no
/// frame.
pub fn landing_of(frame: u64, delays: [u8; 3], posted: &Controls) -> Landing {
    let at = |value: Option<u32>, delay: u8| value.map(|_| frame + u64::from(delay));
    let exposure = at(posted.exposure, delays[0]);
    let analogue_gain = at(posted.analogue_gain, delays[1]);
    let frame_length = at(posted.frame_length, delays[2]);
    let latest = [exposure, analogue_gain, frame_length]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(frame);
    Landing {
        exposure,
        analogue_gain,
        digital_gain: None,
        frame_length,
        latest,
    }
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
    /// Posts values as one group, with group hold where the sensor has it. Each control shows on
    /// the frame its delay in [`Mode`] gives (see the module's timing rule); with equal delays they
    /// show together. `BUSY` while earlier values wait to be written. The post is recorded for
    /// `applied_landing`, with the landing read for it when the receiver can count frame starts.
    #[method(8)]
    fn post_values(&mut self, values: FrameValues) -> Result<()>;
    /// The values last accepted and written by `post_values`, or `None` before any. Not what a
    /// frame shows: see the module's read-back rule.
    #[method(9)]
    fn applied(&mut self) -> Result<Option<FrameValues>>;
    /// Flips the readout and answers the colour order that results.
    #[method(10)]
    fn set_flips(&mut self, horizontal: bool, vertical: bool) -> Result<BayerOrder>;
    /// 0 turns the pattern off. `INVALID_ARGUMENT` past [`SensorInfo::test_patterns`].
    #[method(11)]
    fn set_test_pattern(&mut self, code: u16) -> Result<()>;
    /// The sensor's name as the kernel knows it, where the kernel knows it at all: the name a
    /// receiver's `source` answers for this sensor. Written as UTF-8 into `into`, returning its
    /// length; `into` holds at least [`MAX_IDENTITY_NAME`] bytes. `NOT_SUPPORTED` when no kernel
    /// names this sensor, which is unknown and never matches.
    #[method(12)]
    fn identity(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// Posts values as [`CameraSensor::post_values`] does, and answers where they land: the frame
    /// each posted control first shows on, by the landing rule in the module docs. Refused before
    /// anything is written: `NOT_SUPPORTED` with no link to a receiver that counts frame starts,
    /// and `BUSY` while no frame start has been counted since the receiver's `start`. Other
    /// refusals are those of `post_values`.
    #[method(13)]
    fn post_values_at(&mut self, values: FrameValues) -> Result<Landing>;
    /// The last post accepted by either `post_values` or `post_values_at`, with its values as
    /// posted and where it landed, or `None` before any post. `BUSY` when that post was made before
    /// any frame start had been counted, so its landing is not known. `NOT_SUPPORTED` when there
    /// was no link to a receiver that counts frame starts, so no landing was ever read for it.
    #[method(14)]
    fn applied_landing(&mut self) -> Result<Option<AppliedLanding>>;
}

/// What a sensor's bind found about the board's `link` to its receiver. Three answers, and only
/// `Confirmed` is a pass: a consumer must not read `Unknown` as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkCheck {
    /// The receiver's input is the one this sensor feeds.
    Confirmed,
    /// Not confirmed and not refuted; the reason says why.
    Unknown(&'static str),
    /// The linked receiver is not the one wired to this sensor; the reason says how.
    Mismatch(&'static str),
}

/// Checks a sensor's board link against what its linked receiver answers to `source`.
///
/// `answer` is that reply: the name it gave (empty for nothing wired), or the status it refused
/// with. `identity` is this sensor's own name as the kernel knows it, `None` when no kernel names
/// it. A refusal other than `NOT_SUPPORTED` is returned as is: the receiver could not be asked,
/// so the bind is retried and nothing is concluded.
pub fn check_link(answer: Result<&[u8]>, identity: Option<&[u8]>) -> Result<LinkCheck> {
    let name = match answer {
        Ok(name) => name,
        Err(Status::NOT_SUPPORTED) => {
            return Ok(LinkCheck::Unknown(
                "the linked receiver cannot say what is wired to its input",
            ));
        }
        Err(status) => return Err(status),
    };
    if name.is_empty() {
        return Ok(LinkCheck::Mismatch(
            "the linked receiver has nothing wired to its input",
        ));
    }
    Ok(match identity {
        None => LinkCheck::Unknown("the kernel does not name this sensor"),
        Some(own) if own == name => LinkCheck::Confirmed,
        Some(_) => LinkCheck::Mismatch("the linked receiver is fed by another sensor"),
    })
}
