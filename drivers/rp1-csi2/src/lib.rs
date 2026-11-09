//! The rp1-csi2 camera receiver as `camera.receiver/1`: the block's D-PHY, its CSI-2 host and one
//! DMA channel, delivering frames through a frame stream the module produces. It only receives:
//! the sensor is set up by its own driver, and the consumer asks this one for the link.
//!
//! Everything that touches the block happens in the kernel (`kernel/lemnos_rp1_csi2.c`); this crate
//! configures it and watches it, through the node `/dev/lemnos-rp1-csi2-<device>`. Frames never
//! cross this crate, and it never names a memory address: the module writes only to the pool of the
//! stream it was started on.
//!
//! Provenance: ported from the dev branch (`crates/lemnos-drivers-pisp/src/device/csi2_module.rs`
//! and the receiver module); the kernel part keeps the stock driver's copyright and is GPL-2.0, as
//! is this driver's module.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `device`: the platform device name, whose `/dev/lemnos-rp1-csi2-<device>` node is opened. With
//!   only one receiver bound, this may be left out.
//! - `heap`: the DMA heap for the frame pool, under `/dev/dma_heap`. Default `linux,cma`.
//! - `buffers`: frame buffers in the pool, 2 to 64, default 8.
//! - `stream`: the name the frame stream is opened by, default `frames`, at most
//!   [`MAX_STREAM_NAME`] bytes.
//!
//! The stream is created by `start` and ends at `stop`. Nothing is kept across a restart: `start`
//! opens the node, makes the pool and the stream, and starts the block from nothing.
//!
//! Failures. While streaming, every second the driver reads the module's state. A stream that has
//! ended or faulted, or a receiver that has stopped, starts recovery: the next ticks stop what's
//! left, make a new pool and stream and start again, with the delay between attempts doubling from
//! 250 ms and capped at 8 s, and clearing when one succeeds. The driver never gives up and never
//! turns itself off; `stop` is what ends it. While recovering, health is degraded and `start` is
//! refused with `BUSY`. A failed `start` or `stop` is reported to its caller, who retries it.

mod hw;

#[cfg(test)]
mod tests;

use lemnos_contract_camera_receiver::{
    CameraReceiver, Counters, FrameLayout, Limits, LinkConfig, MAX_STREAM_NAME, Packing,
};
use lemnos_core::{Config, Description, Health, Item, Lifecycle, MethodId, Result, Status, driver};

pub use hw::{Hardware, LinuxHardware, Settings};
pub use lemnos_driver_rp1_csi2_sys as sys;

const START: MethodId = MethodId {
    contract: 0,
    method: 5,
};
const STOP: MethodId = MethodId {
    contract: 0,
    method: 6,
};

/// Lowest per-lane link rate, Mbit/s.
pub const MIN_LINK_RATE_MBPS: u32 = 80;
/// Highest per-lane link rate, Mbit/s.
pub const MAX_LINK_RATE_MBPS: u32 = 1500;
/// The block's frame size limit in each direction, from the width and height it takes.
pub const MAX_DIMENSION: u32 = 0xffff;
/// A buffer's largest size (`LEMNOS_STREAM_MAX_BUFFER_BYTES`).
const MAX_FRAME_BYTES: u32 = 1 << 28;

/// First delay between recovery attempts.
const BACKOFF_FIRST_NS: u64 = 250_000_000;
/// How often the module's state is read while streaming.
const CHECK_NS: u64 = 1_000_000_000;

/// The delay before recovery attempt `attempt` (from 0), doubling to the cap.
const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// Where the driver is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Stopped,
    Streaming {
        /// When the next look at the module is due.
        next_check: Option<u64>,
    },
    Recovering {
        attempt: u32,
        /// When the next attempt is due; `None` until a tick has had the host's clock.
        due: Option<u64>,
    },
}

/// The rp1-csi2 camera receiver.
#[driver("rp1-csi2", provides = [CameraReceiver])]
pub struct Rp1Csi2 {
    hw: Box<dyn Hardware>,
    buffers: u32,
    stream: StreamName,
    link: Option<LinkConfig>,
    packing: Packing,
    layout: Option<FrameLayout>,
    state: State,
}

/// The stream's name, held by value so reading it allocates nothing.
#[derive(Clone, Copy)]
pub struct StreamName {
    bytes: [u8; MAX_STREAM_NAME],
    len: usize,
}

impl StreamName {
    /// `name`, when it's 1 to [`MAX_STREAM_NAME`] bytes.
    pub fn new(name: &str) -> Option<Self> {
        let bytes = name.as_bytes();
        if bytes.is_empty() || bytes.len() > MAX_STREAM_NAME {
            return None;
        }
        let mut out = [0; MAX_STREAM_NAME];
        out.get_mut(..bytes.len())?.copy_from_slice(bytes);
        Some(Self {
            bytes: out,
            len: bytes.len(),
        })
    }

    fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or(&[])
    }
}

/// The frame buffers a `buffers` setting allows.
const fn check_buffers(buffers: i64) -> Option<u32> {
    if buffers >= 2 && buffers <= 64 {
        Some(buffers as u32)
    } else {
        None
    }
}

/// The smallest ring holding `buffers` frames, a power of two.
const fn frame_slots_for(buffers: u32) -> u32 {
    buffers.next_power_of_two()
}

/// How a frame is laid out, from the link and the packing asked for. Sizes are checked here and,
/// again and independently, in the kernel against the buffer.
fn layout(link: &LinkConfig, packing: Packing) -> Result<FrameLayout> {
    let depth = u32::from(link.bit_depth);
    if !matches!(link.bit_depth, 8 | 10 | 12 | 14 | 16) {
        return Err(Status::INVALID_ARGUMENT);
    }
    if packing == Packing::Unpacked16 && !matches!(link.bit_depth, 10 | 12 | 14) {
        return Err(Status::INVALID_ARGUMENT);
    }
    let width = u64::from(link.width);
    let line_bytes = match packing {
        Packing::Packed => width
            .checked_mul(u64::from(depth))
            .ok_or(Status::INVALID_ARGUMENT)?
            .div_ceil(8),
        Packing::Unpacked16 => width.checked_mul(2).ok_or(Status::INVALID_ARGUMENT)?,
    };
    let stride = line_bytes.checked_add(15).ok_or(Status::INVALID_ARGUMENT)? & !15;
    let size = stride
        .checked_mul(u64::from(link.height))
        .ok_or(Status::INVALID_ARGUMENT)?;
    if size > u64::from(MAX_FRAME_BYTES) {
        return Err(Status::INVALID_ARGUMENT);
    }
    Ok(FrameLayout {
        width: link.width,
        height: link.height,
        bit_depth: link.bit_depth,
        packing,
        stride: u32::try_from(stride).map_err(|_| Status::INVALID_ARGUMENT)?,
        size: u32::try_from(size).map_err(|_| Status::INVALID_ARGUMENT)?,
    })
}

fn settings(cfg: &Config<'_>) -> Result<(hw::Settings, u32, StreamName)> {
    let device = cfg.str("device");
    let heap = cfg.str("heap").unwrap_or("linux,cma");
    let buffers = match cfg.int("buffers") {
        Some(buffers) => check_buffers(buffers).ok_or(Status::INVALID_ARGUMENT)?,
        None => 8,
    };
    let stream =
        StreamName::new(cfg.str("stream").unwrap_or("frames")).ok_or(Status::INVALID_ARGUMENT)?;
    let settings = hw::Settings::new(device, heap)?;
    Ok((settings, buffers, stream))
}

impl Lifecycle for Rp1Csi2 {
    type Needs = Rp1Csi2Needs;
    const TICKS: bool = true;

    fn start(_needs: Rp1Csi2Needs, cfg: &Config<'_>) -> Result<Self> {
        let (settings, buffers, stream) = settings(cfg)?;
        let hw: Box<dyn Hardware> = Box::new(LinuxHardware::new(settings));
        Ok(Self::with_hardware(hw, buffers, stream))
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::action("start", START));
        out.add(Item::action("stop", STOP));
    }

    fn health(&self) -> Health {
        match self.state {
            State::Recovering { .. } => {
                Health::Degraded("the receiver stopped while streaming: restarting it")
            }
            _ => Health::Ok,
        }
    }

    /// Looks at the module when streaming, and runs a due recovery attempt. Returns when next.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        match self.state {
            State::Stopped => None,
            State::Streaming { next_check: None } => {
                let at = now_ns.saturating_add(CHECK_NS);
                self.state = State::Streaming {
                    next_check: Some(at),
                };
                Some(at)
            }
            State::Streaming {
                next_check: Some(at),
            } if now_ns < at => Some(at),
            State::Streaming { .. } => {
                if self.hw.check().is_err() {
                    self.state = State::Recovering {
                        attempt: 0,
                        due: None,
                    };
                    return self.tick(now_ns);
                }
                let at = now_ns.saturating_add(CHECK_NS);
                self.state = State::Streaming {
                    next_check: Some(at),
                };
                Some(at)
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
                    let at = now_ns.saturating_add(CHECK_NS);
                    self.state = State::Streaming {
                        next_check: Some(at),
                    };
                    Some(at)
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

impl Rp1Csi2 {
    /// A driver over `hw`, for a simulated board or a test.
    #[must_use]
    pub fn with_hardware(hw: Box<dyn Hardware>, buffers: u32, stream: StreamName) -> Self {
        Self {
            hw,
            buffers,
            stream,
            link: None,
            packing: Packing::Packed,
            layout: None,
            state: State::Stopped,
        }
    }

    /// Stops the block, then starts it again as configured. Takes the configured link and
    /// layout: called with both set.
    fn begin(&mut self) -> Result<()> {
        let (link, layout) = match (self.link, self.layout) {
            (Some(link), Some(layout)) => (link, layout),
            _ => return Err(Status::INVALID_ARGUMENT),
        };
        // Whatever was running ends first; a failure here is not a reason to not try the start.
        let _ = self.hw.stop();
        self.hw.start(&hw::StartParams {
            lanes: u32::from(link.lanes),
            mbps: link.link_rate_mbps,
            vc: u32::from(link.virtual_channel),
            data_type: u32::from(link.data_type),
            width: link.width,
            height: link.height,
            stride: layout.stride,
            size: layout.size,
            unpack16: self.packing == Packing::Unpacked16,
            buffers: self.buffers,
            frame_slots: frame_slots_for(self.buffers),
        })
    }
}

impl CameraReceiver for Rp1Csi2 {
    fn limits(&mut self) -> Result<Limits> {
        Ok(Limits {
            max_lanes: self.hw.max_lanes()?,
            max_link_rate_mbps: MAX_LINK_RATE_MBPS,
            max_width: MAX_DIMENSION,
            max_height: MAX_DIMENSION,
        })
    }

    fn configure(&mut self, link: LinkConfig, packing: Packing) -> Result<FrameLayout> {
        if self.state != State::Stopped {
            return Err(Status::BUSY);
        }
        let max_lanes = self.hw.max_lanes()?;
        if link.lanes == 0 || link.lanes > max_lanes || link.virtual_channel > 3 {
            return Err(Status::INVALID_ARGUMENT);
        }
        if !(1..=63).contains(&link.data_type)
            || !(MIN_LINK_RATE_MBPS..=MAX_LINK_RATE_MBPS).contains(&link.link_rate_mbps)
            || link.width == 0
            || link.width > MAX_DIMENSION
            || link.height == 0
            || link.height > MAX_DIMENSION
        {
            return Err(Status::INVALID_ARGUMENT);
        }
        let layout = layout(&link, packing)?;
        self.link = Some(link);
        self.packing = packing;
        self.layout = Some(layout);
        Ok(layout)
    }

    fn link(&mut self) -> Result<Option<LinkConfig>> {
        Ok(self.link)
    }

    fn frame_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        let name = self.stream.as_bytes();
        if into.len() < name.len() {
            return Err(Status::BUFFER_TOO_SMALL);
        }
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
        self.state = State::Streaming { next_check: None };
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Stop means stop, whatever the driver was doing. Recovery is over either way.
        self.state = State::Stopped;
        self.hw.stop()
    }

    fn counters(&mut self) -> Result<Counters> {
        self.hw.counters()
    }
}
