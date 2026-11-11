//! The sunxi-vin camera receiver as `camera.receiver/1`: raw frames from the block's stock V4L2
//! capture node, delivered through a frame stream this driver produces. It only receives: the
//! sensor is set up by its own driver, and the consumer asks this one for the link.
//!
//! There is no Lemnos kernel code for this block yet, so the vendor kernel driver stays bound and
//! this driver runs its capture node through `lemnos-v4l2`: it selects the node's input, sets the
//! format, keeps a ring of MMAP buffers queued and streams. For each frame the node fills, it takes
//! a buffer of the `frames` stream, copies the node's bytes into it and publishes it, then gives
//! the node its buffer back. That is one copy per frame, and nothing is allocated on the way. When
//! no consumer is attached, or every stream buffer is out, the frame is dropped and the node's
//! buffer goes straight back, so capture never waits on a consumer. The bytes are the node's as
//! they are: the driver never reads them.
//!
//! Provenance: interface facts only, from the vendor kernel's `drivers/vin` (GPL-2.0, no code
//! copied): the capture node is multi-planar (`vin-video/vin_video.c`, `vin_init_video`), selecting
//! input 0 takes the first sensor and sets up the pipeline, and must come before the format
//! (`__vin_s_input`), raw frames are 8 bits per pixel at depth 8 and 16 bits per pixel at depths 10
//! to 16 with lines of `width * bits / 8` bytes (`vin-video/vin_core.c`, the format table, and
//! `vin_video.c`), frames are 32 to 4800 pixels in each direction (`vin-video/vin_core.h`), and a
//! CSI-2 port takes up to 4 lanes (`vin-mipi/sunxi_mipi.c`). The source is named in
//! `testing/board-runs/E12.md`. The kernel driver programs the link itself, from its own sensor
//! subdevice: a [`LinkConfig`] is checked here, not applied.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `capture`: the capture node's media entity name. Required. The node is found by it in the
//!   media graphs, so its `/dev/video` number doesn't matter.
//! - `input`: the input index to select, default 0.
//! - `order`: the colour order, `mono`, `rggb`, `grbg`, `gbrg` or `bggr`, default `mono`. The pixel
//!   format asked of the node carries it. The node has no monochrome format past 8 bits, so a
//!   monochrome sensor deeper than that names an order.
//! - `lanes`: data lanes wired, 1 to 4, default 4.
//! - `buffers`: buffers the node fills, 2 to 32, default 4.
//!
//! Frames go out on the stream the board gives as `[devices.streams.frames]`, whose buffers must
//! hold a frame (`buffer_bytes` at least the configured layout's `size`). Each published frame
//! carries the node's sequence number, widened to 64 bits across wraps, its timestamp, and identity
//! 0: this receiver takes no jobs, so it has no identity to hand back.
//!
//! Which receiver and ISP feed the node is read from the media graph by [`SunxiVin::feeds`]; see
//! [`route`].
//!
//! Failures. While streaming, each tick takes every buffer the node has filled. A node that
//! errors starts recovery: the next ticks stop what's left and open, set up and start the node
//! again, with the delay between attempts doubling from 250 ms and capped at 8 s, and clearing
//! when one succeeds. The driver never gives up and never turns itself off; `stop` is what ends
//! it. While recovering, health is degraded and `start` is refused with `BUSY`. A failed `start`
//! or `stop` is reported to its caller, who retries it. A frame the node flags as bad, or one that
//! doesn't fit a stream buffer, is counted in `errors` and dropped; it is not a reason to restart.

mod hw;
pub mod route;

#[cfg(test)]
mod tests;

use lemnos_contract_camera_receiver::{
    BayerOrder, CameraReceiver, Counters, FrameLayout, Limits, LinkConfig, MAX_STREAM_NAME, Packing,
};
use lemnos_core::{
    Config, Description, Health, Item, Lifecycle, MethodId, Result, Status, StreamHandle, driver,
};
use lemnos_hal::blocks::StreamFrame;
use lemnos_v4l2::FourCc;

pub use hw::{Frame, Granted, Graph, Hardware, LinuxHardware, Settings, Wanted};
pub use route::Feeds;

const START: MethodId = MethodId {
    contract: 0,
    method: 5,
};
const STOP: MethodId = MethodId {
    contract: 0,
    method: 6,
};

/// The stream frames go out on: the need's name, which is the name a consumer opens it by.
pub const STREAM: &str = "frames";

/// Most data lanes a port takes.
pub const MAX_LANES: u8 = 4;
/// Lowest per-lane link rate, Mbit/s: the D-PHY floor.
pub const MIN_LINK_RATE_MBPS: u32 = 80;
/// Highest per-lane link rate, Mbit/s: the D-PHY 1.2 ceiling. It bounds what's accepted; the
/// kernel driver refuses a rate the block can't take when the link comes up.
pub const MAX_LINK_RATE_MBPS: u32 = 2500;
/// Smallest frame in each direction.
pub const MIN_DIMENSION: u32 = 32;
/// Largest frame in each direction.
pub const MAX_DIMENSION: u32 = 4800;

/// First delay between recovery attempts.
const BACKOFF_FIRST_NS: u64 = 250_000_000;
/// How often the node is read for filled buffers while streaming.
const POLL_NS: u64 = 1_000_000;

/// The delay before recovery attempt `attempt` (from 0), doubling to the cap.
const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// Where the driver is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Stopped,
    Streaming,
    Recovering {
        attempt: u32,
        /// When the next attempt is due; `None` until a tick has had the host's clock.
        due: Option<u64>,
    },
}

/// The node's pixel format for raw frames of `depth` bits in `order` (`videodev2.h`).
#[must_use]
pub const fn fourcc(order: BayerOrder, depth: u8) -> Option<FourCc> {
    let code: &[u8; 4] = match (depth, order) {
        (8, BayerOrder::Mono) => b"GREY",
        (8, BayerOrder::Bggr) => b"BA81",
        (8, BayerOrder::Gbrg) => b"GBRG",
        (8, BayerOrder::Grbg) => b"GRBG",
        (8, BayerOrder::Rggb) => b"RGGB",
        (10, BayerOrder::Bggr) => b"BG10",
        (10, BayerOrder::Gbrg) => b"GB10",
        (10, BayerOrder::Grbg) => b"BA10",
        (10, BayerOrder::Rggb) => b"RG10",
        (12, BayerOrder::Bggr) => b"BG12",
        (12, BayerOrder::Gbrg) => b"GB12",
        (12, BayerOrder::Grbg) => b"BA12",
        (12, BayerOrder::Rggb) => b"RG12",
        (14, BayerOrder::Bggr) => b"BG14",
        (14, BayerOrder::Gbrg) => b"GB14",
        (14, BayerOrder::Grbg) => b"GR14",
        (14, BayerOrder::Rggb) => b"RG14",
        (16, BayerOrder::Bggr) => b"BYR2",
        (16, BayerOrder::Gbrg) => b"GB16",
        (16, BayerOrder::Grbg) => b"GR16",
        (16, BayerOrder::Rggb) => b"RG16",
        _ => return None,
    };
    Some(FourCc::new(code))
}

/// `order` from its board file spelling.
#[must_use]
pub fn parse_order(order: &str) -> Option<BayerOrder> {
    Some(match order {
        "mono" => BayerOrder::Mono,
        "rggb" => BayerOrder::Rggb,
        "grbg" => BayerOrder::Grbg,
        "gbrg" => BayerOrder::Gbrg,
        "bggr" => BayerOrder::Bggr,
        _ => return None,
    })
}

/// How the node lays a frame out. Depth 8 is packed; depths 10 to 14 come one 16-bit word per
/// pixel, and depth 16 is packed at 16 bits. Lines have no padding.
fn layout(link: &LinkConfig, packing: Packing) -> Result<FrameLayout> {
    let bytes_per_pixel = match (link.bit_depth, packing) {
        (8, Packing::Packed) => 1,
        (10 | 12 | 14, Packing::Unpacked16) | (16, Packing::Packed) => 2,
        _ => return Err(Status::INVALID_ARGUMENT),
    };
    let stride = link
        .width
        .checked_mul(bytes_per_pixel)
        .ok_or(Status::INVALID_ARGUMENT)?;
    let size = stride
        .checked_mul(link.height)
        .ok_or(Status::INVALID_ARGUMENT)?;
    Ok(FrameLayout {
        width: link.width,
        height: link.height,
        bit_depth: link.bit_depth,
        packing,
        stride,
        size,
    })
}

/// The driver's own settings, beside where the node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub input: u32,
    pub order: BayerOrder,
    pub lanes: u8,
    pub buffers: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            input: 0,
            order: BayerOrder::Mono,
            lanes: MAX_LANES,
            buffers: 4,
        }
    }
}

fn options(cfg: &Config<'_>) -> Result<Options> {
    let defaults = Options::default();
    let int = |key, min: i64, max: i64, default: u32| match cfg.int(key) {
        Some(v) if (min..=max).contains(&v) => {
            u32::try_from(v).map_err(|_| Status::INVALID_ARGUMENT)
        }
        Some(_) => Err(Status::INVALID_ARGUMENT),
        None => Ok(default),
    };
    let order = match cfg.str("order") {
        Some(order) => parse_order(order).ok_or(Status::INVALID_ARGUMENT)?,
        None => defaults.order,
    };
    let lanes = int("lanes", 1, i64::from(MAX_LANES), u32::from(defaults.lanes))?;
    Ok(Options {
        input: int("input", 0, i64::from(i32::MAX), defaults.input)?,
        order,
        lanes: u8::try_from(lanes).map_err(|_| Status::INVALID_ARGUMENT)?,
        buffers: int("buffers", 2, 32, defaults.buffers)?,
    })
}

/// The node's sequence number, widened to 64 bits across its wraps.
#[derive(Debug, Clone, Copy, Default)]
struct Sequence {
    last: Option<u32>,
    wraps: u64,
}

impl Sequence {
    fn widen(&mut self, sequence: u32) -> u64 {
        if self.last.is_some_and(|last| sequence < last) {
            self.wraps = self.wraps.wrapping_add(1);
        }
        self.last = Some(sequence);
        (self.wraps << 32) | u64::from(sequence)
    }
}

/// What became of one frame.
enum Delivery {
    Published,
    /// No consumer, or no stream buffer free.
    Dropped,
    /// Flagged bad by the node, or didn't fit.
    Bad,
}

/// The sunxi-vin camera receiver.
#[driver("sunxi-vin", provides = [CameraReceiver])]
pub struct SunxiVin {
    #[needs]
    frames: StreamHandle,
    hw: Box<dyn Hardware>,
    options: Options,
    link: Option<LinkConfig>,
    layout: Option<FrameLayout>,
    state: State,
    counters: Counters,
    sequence: Sequence,
    /// A stream buffer kept from a frame that didn't fit, for the next one.
    held: Option<u32>,
}

impl Lifecycle for SunxiVin {
    type Needs = SunxiVinNeeds;
    const TICKS: bool = true;

    fn start(needs: SunxiVinNeeds, cfg: &Config<'_>) -> Result<Self> {
        let settings = Settings::new(cfg.str("capture").ok_or(Status::INVALID_ARGUMENT)?)?;
        let options = options(cfg)?;
        let hw: Box<dyn Hardware> = Box::new(LinuxHardware::new(settings));
        Ok(Self::with_hardware(hw, needs.frames, options))
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::action("start", START));
        out.add(Item::action("stop", STOP));
    }

    fn health(&self) -> Health {
        match self.state {
            State::Recovering { .. } => {
                Health::Degraded("the capture node failed while streaming: restarting it")
            }
            _ => Health::Ok,
        }
    }

    /// Takes what the node has filled when streaming, and runs a due recovery attempt. Returns
    /// when next.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        match self.state {
            State::Stopped => None,
            State::Streaming => {
                if self.pump().is_err() {
                    self.state = State::Recovering {
                        attempt: 0,
                        due: None,
                    };
                    return self.tick(now_ns);
                }
                Some(now_ns.saturating_add(POLL_NS))
            }
            State::Recovering { attempt, due: None } => {
                let at = now_ns.saturating_add(backoff_ns(attempt));
                self.state = State::Recovering {
                    attempt,
                    due: Some(at),
                };
                Some(at)
            }
            State::Recovering { due: Some(at), .. } if now_ns < at => Some(at),
            State::Recovering { attempt, .. } => match self.begin() {
                Ok(()) => {
                    self.state = State::Streaming;
                    Some(now_ns.saturating_add(POLL_NS))
                }
                Err(_) => {
                    let attempt = attempt.saturating_add(1);
                    let at = now_ns.saturating_add(backoff_ns(attempt));
                    self.state = State::Recovering {
                        attempt,
                        due: Some(at),
                    };
                    Some(at)
                }
            },
        }
    }
}

impl SunxiVin {
    /// A driver over `hw` producing into `frames`, for a simulated board or a test.
    #[must_use]
    pub fn with_hardware(hw: Box<dyn Hardware>, frames: StreamHandle, options: Options) -> Self {
        Self {
            frames,
            hw,
            options,
            link: None,
            layout: None,
            state: State::Stopped,
            counters: Counters::default(),
            sequence: Sequence::default(),
            held: None,
        }
    }

    /// Which sensor, receiver and ISP feed the capture node, from the media graph as it stands.
    /// Complete while the node is open, which is from `start` to `stop`: the kernel driver links
    /// the sensor and the receiver's ISP in when the input is selected, and out when the node
    /// closes.
    pub fn feeds(&mut self) -> Result<Feeds> {
        let graph = self.hw.graph()?;
        Ok(route::feeds(&graph.topology, graph.capture))
    }

    /// Stops the node, then opens and starts it again as configured. Called with a layout set.
    fn begin(&mut self) -> Result<()> {
        let (Some(link), Some(layout)) = (self.link, self.layout) else {
            return Err(Status::INVALID_ARGUMENT);
        };
        let fourcc = fourcc(self.options.order, link.bit_depth).ok_or(Status::INVALID_ARGUMENT)?;
        // Whatever was running ends first; a failure here is not a reason to not try the start.
        let _ = self.hw.stop();
        let granted = self.hw.open(&Wanted {
            input: self.options.input,
            fourcc,
            width: layout.width,
            height: layout.height,
            stride: layout.stride,
            size: layout.size,
        })?;
        let as_asked = granted.fourcc == fourcc
            && granted.width == layout.width
            && granted.height == layout.height
            && granted.stride == layout.stride
            && granted.size >= layout.size;
        if !as_asked {
            let _ = self.hw.stop();
            return Err(Status::NOT_SUPPORTED);
        }
        self.hw.stream_on(self.options.buffers)?;
        self.counters = Counters::default();
        self.sequence = Sequence::default();
        Ok(())
    }

    /// Takes every buffer the node has filled, at most one round of the ring, and gives each
    /// back. An error means the node failed.
    fn pump(&mut self) -> Result<()> {
        for _ in 0..self.options.buffers {
            let Some(frame) = self.hw.dequeue()? else {
                return Ok(());
            };
            let counter = match self.deliver(&frame) {
                Delivery::Published => &mut self.counters.frames,
                Delivery::Dropped => &mut self.counters.dropped,
                Delivery::Bad => &mut self.counters.errors,
            };
            *counter = counter.saturating_add(1);
            self.hw.requeue(frame.index)?;
        }
        Ok(())
    }

    /// Copies one frame into a stream buffer and publishes it.
    fn deliver(&mut self, frame: &Frame) -> Delivery {
        let sequence = self.sequence.widen(frame.sequence);
        if frame.bad {
            return Delivery::Bad;
        }
        let buffer = match self.held.take() {
            Some(buffer) => buffer,
            None => match self.frames.next_buffer() {
                Ok(Some(buffer)) => buffer,
                Ok(None) | Err(_) => return Delivery::Dropped,
            },
        };
        let mut offset = 0usize;
        for plane in 0..frame.planes {
            let Ok(bytes) = self.hw.payload(frame, plane) else {
                self.held = Some(buffer);
                return Delivery::Bad;
            };
            match self.frames.write(buffer, offset, bytes) {
                Ok(true) => offset = offset.saturating_add(bytes.len()),
                // Nobody to land on: hand the buffer back empty, which the stream drops.
                Ok(false) => {
                    let _ = self.frames.publish(StreamFrame {
                        buffer,
                        bytes: 0,
                        sequence,
                        identity: 0,
                        timestamp_ns: frame.timestamp_ns,
                    });
                    return Delivery::Dropped;
                }
                Err(_) => {
                    self.held = Some(buffer);
                    return Delivery::Bad;
                }
            }
        }
        let Ok(bytes) = u32::try_from(offset) else {
            self.held = Some(buffer);
            return Delivery::Bad;
        };
        let published = self.frames.publish(StreamFrame {
            buffer,
            bytes,
            sequence,
            identity: 0,
            timestamp_ns: frame.timestamp_ns,
        });
        match published {
            Ok(()) => Delivery::Published,
            Err(_) => {
                self.held = Some(buffer);
                Delivery::Dropped
            }
        }
    }
}

impl CameraReceiver for SunxiVin {
    fn limits(&mut self) -> Result<Limits> {
        Ok(Limits {
            max_lanes: self.options.lanes,
            max_link_rate_mbps: MAX_LINK_RATE_MBPS,
            max_width: MAX_DIMENSION,
            max_height: MAX_DIMENSION,
        })
    }

    fn configure(&mut self, link: LinkConfig, packing: Packing) -> Result<FrameLayout> {
        if self.state != State::Stopped {
            return Err(Status::BUSY);
        }
        let dimension = MIN_DIMENSION..=MAX_DIMENSION;
        if link.lanes == 0
            || link.lanes > self.options.lanes
            || link.virtual_channel > 3
            || !(1..=63).contains(&link.data_type)
            || !(MIN_LINK_RATE_MBPS..=MAX_LINK_RATE_MBPS).contains(&link.link_rate_mbps)
            || !dimension.contains(&link.width)
            || !dimension.contains(&link.height)
            || fourcc(self.options.order, link.bit_depth).is_none()
        {
            return Err(Status::INVALID_ARGUMENT);
        }
        let layout = layout(&link, packing)?;
        self.link = Some(link);
        self.layout = Some(layout);
        Ok(layout)
    }

    fn link(&mut self) -> Result<Option<LinkConfig>> {
        Ok(self.link)
    }

    fn frame_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        const _: () = assert!(STREAM.len() <= MAX_STREAM_NAME);
        let name = STREAM.as_bytes();
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }

    fn start(&mut self) -> Result<()> {
        if self.link.is_none() {
            return Err(Status::INVALID_ARGUMENT);
        }
        if self.state != State::Stopped {
            return Err(Status::BUSY);
        }
        self.begin()?;
        self.state = State::Streaming;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Stop means stop, whatever the driver was doing. Recovery is over either way.
        self.state = State::Stopped;
        self.hw.stop()
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.counters)
    }

    /// The ISP the frames pass through on the way to the capture node, by its media entity
    /// name ([`route::feeds`]), read from the graph at each call. The kernel driver links the
    /// path in when the node opens, so this is complete from `start` to `stop`; before, it
    /// answers what the graph has then, which may be none.
    fn feeds(&mut self, into: &mut [u8]) -> Result<usize> {
        let feeds = SunxiVin::feeds(self)?;
        let name = feeds.isp.as_deref().map_or(&[][..], str::as_bytes);
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }
}
