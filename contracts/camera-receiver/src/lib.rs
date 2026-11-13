//! The `camera.receiver/1` contract: the block that takes a sensor's frames off a camera link and
//! writes them to memory.
//!
//! The order of calls is `configure`, `start`, frames, `stop`. A consumer gets the receiver's frame
//! stream name from `frame_stream` and opens that stream through the client. Frames never cross
//! this contract: a method carries settings, and a stream carries frames.
//!
//! Where the frames go after the receiver. Which device of the board takes them next is the
//! board file's `link` on the receiver (a sensor's `link` names its receiver, the receiver's its
//! ISP), which a client reads from the device list and follows. What the hardware is doing is
//! [`CameraReceiver::feeds`]: on a chip where the receiver hands frames to an ISP inside the
//! hardware, it names that ISP; on one where the receiver only writes to memory, it names none.
//!
//! A receiver whose sensor carries embedded data (`camera.sensor` `SensorInfo::embedded_data`) may
//! publish it on a second stream named by `embedded_stream`, one buffer per frame with that frame's
//! sequence number.
//!
//! Each frame a stream publishes carries a sequence number, counted from the receiver's `start`, a
//! timestamp and an identity. A receiver takes no jobs, so nothing names its frames: it publishes
//! identity 0 on every one, and a consumer tells frames apart by sequence. Sequence is the only
//! per-frame count a consumer can match against the sensor's embedded data.
//!
//! Units: sizes in pixels and lines, `pixel_rate_hz` in hertz, `link_rate_mbps` in megabits per
//! second on each lane, `stride` and `size` in bytes. `data_type` is the CSI-2 data type code
//! (`0x2b` for RAW10).

#![no_std]

use lemnos_core::{Decode, Encode, Reader, Result, Shape, Status, Writer, contract};

/// Longest stream name a method answers, in bytes. A name is UTF-8 and at most this long.
pub const MAX_STREAM_NAME: usize = 32;

/// Longest name [`CameraReceiver::embedded_stream`] answers, in bytes.
pub const MAX_EMBEDDED_STREAM_NAME: usize = 32;

/// Longest name [`CameraReceiver::feeds`] answers, in bytes: a media entity's name is at most
/// this long.
pub const MAX_FEED_NAME: usize = 64;

macro_rules! wire_enum {
    ($name:ident { $($variant:ident = $value:literal),* $(,)? }) => {
        impl Encode for $name {
            const SHAPE: Shape = Shape::U8;
            fn encode(&self, out: &mut Writer<'_>) -> Result<()> {
                (*self as u8).encode(out)
            }
        }

        impl Decode<'_> for $name {
            fn decode(from: &mut Reader<'_>) -> Result<Self> {
                match u8::decode(from)? {
                    $($value => Ok(Self::$variant),)*
                    _ => Err(Status::INVALID_ARGUMENT),
                }
            }
        }
    };
}

/// The colour order of a raw frame, read from the top-left pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum BayerOrder {
    /// No colour filter.
    #[default]
    Mono = 0,
    Rggb = 1,
    Grbg = 2,
    Gbrg = 3,
    Bggr = 4,
}

wire_enum!(BayerOrder { Mono = 0, Rggb = 1, Grbg = 2, Gbrg = 3, Bggr = 4 });

/// How the receiver lays raw pixels out in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Packing {
    /// As the link carries them: packed at the frame's bit depth.
    #[default]
    Packed = 0,
    /// One little-endian 16-bit word per pixel, the value in the most significant bits.
    Unpacked16 = 1,
}

wire_enum!(Packing { Packed = 0, Unpacked16 = 1 });

/// What a sensor needs a receiver to take. A sensor's mode carries one of these for the consumer
/// to pass to `configure`.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinkConfig {
    /// Data lanes in use.
    pub lanes: u8,
    pub virtual_channel: u8,
    /// CSI-2 data type code.
    pub data_type: u8,
    /// Bits per pixel on the link.
    pub bit_depth: u8,
    /// The clock lane runs between lines as well.
    pub continuous_clock: bool,
    pub width: u32,
    pub height: u32,
    pub pixel_rate_hz: u64,
    /// Per-lane rate.
    pub link_rate_mbps: u32,
}

/// How a configured receiver writes each frame. It is single-plane: a raw frame is one run of lines.
///
/// `stride` is the bytes per line the block writes, which can be more than the line's pixels take
/// (padding). It is what the kernel reports for the frame, or what the block is programmed to write,
/// never a figure the driver assumes. Where the kernel reports another stride, the receiver refuses to start.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameLayout {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub packing: Packing,
    /// Bytes per line in a buffer, as the block writes them: padding included.
    pub stride: u32,
    /// Bytes per frame at most: `stride` times `height`.
    pub size: u32,
}

/// What the receiver can take.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Limits {
    pub max_lanes: u8,
    pub max_link_rate_mbps: u32,
    pub max_width: u32,
    pub max_height: u32,
}

/// Counted since the last `start`.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// Frames written.
    pub frames: u64,
    /// Frame starts with no buffer to write into.
    pub dropped: u64,
    /// Frames the block flagged as bad.
    pub errors: u64,
}

#[contract("camera.receiver", version = 1)]
pub trait CameraReceiver {
    #[method(1)]
    fn limits(&mut self) -> Result<Limits>;
    /// Sets the link and the memory layout, stopped. `BUSY` while streaming; `INVALID_ARGUMENT` for
    /// a link past [`Limits`].
    #[method(2)]
    fn configure(&mut self, link: LinkConfig, packing: Packing) -> Result<FrameLayout>;
    /// The link `configure` set, or `None` before one is set.
    #[method(3)]
    fn link(&mut self) -> Result<Option<LinkConfig>>;
    /// The name to open the frame stream by, as UTF-8 into `into`, returning its length.
    /// `into` holds at least [`MAX_STREAM_NAME`] bytes.
    #[method(4)]
    fn frame_stream(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// Starts taking frames. The sensor's own start follows this.
    #[method(5)]
    fn start(&mut self) -> Result<()>;
    /// Stops taking frames and returns every buffer.
    #[method(6)]
    fn stop(&mut self) -> Result<()>;
    #[method(7)]
    fn counters(&mut self) -> Result<Counters>;
    /// The ISP that takes this receiver's frames inside the hardware now, with no memory in
    /// between: its name as the hardware's own driver has it, which is the name an ISP driver's
    /// `device` setting takes, as UTF-8 into `into`, returning its length. `into` holds at least
    /// [`MAX_FEED_NAME`] bytes. A receiver wired to more than one ISP answers the one whose link
    /// is on.
    ///
    /// Length 0 when no ISP does: the frames reach only the frame stream, and whoever reads it
    /// passes them on. A receiver that can't read its wiring at the moment answers the error
    /// that stopped it.
    #[method(8)]
    fn feeds(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// The name of the stream the receiver publishes each frame's embedded data on, as UTF-8 into
    /// `into`, returning its length. `into` holds at least [`MAX_EMBEDDED_STREAM_NAME`] bytes.
    ///
    /// The embedded stream has one buffer per frame, published with that frame's sequence number,
    /// so a consumer pairs the two by sequence. Its layout is the sensor's (`SensorInfo::embedded_data`).
    /// Length 0 when the receiver captures none: a receiver that cannot take its sensor's embedded
    /// lines answers 0 and publishes nothing.
    #[method(9)]
    fn embedded_stream(&mut self, #[out] into: &mut [u8]) -> Result<usize>;

    /// The sensor whose frames arrive on this receiver's input, as the kernel names it: the name
    /// the sensor's own `identity` answers, as UTF-8 into `into`, returning its length. `into`
    /// holds at least [`MAX_FEED_NAME`] bytes.
    ///
    /// Three answers, which a consumer must keep apart. A name is the sensor on the input.
    /// Length 0 is nothing wired to the input. `NOT_SUPPORTED` is unknown: the receiver's kernel
    /// does not describe its input, so it cannot say what is wired to it, and a consumer must not
    /// take that for a match. Any other error is a read that failed now.
    #[method(10)]
    fn source(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// The layout `configure` set, or `None` before one is set: the same `FrameLayout`. Its `size`
    /// is the most bytes one frame takes in a buffer of the `frames` stream, so a consumer checks
    /// it against the stream's buffer length (`FrameStream::buffer_len` in the client) before or
    /// after opening the stream.
    #[method(11)]
    fn layout(&mut self) -> Result<Option<FrameLayout>>;
}
