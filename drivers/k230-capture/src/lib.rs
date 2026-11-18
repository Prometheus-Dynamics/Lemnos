//! The ISP8000 frame path, with the CSI receiver behind the vendor kernel's `vvcam-mipi` device,
//! as `camera.receiver/1`: [`K230Capture`] sets up the link, the ISP's input and output and its
//! memory interface, and delivers each frame the block writes through a frame stream. It only
//! receives: the sensor is set up by its own driver.
//!
//! The vendor's capture daemon is never used. The kernel takes the link as plain numbers and sets
//! up the CSI receiver and video input itself. The ISP is reached one of two ways, as the board
//! file chooses (see Settings):
//!
//! - **The vendor's `vvcam-isp`** ([`LinuxHardware`]): the ISP is programmed directly through its
//!   register requests, every one through the sys crate's table, and the memory interface writes
//!   only into DMA memory the sys crate holds (see its documentation for every path by which a
//!   caller's number reaches the block, and the check on each).
//! - **`lemnos.ko`** ([`ModuleHardware`]): the ISP bound to the module, which drives its frames
//!   role: it writes every address register itself, from memory it allocated for the ISP and a
//!   range it checked, and switches the path between memories only at a frame's end. The
//!   module's configuration role runs beside it on the same block, so `k230isp` gives tuning and
//!   statistics for the frames this driver delivers. See [`module`].
//!
//! # Frames
//!
//! The block writes a ring of three memories, one being written, one armed for the next frame,
//! one being read. On the vendor's way the memories come from the host's DMA memory (the `dma`
//! need); on the module's way the module allocates them for the ISP. At each of the main path's frame
//! ends the driver reads which memory the block has moved to, arms the free one for the frame
//! after, and copies the finished frame into a buffer of the `frames` stream with
//! `StreamEndpoint::write`, then publishes it. That is one copy per frame; nothing is allocated
//! on the way. When no consumer is attached or every stream buffer is out, the frame is dropped.
//!
//! The block cannot fill the stream's own buffers, so a stream with dma-buf handles is filled the
//! same way: nothing on this kernel gives user space the bus address of the stream's dma-buf
//! (see the sys crate's documentation), and the memory interface takes nothing else.
//!
//! The vendor driver never sets a frame's sequence or time. Each published frame carries the
//! driver's own count of frame ends since `start` (frames dropped included, so a gap shows a
//! loss). A recovery restart goes on counting from where it was, so the sequence only goes up: a
//! consumer sees a gap across a stall, never a reset to zero. Only a fresh `start` begins again.
//! Each published frame carries the time the kernel's interrupt handler ran for its frame end (`ktime_get_ns`, the
//! monotonic clock), and identity 0: this receiver takes no jobs. A frame end the kernel's event
//! queue dropped (it keeps ten) is not counted.
//!
//! # Settings
//!
//! The way to the ISP is the board file's, and there is no falling back from one to the other:
//!
//! - **The module**: `config` gives `block`, the ISP's platform device name as `lemnos.ko` lists
//!   it, and optionally `node`, the module's control node (default `/dev/lemnos`). `isp` must not
//!   be set, and `dma` must not be named.
//! - **The vendor device**: no `block`; the device's `uses` names `dma`, and `config` may give
//!   `isp`.
//!
//! Anything else (`block` with `isp` or `dma`, an empty `block`, the vendor way without `dma`)
//! is refused when the device binds (`INVALID_ARGUMENT`).
//!
//! From the device's `config` in the board file:
//!
//! - `isp`, `mipi`: the device nodes, default `/dev/vvcam-isp.0` (on the vendor's way only) and
//!   `/dev/vvcam-mipi.0`.
//! - `format`: `bayer` (default), `nv12`, `nv16`, `yuyv`, `bgr24` or `p010`; see [`Format`].
//! - `order`: the sensor's colour order, `rggb` (default), `grbg`, `gbrg` or `bggr`.
//!
//! Frames go out on the stream the board gives as `[devices.streams.frames]`, whose buffers must
//! hold a frame (`buffer_bytes` at least the configured layout's `size`). The layout `configure`
//! returns has the format's first-plane stride and the whole frame's size; planes follow each
//! other with no gap. Raw frames take [`Packing::Packed`] at 8 bits and [`Packing::Unpacked16`]
//! at 10 and 12; the other formats take [`Packing::Packed`], except P010, which is
//! [`Packing::Unpacked16`].
//!
//! The ring's memory is kept from one start to the next. On the vendor's way a start at a larger
//! frame size than the last one within
//! [`RELEASE_AFTER`](lemnos_driver_k230_capture_sys::RELEASE_AFTER) of a stop is refused with
//! `BUSY`: the block may still be writing the old memory, so it is not given back until then. On
//! the module's way a stop of a running path has the module reset the block, so the memory is
//! free at once.
//!
//! # Failures
//!
//! While streaming, each tick takes every frame end queued. A device that errors starts recovery:
//! the next ticks stop what is left and open, set up and start everything again, with the delay
//! between attempts doubling from 250 ms and capped at 8 s, and clearing when one succeeds. The
//! driver never gives up and never turns itself off; `stop` is what ends it. While recovering,
//! health is degraded and `start` is refused with `BUSY`. A frame that can't be copied out, or a
//! frame end after which the block names none of the ring, is counted in `errors` and is not a
//! reason to restart.
//!
//! Frame stop. On either way, a frame end that does not come for `max(4 x the measured frame
//! period, 200 ms)` restarts the frame path under the same backoff, with health failed and a reason
//! that names the stop. The period is the longest of the last four intervals between the frame ends'
//! times (the kernel's handler times), so a long exposure raises the bound with it. Before two frame
//! ends have given an interval the bound is `max(4 x the mode's nominal period, 3 s)`, the same
//! fallback as `rp1-csi2`. Frame ends count whether or not the consumer holds a buffer, so a consumer
//! holding every buffer is no stop here. Unmeasured on a board.
//!
//! On the module's way a frame that doesn't come within
//! [`FRAME_TIMEOUT_MS`](module::FRAME_TIMEOUT_MS) has the module reset the block, which is a
//! device error here: recovery starts it again.
//!
//! # Not delivered
//!
//! - On the vendor's way the host must provide the `dma` need. `LinuxBoard` answers `dma:<block>`
//!   through the Lemnos kernel module (`backends/linux/src/module/`): each buffer is the module's allocation for the
//!   named block, with the device address the module reports. That address is valid for the DMA
//!   setup of the block the module owns, and is the address the ISP sees only if both blocks have
//!   the same view of memory (no IOMMU, the same `dma-ranges`). The ISP is bound to the vendor's
//!   `vvcam-isp` driver, so the module cannot own it; which block to name, and whether the two
//!   agree, is checked on a board (`testing/board-runs/B23.md`, step 2). If that cannot be shown,
//!   the memory must not be handed to the ISP. The driver is built into `lemnosd`, so a board
//!   file can name it; until that check passes, it is for that board run.
//! - Scaling, cropping and the self paths: the main path writes the sensor's full frame.
//!
//! Untested on board: written from the vendor driver's interface and the ISP8000's open register
//! map; the check is `testing/board-runs/E30.md`.

#![forbid(unsafe_code)]

mod hw;
#[cfg(target_os = "linux")]
pub mod module;
pub mod plan;

#[cfg(all(test, target_os = "linux"))]
mod module_tests;
#[cfg(all(test, target_os = "linux"))]
mod tests;

use lemnos_contract_camera_receiver::{
    BayerOrder, CameraReceiver, Counters, FrameLayout, FrameStarts, Limits, LinkConfig,
    MAX_STREAM_NAME, Packing,
};
use lemnos_core::{
    Config, ConfigError, Description, DmaHandle, Health, Item, Key, Kind, Lifecycle, MethodId,
    Result, Status, StreamHandle, driver,
};
use lemnos_driver_k230_capture_sys::mipi::DevAttr;
use lemnos_driver_k230_capture_sys::regs::MIS_MP_FRAME_END;
use lemnos_driver_k230_capture_sys::{MiEvent, RING};
use lemnos_hal::blocks::StreamFrame;

#[cfg(target_os = "linux")]
pub use hw::LinuxHardware;
pub use hw::{Hardware, ISP_PATH, MIPI_PATH, Settings};
pub use lemnos_driver_k230_capture_sys as sys;
#[cfg(target_os = "linux")]
pub use module::{FrameSession, LinuxFrameSession, ModuleHardware};
pub use plan::{Format, Layout, MAX_DIMENSION, MIN_DIMENSION, WIDTH_STEP};

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
/// Most data lanes the receiver takes.
pub const MAX_LANES: u8 = 4;
/// Lowest per-lane link rate, Mbit/s: the D-PHY floor.
pub const MIN_LINK_RATE_MBPS: u32 = 80;
/// Highest per-lane link rate, Mbit/s: the fastest D-PHY setting the kernel takes.
pub const MAX_LINK_RATE_MBPS: u32 = 1600;
/// Bytes copied from the block's memory to the stream at a time.
pub const COPY_CHUNK: usize = 64 * 1024;

/// First delay between recovery attempts.
const BACKOFF_FIRST_NS: u64 = 250_000_000;
/// How often the device is read for frame ends while streaming.
const POLL_NS: u64 = 1_000_000;
/// Frame ends taken in one tick at most: the kernel's event queue holds ten.
const EVENTS_PER_TICK: usize = 11;

/// The delay before recovery attempt `attempt` (from 0), doubling to the cap.
#[must_use]
pub const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// The CSI-2 data type for raw samples of `depth` bits.
const fn raw_data_type(depth: u8) -> Option<u8> {
    match depth {
        8 => Some(0x2A),
        10 => Some(0x2B),
        12 => Some(0x2C),
        _ => None,
    }
}

/// `order` from its board file spelling.
#[must_use]
pub fn parse_order(order: &str) -> Option<BayerOrder> {
    Some(match order {
        "rggb" => BayerOrder::Rggb,
        "grbg" => BayerOrder::Grbg,
        "gbrg" => BayerOrder::Gbrg,
        "bggr" => BayerOrder::Bggr,
        _ => return None,
    })
}

/// The driver's own settings, beside where the devices are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub format: Format,
    pub order: BayerOrder,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            format: Format::Bayer,
            order: BayerOrder::Rggb,
        }
    }
}

fn options(cfg: &Config<'_>) -> Result<Options> {
    let defaults = Options::default();
    // The names are checked by check_config; these read them.
    let format = match cfg.str("format") {
        Some(f) => Format::parse(f).ok_or(Status::INVALID_ARGUMENT)?,
        None => defaults.format,
    };
    let order = match cfg.str("order") {
        Some(o) => parse_order(o).ok_or(Status::INVALID_ARGUMENT)?,
        None => defaults.order,
    };
    Ok(Options { format, order })
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

/// Which memory of the ring does what.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ring {
    /// The memory the block writes now.
    writing: usize,
    /// The memory neither written nor armed.
    free: usize,
}

impl Ring {
    /// As [`Capture::start`](lemnos_driver_k230_capture_sys::Capture::start) leaves it: memory 0
    /// loaded, memory 1 armed.
    const START: Self = Self {
        writing: 0,
        free: 2,
    };
}

/// What became of one frame end.
enum Delivery {
    Published,
    /// No consumer, no stream buffer free, or nothing new finished.
    Dropped,
    /// Couldn't be copied out, or the block named none of the ring.
    Bad,
}

/// The ISP8000 frame path as a camera receiver.
#[driver("k230-capture", provides = [CameraReceiver])]
pub struct K230Capture {
    #[needs(free)]
    frames: StreamHandle,
    /// The host's DMA memory, on the vendor's way. Moved into the hardware at bind, so the
    /// memory has one owner; what is left here is `None`. Declared here so the host provides it.
    #[needs(optional)]
    #[expect(dead_code, reason = "moved into the hardware at bind")]
    dma: Option<DmaHandle>,
    hw: Box<dyn Hardware>,
    options: Options,
    link: Option<LinkConfig>,
    layout: Option<Layout>,
    state: State,
    counters: Counters,
    ring: Ring,
    /// Frame ends since `start`.
    sequence: u64,
    /// Where a frame passes on its way to the stream: made once.
    chunk: Vec<u8>,
    /// Frame ends the block raised, delivered or not: the frame-stop watch counts these.
    frame_ends: u64,
    /// Frame ends since the last one delivered that found no stream buffer free: every buffer is
    /// with the consumer. Health names the count (the producer is waiting on the consumer's holds).
    waiting_drops: u64,
    /// When a frame end last came while streaming, or the first tick after a start. `None` until then.
    last_frame_ns: Option<u64>,
    /// Set while recovering from a frame stop, so health names it.
    frame_stopped: bool,
    /// The frame-end times the block reports, for the measured frame period.
    clock: FrameClock,
}

/// Health reasons, one per count of frame ends dropped with no buffer free, from 0 to `WAITING_TOP`.
/// The last one says that count or more. A reason is a static text, so the count is written into it.
macro_rules! waiting_reasons {
    ($($count:literal)* ; $top:literal) => {
        [
            $(concat!(
                "the producer is waiting for buffers the consumer holds: ", $count,
                " frame ends dropped with no buffer free"
            ),)*
            concat!(
                "the producer is waiting for buffers the consumer holds: ", $top,
                " or more frame ends dropped with no buffer free"
            ),
        ]
    };
}
/// The largest count a waiting reason names, and the reason for it and above.
const WAITING_TOP: u64 = 64;
const WAITING: [&str; 65] = waiting_reasons!(
    0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 33
    34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59 60 61 62 63
    ; 64
);

/// The shortest no-frame bound: frame stop is judged after this much at least.
const FRAME_STOP_MIN_NS: u64 = 200_000_000;
/// Frame periods a stop waits for before it is judged.
const FRAME_STOP_PERIODS: u64 = 4;
/// Until two frame ends have given an interval, a stop waits this long: the first frame carries the
/// sensor's exposure and readout, which the driver has not measured yet (the same allowance the
/// `ov9782` driver gives the first frame after a restart).
const FIRST_FRAME_NS: u64 = 3_000_000_000;

/// The frame intervals of the last few frame ends, from the times the kernel's handler ran for
/// them. The period is measured, not read from the mode: a long exposure stretches the frame (VTS),
/// so the real period can be far longer than the mode's nominal one. The same as `rp1-csi2`'s.
#[derive(Debug, Clone, Copy, Default)]
struct FrameClock {
    last: Option<u64>,
    intervals: [u64; 4],
    next: usize,
}

impl FrameClock {
    /// One frame end at `at_ns`. A frame end that is not later than the last gives no interval.
    fn frame(&mut self, at_ns: u64) {
        if let Some(prev) = self.last.filter(|prev| at_ns > *prev) {
            self.intervals[self.next] = at_ns - prev;
            self.next = (self.next + 1) % self.intervals.len();
        }
        self.last = Some(at_ns);
    }

    /// The longest of the last four intervals, or `None` before any pair has given one. The
    /// longest, so one short interval (a burst) cannot shorten the bound.
    fn period(&self) -> Option<u64> {
        let longest = self.intervals.iter().copied().max().unwrap_or(0);
        (longest > 0).then_some(longest)
    }
}

/// The frame period of the configured mode, from its active picture (width times height at the
/// pixel rate). A nominal figure: it leaves out blanking and any exposure stretch.
fn nominal_period_ns(link: &LinkConfig) -> Option<u64> {
    if link.pixel_rate_hz == 0 {
        return None;
    }
    let period = u128::from(link.width) * u128::from(link.height) * 1_000_000_000
        / u128::from(link.pixel_rate_hz);
    u64::try_from(period).ok()
}

/// The no-frame bound: `max(4 x the measured frame period, 200 ms)`. Before a period is measured
/// it is `max(4 x the nominal period, 3 s)`, so a slow start is not a stop.
fn frame_stop_bound_ns(link: &LinkConfig, period: Option<u64>) -> Option<u64> {
    match period {
        Some(period) => Some(
            period
                .saturating_mul(FRAME_STOP_PERIODS)
                .max(FRAME_STOP_MIN_NS),
        ),
        None => {
            let nominal = nominal_period_ns(link)?.saturating_mul(FRAME_STOP_PERIODS);
            Some(nominal.max(FIRST_FRAME_NS))
        }
    }
}

/// The settings this driver reads from its board file config.
const KEYS: &[Key] = &[
    Key::optional("block", Kind::Str),
    Key::optional("isp", Kind::Str),
    Key::optional("mipi", Kind::Str),
    Key::optional("node", Kind::Str),
    Key::optional("format", Kind::Str),
    Key::optional("order", Kind::Str),
];

impl Lifecycle for K230Capture {
    type Needs = K230CaptureNeeds;
    const TICKS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        if cfg.str("block") == Some("") {
            return Err(ConfigError::new(
                "block",
                "must be a platform device name, not empty",
            ));
        }
        // One way to the ISP, not both. Which way the device has is checked at bind, against its needs.
        if cfg.str("block").is_some() && cfg.str("isp").is_some() {
            return Err(ConfigError::new("isp", "cannot be given with block"));
        }
        for key in ["isp", "mipi"] {
            if cfg.str(key) == Some("") {
                return Err(ConfigError::new(key, "must be a device path, not empty"));
            }
        }
        if let Some(name) = cfg.str("format")
            && Format::parse(name).is_none()
        {
            return Err(ConfigError::new(
                "format",
                "must be bayer, nv12, nv16, yuyv, bgr24 or p010",
            ));
        }
        if let Some(name) = cfg.str("order")
            && parse_order(name).is_none()
        {
            return Err(ConfigError::new(
                "order",
                "must be rggb, grbg, gbrg or bggr",
            ));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn start(needs: K230CaptureNeeds, cfg: &Config<'_>) -> Result<Self> {
        let settings = Settings::new(cfg.str("isp"), cfg.str("mipi"))?;
        let options = options(cfg)?;
        let hw: Box<dyn Hardware> = match (cfg.str("block"), cfg.str("isp"), needs.dma) {
            (Some(block), None, None) if !block.is_empty() => {
                let node = std::path::PathBuf::from(cfg.str("node").unwrap_or(module::NODE));
                Box::new(ModuleHardware::new(
                    &settings,
                    Box::new(LinuxFrameSession::new(node, block.to_owned())),
                ))
            }
            (None, _, Some(dma)) => {
                Box::new(LinuxHardware::new(settings, Box::new(hw::HeldDma(dma))))
            }
            // Both ways at once, or half of one: the board file is wrong, and nothing is guessed.
            _ => return Err(Status::INVALID_ARGUMENT),
        };
        Ok(Self::with_hardware(hw, needs.frames, options))
    }

    #[cfg(not(target_os = "linux"))]
    fn start(_needs: K230CaptureNeeds, _cfg: &Config<'_>) -> Result<Self> {
        Err(Status::NOT_SUPPORTED)
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::action("start", START));
        out.add(Item::action("stop", STOP));
    }

    fn health(&self) -> Health {
        match self.state {
            State::Recovering { .. } if self.frame_stopped => Health::Failed(
                "no frame end arrived within the bound for the configured mode: restarting the frame path",
            ),
            State::Recovering { .. } => {
                Health::Failed("the frame path failed while streaming: restarting it")
            }
            State::Streaming if self.waiting_drops > 0 => {
                Health::Degraded(WAITING[self.waiting_drops.min(WAITING_TOP) as usize])
            }
            _ => Health::Ok,
        }
    }

    /// Takes the frame ends queued when streaming, and runs a due recovery attempt. Returns when
    /// next.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        match self.state {
            State::Stopped => None,
            State::Streaming => {
                let ends = self.frame_ends;
                if self.pump().is_err() {
                    self.state = State::Recovering {
                        attempt: 0,
                        due: None,
                    };
                    return self.tick(now_ns);
                }
                if self.frame_ends != ends || self.last_frame_ns.is_none() {
                    self.last_frame_ns = Some(now_ns);
                }
                if self.frame_stop_judged(now_ns) {
                    self.frame_stopped = true;
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
                    self.frame_stopped = false;
                    self.last_frame_ns = Some(now_ns);
                    // A restart is a new stream: its first interval is not the gap across the stop.
                    self.clock = FrameClock::default();
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

impl K230Capture {
    /// Whether no frame end has come for the bound of the configured mode. Frame ends are counted
    /// whether or not the consumer holds a buffer (a frame it cannot take is dropped, and still a
    /// frame end), so a consumer holding every buffer is not a stop here.
    fn frame_stop_judged(&self, now_ns: u64) -> bool {
        let (Some(link), Some(since)) = (self.link, self.last_frame_ns) else {
            return false;
        };
        let Some(bound) = frame_stop_bound_ns(&link, self.clock.period()) else {
            return false;
        };
        now_ns.saturating_sub(since) > bound
    }

    /// A driver over `hw` producing into `frames`, for a simulated board or a test. `hw` owns
    /// the DMA memory.
    #[must_use]
    pub fn with_hardware(hw: Box<dyn Hardware>, frames: StreamHandle, options: Options) -> Self {
        Self {
            frames,
            dma: None,
            hw,
            options,
            link: None,
            layout: None,
            state: State::Stopped,
            counters: Counters::default(),
            ring: Ring::START,
            sequence: 0,
            chunk: vec![0; COPY_CHUNK],
            frame_ends: 0,
            waiting_drops: 0,
            last_frame_ns: None,
            frame_stopped: false,
            clock: FrameClock::default(),
        }
    }

    /// The configured layout, once `configure` has set one.
    #[must_use]
    pub fn layout(&self) -> Option<Layout> {
        self.layout
    }

    /// Stops what was running, then opens and sets up everything again as configured. Called
    /// with a layout set.
    fn begin(&mut self) -> Result<()> {
        let (Some(link), Some(layout)) = (self.link, self.layout) else {
            return Err(Status::INVALID_ARGUMENT);
        };
        let pattern = plan::bayer_pattern(self.options.order).ok_or(Status::INVALID_ARGUMENT)?;
        let attr = DevAttr::csi(
            link.lanes,
            link.virtual_channel,
            link.bit_depth,
            link.link_rate_mbps,
            link.width,
            link.height,
        )
        .map_err(|_| Status::INVALID_ARGUMENT)?;
        // Whatever was running ends first; a failure here is not a reason to not try the start.
        let _ = self.halt();
        self.hw.open()?;
        self.halt()?;
        self.hw.set_link(&attr)?;
        self.hw.provide(u64::from(layout.bytes))?;
        for &(addr, value) in plan::setup(&layout, pattern, link.bit_depth).as_slice() {
            self.hw.write(addr, value)?;
        }
        self.hw.start(layout.output, layout.planes)?;
        for &(addr, value) in plan::run(&layout).as_slice() {
            self.hw.write(addr, value)?;
        }
        self.counters = Counters::default();
        self.ring = Ring::START;
        Ok(())
    }

    /// The ISP stopped, then the memory interface off.
    fn halt(&mut self) -> Result<()> {
        let mut result = Ok(());
        for &(addr, value) in plan::halt().as_slice() {
            if let Err(e) = self.hw.write(addr, value) {
                result = Err(e);
            }
        }
        self.hw.stop().and(result)
    }

    /// Takes the frame ends queued, at most a queue's worth. An error means the device failed.
    fn pump(&mut self) -> Result<()> {
        for _ in 0..EVENTS_PER_TICK {
            let Some(event) = self.hw.next_event()? else {
                return Ok(());
            };
            if event.status & MIS_MP_FRAME_END == 0 {
                continue;
            }
            self.frame_ends = self.frame_ends.wrapping_add(1);
            self.clock.frame(event.timestamp_ns);
            self.sequence = self.sequence.wrapping_add(1);
            let counter = match self.frame_end(&event)? {
                Delivery::Published => &mut self.counters.frames,
                Delivery::Dropped => &mut self.counters.dropped,
                Delivery::Bad => &mut self.counters.errors,
            };
            *counter = counter.saturating_add(1);
        }
        Ok(())
    }

    /// One frame end: moves the ring on, arms the free memory, and delivers the finished one.
    fn frame_end(&mut self, event: &MiEvent) -> Result<Delivery> {
        let Some(now) = self.hw.active()? else {
            return Ok(Delivery::Bad);
        };
        if now == self.ring.writing {
            // Already moved on for this frame end (events that arrived together), or the block
            // has not moved: nothing new finished.
            return Ok(Delivery::Dropped);
        }
        if now == self.ring.free {
            // The block is writing memory that was never armed: the ring is lost.
            return Ok(Delivery::Bad);
        }
        let done = self.ring.writing;
        // Arm the free memory first, so the block never comes back to the one being read.
        self.hw.rearm(self.ring.free)?;
        self.ring = Ring {
            writing: now,
            free: done,
        };
        Ok(self.deliver(done, event.timestamp_ns))
    }

    /// Copies memory `slot`'s frame into a stream buffer and publishes it.
    fn deliver(&mut self, slot: usize, timestamp_ns: u64) -> Delivery {
        let Some(layout) = self.layout else {
            return Delivery::Bad;
        };
        let buffer = match self.frames.next_buffer() {
            Ok(Some(buffer)) => buffer,
            Ok(None) => {
                // Every stream buffer is with the consumer: the frame has nowhere to go.
                self.waiting_drops = self.waiting_drops.saturating_add(1);
                return Delivery::Dropped;
            }
            Err(_) => return Delivery::Dropped,
        };
        let frame = StreamFrame {
            buffer,
            bytes: 0,
            sequence: self.sequence,
            identity: 0,
            timestamp_ns,
            end_ns: 0,
        };
        if self.hw.take(slot).is_err() {
            let _ = self.frames.publish(frame);
            return Delivery::Bad;
        }
        let total = layout.bytes as usize;
        let mut offset = 0usize;
        while offset < total {
            let n = (total - offset).min(self.chunk.len());
            let chunk = &mut self.chunk[..n];
            if self.hw.copy_out(slot, offset as u64, chunk).is_err() {
                // Nothing usable in the buffer: handed back empty, which the stream drops.
                let _ = self.frames.publish(frame);
                return Delivery::Bad;
            }
            match self.frames.write(buffer, offset, chunk) {
                Ok(true) => offset += n,
                Ok(false) => {
                    let _ = self.frames.publish(frame);
                    return Delivery::Dropped;
                }
                Err(_) => {
                    let _ = self.frames.publish(frame);
                    return Delivery::Bad;
                }
            }
        }
        let published = self.frames.publish(StreamFrame {
            bytes: layout.bytes,
            ..frame
        });
        match published {
            Ok(()) => {
                self.waiting_drops = 0;
                Delivery::Published
            }
            Err(_) => Delivery::Dropped,
        }
    }
}

impl CameraReceiver for K230Capture {
    fn limits(&mut self) -> Result<Limits> {
        Ok(Limits {
            max_lanes: MAX_LANES,
            max_link_rate_mbps: MAX_LINK_RATE_MBPS,
            max_width: MAX_DIMENSION,
            max_height: MAX_DIMENSION,
        })
    }

    fn configure(&mut self, link: LinkConfig, packing: Packing) -> Result<FrameLayout> {
        if self.state != State::Stopped {
            return Err(Status::BUSY);
        }
        if raw_data_type(link.bit_depth) != Some(link.data_type)
            || !(MIN_LINK_RATE_MBPS..=MAX_LINK_RATE_MBPS).contains(&link.link_rate_mbps)
        {
            return Err(Status::INVALID_ARGUMENT);
        }
        DevAttr::csi(
            link.lanes,
            link.virtual_channel,
            link.bit_depth,
            link.link_rate_mbps,
            link.width,
            link.height,
        )
        .map_err(|_| Status::INVALID_ARGUMENT)?;
        let layout = plan::layout(self.options.format, link.bit_depth, link.width, link.height)
            .ok_or(Status::INVALID_ARGUMENT)?;
        if layout.packing != packing {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.link = Some(link);
        self.layout = Some(layout);
        Ok(FrameLayout {
            width: layout.width,
            height: layout.height,
            bit_depth: layout.bit_depth,
            packing: layout.packing,
            stride: layout.stride,
            size: layout.bytes,
        })
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
        // A fresh start numbers its frames from zero again. A recovery restart does not (`begin`).
        self.sequence = 0;
        self.waiting_drops = 0;
        self.begin()?;
        self.state = State::Streaming;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Stop means stop, whatever the driver was doing. Recovery is over either way.
        self.state = State::Stopped;
        self.last_frame_ns = None;
        self.frame_stopped = false;
        self.clock = FrameClock::default();
        self.halt()
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.counters)
    }

    /// No ISP takes this block's frames in hardware: it writes them to memory, and the frame
    /// stream is where they go.
    fn feeds(&mut self, _into: &mut [u8]) -> Result<usize> {
        Ok(0)
    }

    /// This receiver captures no embedded data: nothing in its driver names where the block writes a
    /// sensor's embedded lines, and no sensor on a board in the tree carries any. Answers 0.
    /// Not counted yet: the frame-start count is not wired in this driver.
    fn frame_starts(&mut self) -> Result<Option<FrameStarts>> {
        Err(Status::NOT_SUPPORTED)
    }

    fn embedded_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Ok(0)
    }

    /// Unknown: the block's driver describes no sensor, so the link is not confirmed from here.
    fn source(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn layout(&mut self) -> Result<Option<FrameLayout>> {
        Ok(self.layout.map(|layout| FrameLayout {
            width: layout.width,
            height: layout.height,
            bit_depth: layout.bit_depth,
            packing: layout.packing,
            stride: layout.stride,
            size: layout.bytes,
        }))
    }

    fn inputs(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn input(&mut self) -> Result<u32> {
        Err(Status::NOT_SUPPORTED)
    }
}

const _: () = assert!(RING == 3);
