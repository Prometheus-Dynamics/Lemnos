//! The rp1-csi2 camera receiver as `camera.receiver/1`: the block's D-PHY, its CSI-2 host and one
//! DMA channel, delivering frames on the board's `frames` stream. It only receives: the sensor is
//! set up by its own driver, and the consumer asks this one for the link.
//!
//! Everything that touches the block happens in the kernel (`kernel/lemnos_rp1_csi2.c`); this crate
//! configures it and watches it, through the node `/dev/lemnos-rp1-csi2-<device>`. A frame's bytes
//! never cross this crate, and it never names a memory address: the block writes each frame
//! straight into a buffer of the `frames` stream, the memory its consumer reads.
//!
//! How a frame gets there. The board declares the stream on this device with
//! `memory = "dma-buf"`, so each of its buffers is a dma-buf of its own. The module's stream is
//! made with no pool, and the driver hands the module the dma-buf of every `frames` buffer it
//! holds (`StreamEndpoint::buffer_handle`); the module pins it, checks its size and makes it that
//! buffer's memory. When the block finishes buffer *n* the driver publishes buffer *n* on
//! `frames`, and when the consumer gives *n* back the driver gives it back to the module. Before a
//! frame is published, and before a buffer goes back to the block, the module has been shown the
//! handle behind the driver's hold on that buffer, so what the consumer reads is what the block
//! wrote ([`relay`]).
//!
//! A consumer that leaves, and the next one. The stream's memory is made anew for each consumer.
//! The block is not stopped: the module keeps the old memory pinned for as long as the block may
//! be writing it, so nothing is ever written to freed memory, and frames finished into it are
//! dropped. Each buffer is given the new consumer's memory as soon as the block is done with it
//! (at once for a buffer the block isn't using). With no consumer no buffer has memory the driver
//! may use, and the block drops frames until one comes.
//!
//! What is refused. A `frames` stream whose buffers have no handles (plain shared memory, not
//! `memory = "dma-buf"`) is refused when the driver binds, with `NOT_SUPPORTED`: the stream says
//! what its buffers are before any consumer attaches (`StreamEndpoint::buffer_kind`), so the
//! device shows as failing to bind until the board file is fixed, and no frame is ever published
//! over memory the block didn't write. `start` without a `frames` stream on the device is
//! `NOT_FOUND`. Buffers smaller than a frame, or more than 64 of them, are `INVALID_ARGUMENT` at
//! `start` when a consumer is already attached, and otherwise show as degraded health once one
//! is.
//!
//! Provenance: ported from the dev branch (`crates/lemnos-drivers-pisp/src/device/csi2_module.rs`
//! and the receiver module); the kernel part keeps the stock driver's copyright and is GPL-2.0, as
//! is this driver's module.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `device`: the platform device name, whose `/dev/lemnos-rp1-csi2-<device>` node is opened. With
//!   only one receiver bound, this may be left out.
//! - `embedded_bytes`: the length of each embedded data line, a multiple of 16 from 16 to 65520.
//!   Read only when the board declares an `embedded` stream on the device (see below).
//!
//! The frame memory is the stream's, so its buffer count and size are the board file's
//! (`[devices.streams.frames]`: `buffers`, at most 64, and `buffer_bytes`, at least a frame).
//! `buffers` is 1 to 64 (`MAX_BUFFERS` in `relay.rs`; 0 or more than 64 is refused at `start`).
//! The receiver needs one buffer free to receive into, and holds one more while a frame is being
//! written, so the usable minimum is the number of frames a consumer holds at once plus two. A
//! consumer that holds every buffer stops frames arriving; the receiver then answers
//! `frame_starts` with `None` ("cannot say now"), so a sensor watching the count is not restarted
//! for it.
//!
//! The embedded data is a second stream on the same device, named `embedded` in the board file:
//! one buffer per line, the frame's sequence on it, the line taken on the block's second DMA
//! channel (CSI-2 data type 0x12, the image's virtual channel). A frame with no line publishes
//! nothing, and the sequence skips it. Its buffers are the board's, dma-bufs like `frames`'s.
//! Asked for only where the sensor sends embedded lines. Untested on a board.
//!
//! The module's stream is created by `start` and ends at `stop`. Nothing of it is kept across a
//! restart: `start` opens the node, makes the module's stream, gives it the buffers the driver
//! holds, and starts the block from nothing.
//!
//! Frame starts. `frame_starts` reads the kernel's count of the block's frame starts since `start`,
//! with the time of the latest one (CLOCK_MONOTONIC). A start is counted when the block raises its
//! interrupt for it, whether or not a buffer was armed (then the frame is dropped), so a published
//! frame's sequence is the count at its start, less one, as the frame stream numbers it. A block
//! with no buffer address raises no interrupt, and such a start is in no count (measured on the
//! board: `testing/board-runs/D5.md`): the count is the sensor's starts only while every start
//! shows. The count is unknown (`None`) before `start` and after `stop`, and while the block has no
//! buffer armed (every buffer held by the consumer, or none given yet): then the block makes no frame
//! start, the count cannot move, and a sensor watching it must not read that as the sensor going
//! quiet. The kernel module answers `valid = 0` then, and the count is read again once a buffer is
//! armed (`kernel/lemnos_rp1_csi2.c`, `lrc_frame_starts`). A recovery restart is a new
//! start: the count begins again at zero, so a read lower than the one before means a restart.
//!
//! Failures. While streaming, every millisecond the driver moves finished frames and returned
//! buffers, and every second it reads the module's state. A stream that has ended or faulted, or
//! a receiver that has stopped, starts recovery: the next ticks stop what's left, make a new
//! module stream and start again, with the delay between attempts doubling from
//! 250 ms and capped at 8 s, and clearing when one succeeds. The driver never gives up and never
//! turns itself off; `stop` is what ends it. While recovering, health is degraded and `start` is
//! refused with `BUSY`. A failed `start` or `stop` is reported to its caller, who retries it.
//! A node that answers `ENODEV` (its block was unbound) is dropped, and the next attempt opens
//! the path again, so the driver comes back once the block is bound again.

mod hw;
pub mod relay;

#[cfg(test)]
mod tests;

use lemnos_contract_camera_isp::CameraIsp;
use lemnos_contract_camera_receiver::{
    CameraReceiver, Counters, FrameLayout, FrameStarts, Limits, LinkConfig, Packing,
};
use lemnos_core::{
    BufferHandle, CallerId, Config, ConfigError, Description, Dev, Health, Item, Key, Kind,
    Lifecycle, MethodId, Result, Status, StreamHandle, driver,
};

use lemnos_hal::blocks::BufferKind;

pub use hw::{Channel, Finished, Hardware, LinuxHardware, Settings, StartParams};
pub use lemnos_driver_rp1_csi2_sys as sys;
use relay::{MAX_BUFFERS, Relay};

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
/// How often finished frames and returned buffers are looked for.
const POLL_NS: u64 = 1_000_000;
/// How long the driver may go without a tick while streaming, when the block's doorbell is there:
/// one frame period at 30 fps. A frame the doorbell failed to announce is found by the tick after
/// this, and counted (see [`Rp1Csi2::lost_wakes`]).
const FALLBACK_NS: u64 = 33_000_000;
/// The stream the board declares on the device, and the name its consumer opens it by.
const STREAM: &str = "frames";
/// The stream of the embedded data, when the board declares one on the device.
const EMBEDDED: &str = "embedded";

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
    /// Where frames go: the board's `frames` stream on this device.
    #[needs(optional, free)]
    frames: Option<StreamHandle>,
    /// The ISP the board feeds this receiver to (`link` on the receiver): the front end on a
    /// PiSP block. Resolved at bind, where the host checks it serves `camera.isp/1`.
    #[link]
    _isp: Option<Dev<dyn CameraIsp>>,
    /// Where the embedded data goes, when the board declares an `embedded` stream on the device.
    #[needs(optional, free)]
    embedded: Option<StreamHandle>,
    /// The bytes of each embedded line (`config.embedded_bytes`), with `embedded`.
    embedded_bytes: u32,
    hw: Box<dyn Hardware>,
    relay: Relay,
    embedded_relay: Relay,
    link: Option<LinkConfig>,
    packing: Packing,
    layout: Option<FrameLayout>,
    state: State,
    /// The caller of the call in progress, told by the host (`Lifecycle::caller`).
    caller: Option<CallerId>,
    /// The caller whose START started the receiver: its going away stops it (`caller_gone`).
    owner: Option<CallerId>,
    /// When the last tick asked to be called again. A tick at or after it is a deadline tick, not a
    /// wake of the doorbell.
    wake_at: Option<u64>,
    /// Frames the block finished that the driver has moved, on the image stream.
    frames_moved: u64,
    /// Frames a deadline tick found: the doorbell should have woken the driver for them. Counted,
    /// so a lost bell is visible.
    lost_wakes: u64,
}

/// The smallest ring holding `buffers` frames, a power of two.
const fn frame_slots_for(buffers: u32) -> u32 {
    buffers.next_power_of_two()
}

/// The bytes one packed frame of `width` x `height` at `bit_depth` takes in the receiver's buffer:
/// the receiver's own layout, so a board's `buffer_bytes` for `frames` is checked by the same rule.
/// `None` where the receiver would refuse the frame.
#[must_use]
pub fn packed_frame_bytes(width: u32, height: u32, bit_depth: u8) -> Option<u32> {
    let link = LinkConfig {
        bit_depth,
        width,
        height,
        ..LinkConfig::default()
    };
    layout(&link, Packing::Packed).ok().map(|frame| frame.size)
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

/// `NOT_SUPPORTED` for a stream whose buffers have no handles: there is nothing to give the
/// block. The stream knows from the board file, with or without a consumer.
fn handles_only(frames: &relay::Frames) -> Result<()> {
    if frames.buffer_kind() == BufferKind::Handles {
        Ok(())
    } else {
        Err(Status::NOT_SUPPORTED)
    }
}

/// Every setting the driver reads. Both are optional: `embedded_bytes` is needed only when the
/// embedded-data stream is wired, which only the board's needs say, so that case stays at bind.
const KEYS: &[Key] = &[
    Key::optional("device", Kind::Str),
    Key::optional("embedded_bytes", Kind::Int),
];

impl Lifecycle for Rp1Csi2 {
    type Needs = Rp1Csi2Needs;
    const TICKS: bool = true;
    // The host says who is calling, so a caller that goes away can stop the receiver it started.
    const TRACKS_CALLERS: bool = true;

    fn caller(&mut self, caller: CallerId) {
        self.caller = Some(caller);
    }

    fn caller_gone(&mut self, caller: CallerId) {
        if self.owner == Some(caller) {
            self.owner = None;
            let _ = CameraReceiver::stop(self);
        }
    }

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        hw::Settings::new(cfg.str("device"))
            .map_err(|_| ConfigError::new("device", "must be a name with no '/' or NUL"))?;
        if let Some(bytes) = cfg.int("embedded_bytes") {
            u32::try_from(bytes)
                .map_err(|_| ConfigError::new("embedded_bytes", "must be 0 to 4294967295"))?;
        }
        Ok(())
    }

    fn start(needs: Rp1Csi2Needs, cfg: &Config<'_>) -> Result<Self> {
        let settings = hw::Settings::new(cfg.str("device"))?;
        // The block writes a frame only into a dma-buf: known now, before any consumer comes.
        if let Some(frames) = needs.frames.as_ref() {
            handles_only(&**frames)?;
        }
        let embedded_bytes = match needs.embedded.is_some() {
            true => {
                if let Some(frames) = needs.embedded.as_ref() {
                    handles_only(&**frames)?;
                }
                cfg.int("embedded_bytes")
                    .and_then(|bytes| u32::try_from(bytes).ok())
                    .ok_or(Status::INVALID_ARGUMENT)?
            }
            false => 0,
        };
        let hw: Box<dyn Hardware> = Box::new(LinuxHardware::new(settings));
        let mut driver = Self::with_hardware(hw, needs.frames);
        driver._isp = needs._isp;
        Ok(match needs.embedded {
            Some(embedded) => driver.with_embedded(embedded, embedded_bytes),
            None => driver,
        })
    }

    fn wait_fd(&self) -> Option<BufferHandle<'_>> {
        self.hw.doorbell().map(BufferHandle::from)
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::action("start", START));
        out.add(Item::action("stop", STOP));
    }

    fn health(&self) -> Health {
        match self.state {
            State::Recovering { .. } => {
                Health::Failed("the receiver stopped while streaming: restarting it")
            }
            State::Streaming { .. } if self.relay.refused() => Health::Degraded(
                "the frames stream's buffers are not memory the block can take \
                 (smaller than a frame?): no frame can reach its consumer",
            ),
            _ => Health::Ok,
        }
    }

    /// Moves frames and buffers and looks at the module when streaming, and runs a due
    /// recovery attempt. Returns when next.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        match self.state {
            State::Stopped => None,
            State::Streaming { next_check } => {
                let due = next_check.is_some_and(|at| now_ns >= at);
                // A tick at or after the time the last one asked for came from its deadline: no
                // wake of the doorbell brought it, so whatever it finds is a lost bell.
                let at_deadline = self.wake_at.is_some_and(|at| now_ns >= at);
                let moved = self.frames_moved;
                if self.pump().is_err() || (due && self.hw.check().is_err()) {
                    self.state = State::Recovering {
                        attempt: 0,
                        due: None,
                    };
                    return self.tick(now_ns);
                }
                let check_at = match next_check {
                    Some(at) if !due => at,
                    _ => now_ns.saturating_add(CHECK_NS),
                };
                self.state = State::Streaming {
                    next_check: Some(check_at),
                };
                if at_deadline {
                    self.lost_wakes = self
                        .lost_wakes
                        .saturating_add(self.frames_moved.saturating_sub(moved));
                }
                let period = if self.hw.doorbell().is_some() {
                    FALLBACK_NS
                } else {
                    POLL_NS
                };
                let next = now_ns.saturating_add(period).min(check_at);
                self.wake_at = Some(next);
                Some(next)
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
                    self.state = State::Streaming {
                        next_check: Some(now_ns.saturating_add(CHECK_NS)),
                    };
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

impl Rp1Csi2 {
    /// A driver over `hw`, for a simulated board or a test.
    #[must_use]
    pub fn with_hardware(hw: Box<dyn Hardware>, frames: Option<StreamHandle>) -> Self {
        Self {
            frames,
            _isp: None,
            embedded: None,
            embedded_bytes: 0,
            hw,
            relay: Relay::new(Channel::Image),
            embedded_relay: Relay::new(Channel::Embedded),
            link: None,
            packing: Packing::Packed,
            layout: None,
            state: State::Stopped,
            caller: None,
            owner: None,
            wake_at: None,
            frames_moved: 0,
            lost_wakes: 0,
        }
    }

    /// Frames a deadline tick found, since the driver was made: a wake the doorbell should have
    /// made. Zero while the doorbell works.
    #[must_use]
    pub fn lost_wakes(&self) -> u64 {
        self.lost_wakes
    }

    /// Gives the receiver its `embedded` stream and the bytes of each embedded line. Without
    /// one, the receiver captures no embedded data.
    #[must_use]
    pub fn with_embedded(mut self, embedded: StreamHandle, bytes: u32) -> Self {
        self.embedded = Some(embedded);
        self.embedded_bytes = bytes;
        self
    }

    /// Finished frames out to the stream, returned buffers back to the block, and each buffer
    /// the driver holds matched to the memory behind it. An error means the module's stream is
    /// over. The embedded data is moved the same way, on its own stream.
    fn pump(&mut self) -> Result<()> {
        if let Some(embedded) = self.embedded.as_mut() {
            while let Some(done) = self.hw.next_frame(Channel::Embedded)? {
                self.embedded_relay
                    .finished(&done, &mut **embedded, &mut *self.hw)?;
            }
            self.embedded_relay.sweep(&mut **embedded);
            self.embedded_relay.take(&mut **embedded, &mut *self.hw)?;
        }
        let Some(frames) = self.frames.as_mut() else {
            return Ok(());
        };
        while let Some(done) = self.hw.next_frame(Channel::Image)? {
            self.frames_moved += 1;
            self.relay.finished(&done, &mut **frames, &mut *self.hw)?;
        }
        self.relay.sweep(&mut **frames);
        self.relay.take(&mut **frames, &mut *self.hw)
    }

    /// Stops the block, then starts it again as configured: a new module stream, given the
    /// memory of every `frames` buffer the driver holds before the block is started. Takes the
    /// configured link and layout: called with both set.
    fn begin(&mut self) -> Result<()> {
        let (link, layout) = match (self.link, self.layout) {
            (Some(link), Some(layout)) => (link, layout),
            _ => return Err(Status::INVALID_ARGUMENT),
        };
        let Some(frames) = self.frames.as_mut() else {
            // Nowhere for a frame to go.
            return Err(Status::NOT_FOUND);
        };
        handles_only(&**frames)?;
        let buffers = frames.buffer_count();
        if buffers == 0 || buffers as usize > MAX_BUFFERS {
            return Err(Status::INVALID_ARGUMENT);
        }
        let (embedded_buffers, embedded_bytes) = match self.embedded.as_ref() {
            None => (0, 0),
            Some(embedded) => {
                handles_only(&**embedded)?;
                let count = embedded.buffer_count();
                let bytes = self.embedded_bytes;
                if count == 0
                    || count as usize > MAX_BUFFERS
                    || bytes == 0
                    || bytes > sys::EMBEDDED_MAX
                    || !bytes.is_multiple_of(16)
                {
                    return Err(Status::INVALID_ARGUMENT);
                }
                (count, bytes)
            }
        };
        // Whatever was running ends first; a failure here is not a reason to not try the start.
        let _ = self.hw.stop();
        self.hw.make(&hw::StartParams {
            embedded_buffers,
            embedded_frame_slots: if embedded_buffers == 0 {
                0
            } else {
                frame_slots_for(embedded_buffers)
            },
            embedded_bytes,
            lanes: u32::from(link.lanes),
            mbps: link.link_rate_mbps,
            vc: u32::from(link.virtual_channel),
            data_type: u32::from(link.data_type),
            width: link.width,
            height: link.height,
            stride: layout.stride,
            size: layout.size,
            unpack16: self.packing == Packing::Unpacked16,
            buffers,
            frame_slots: frame_slots_for(buffers),
        })?;
        self.relay.restart(buffers);
        self.embedded_relay.restart(embedded_buffers);
        let ready = match self.relay.settle_all(&mut **frames, &mut *self.hw) {
            Ok(()) => self.relay.take(&mut **frames, &mut *self.hw),
            Err(status) => Err(status),
        };
        let refusal = if self.relay.no_handles() {
            // A consumer is attached to plain shared memory: the block can't be given it.
            Err(Status::NOT_SUPPORTED)
        } else if self.relay.refused() {
            Err(Status::INVALID_ARGUMENT)
        } else {
            ready
        };
        let embedded_ready = match self.embedded.as_mut() {
            None => Ok(()),
            Some(embedded) => match self
                .embedded_relay
                .settle_all(&mut **embedded, &mut *self.hw)
            {
                Ok(()) => self.embedded_relay.take(&mut **embedded, &mut *self.hw),
                Err(status) => Err(status),
            },
        };
        let embedded_refusal = if self.embedded.is_none() {
            Ok(())
        } else if self.embedded_relay.no_handles() {
            Err(Status::NOT_SUPPORTED)
        } else if self.embedded_relay.refused() {
            Err(Status::INVALID_ARGUMENT)
        } else {
            embedded_ready
        };
        let refusal = refusal.and(embedded_refusal);
        if let Err(status) = refusal.and_then(|()| self.hw.go()) {
            let _ = self.hw.stop();
            return Err(status);
        }
        Ok(())
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
        if self.frames.is_none() {
            return Err(Status::NOT_FOUND);
        }
        let name = STREAM.as_bytes();
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
        // Whoever starts the receiver owns it until it stops, even if the start then fails.
        self.owner = self.caller;
        self.begin()?;
        self.state = State::Streaming { next_check: None };
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Stop means stop, whatever the driver was doing. Recovery is over either way.
        self.owner = None;
        self.state = State::Stopped;
        self.hw.stop()
    }

    fn counters(&mut self) -> Result<Counters> {
        let mut counters = self.hw.counters()?;
        // Finished by the block, but into memory nobody was reading by then.
        counters.dropped = counters.dropped.saturating_add(self.relay.lost());
        Ok(counters)
    }

    /// No ISP takes this driver's frames in hardware: the block's DMA channel writes them to
    /// memory, and the frame stream is where they go. (The front end, which is a receiver and
    /// an ISP in one, is `drivers/pisp`.) Which device takes them next is the board's `link`.
    fn feeds(&mut self, _into: &mut [u8]) -> Result<usize> {
        Ok(0)
    }

    /// The frame starts the block has counted since `start`, as the kernel's count: `None` before
    /// `start` and after `stop`, where nothing is counted and nothing is zero. Counted in the
    /// kernel (`LEMNOS_RP1_CSI2_IOC_FRAME_STARTS`), so a read is the block's own count. Also `None`
    /// while no buffer is armed (the kernel's `valid` is 0): the block makes no frame start then.
    fn frame_starts(&mut self) -> Result<Option<FrameStarts>> {
        if self.state == State::Stopped {
            return Ok(None);
        }
        self.hw.frame_starts()
    }

    /// The name of the stream the embedded data goes to: `embedded`, when the board declares
    /// one on the device. One buffer per frame, carrying that frame's sequence, on the image's
    /// virtual channel with data type 0x12 (`kernel/lemnos_rp1_csi2.c`). 0 when none.
    fn embedded_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        if self.embedded.is_none() {
            return Ok(0);
        }
        let name = EMBEDDED.as_bytes();
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }

    /// Unknown: the module knows the block, not which sensor the board wires to it. A link the
    /// board names is confirmed by the sensor's side only where the kernel names that sensor.
    fn source(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn layout(&mut self) -> Result<Option<FrameLayout>> {
        Ok(self.layout)
    }

    fn inputs(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn input(&mut self) -> Result<u32> {
        Err(Status::NOT_SUPPORTED)
    }
}
