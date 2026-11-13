//! The rkcif camera receiver as `camera.receiver/1`, through the vendor kernel's own driver and the
//! media controller: it routes a MIPI channel to its capture node, keeps that route, and answers
//! which ISP the receiver feeds.
//!
//! This block has no Lemnos kernel code yet, so the vendor kernel driver stays bound and this
//! driver speaks to it through `lemnos-v4l2`. It only receives: the sensor is set up by its own
//! driver, and the ISP takes the receiver's frames inside the kernel, through the subdevice the
//! receiver exposes in the ISP's graph. Frames never cross this crate.
//!
//! Which ISP the receiver feeds is read from the graphs, not from a table: see [`route`] and
//! [`Rkcif::feeds`]. The board file's `link` on the sensor names this receiver and the
//! receiver's names its ISP, which a client reads from the device list; the contract's `feeds`
//! answers the ISP whose link is on in the hardware now.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `device`: the receiver's media device model (its device name in the kernel). With only one
//!   receiver on the machine, this may be left out.
//!
//! The MIPI channel routed is the link's virtual channel: without HDR the vendor driver captures
//! virtual channel `n` on channel `n`. The link's lanes, rate and data type are the kernel sensor
//! driver's, read by the vendor driver from it; `configure` checks the link fits the receiver and
//! works out the layout the receiver writes in its default compact mode.
//!
//! # Raw frames
//!
//! The receiver's capture node can also hand its raw frames to a caller, through a frame
//! stream this driver produces. That path is on when the board file gives the device a
//! `frames` stream (`[devices.streams.frames]`) and names the capture node (`capture`, below):
//! `frame_stream` then answers `frames`, `start` starts the node after routing the channel,
//! and `counters` counts what it delivered. `lemnos-driver-v4l2-capture`'s pump moves the
//! frames: the node fills the stream's own buffers when they are dma-bufs
//! (`memory = "dma-buf"`) and the node takes them, with nothing copied, and otherwise fills
//! buffers of its own with one copy per frame into the stream. Without both, `frame_stream`
//! and `counters` answer `NOT_SUPPORTED` as before, and frames reach only the ISP.
//!
//! More settings, for the raw path:
//!
//! - `capture`: the capture node's media entity name (the routed channel's video node).
//! - `order`: the colour order, `mono`, `rggb`, `grbg`, `gbrg` or `bggr`, default `mono`. The
//!   pixel format asked of the node carries it; there is no monochrome format past 8 bits.
//! - `buffers`: buffers the node fills when frames are copied, 2 to 32, default 4.
//! - `embedded`: a metadata node's media entity name, for the sensor's embedded lines. Used with the
//!   `embedded` stream (`[devices.streams.embedded]`): each line is copied onto that stream with the
//!   frame's sequence (`lemnos-driver-v4l2-capture`'s `pump_embedded`). Needs `embedded_format`
//!   (the node's data format, four characters) and `embedded_bytes` (a line's length, above 0).
//!   Left out, the receiver captures no embedded data.
//!
//! # Layouts
//!
//! `configure` takes either packing, for the raw path:
//!
//! - `Packed`, at 8, 10 or 12 bits: the receiver's compact layout, samples end to end and
//!   lines of `width * depth / 8` bytes rounded up to 256.
//! - `Unpacked16`, at 10 or 12 bits: one sample per 16-bit little-endian word, in its low
//!   bits, lines of `width * 2` bytes rounded up to 8. This is exactly what the kernel uAPI's
//!   own formats for those depths name (`V4L2_PIX_FMT_SRGGB10`, `V4L2_PIX_FMT_Y12` and the
//!   rest are "expanded to 16 bits"), and the node is asked for that format.
//!
//! The vendor driver writes its compact layout under the same format names unless told
//! otherwise, so before each format is set the driver sets the node's memory mode with the
//! vendor driver's own call (`lemnos-driver-rkcif-sys`): compact for `Packed`, the low-aligned
//! word for `Unpacked16`. A kernel without the call can only be compact: `Packed` carries on
//! there, `Unpacked16` fails its start, and either way the stride the node answers the format
//! with must be the one the layout says, or the start is refused with `NOT_SUPPORTED`.
//! Without a raw path frames reach only the ISP, inside the kernel, and `Unpacked16` is
//! refused with `NOT_SUPPORTED`.
//!
//! Failures. While routed, every second the driver reads the receiver's graph without allocating
//! and checks the channel's link is still on. A link someone turned off, or a graph that can't be
//! read, starts recovery: the next ticks open the media device again and route the channel again,
//! with the delay between attempts doubling from 250 ms and capped at 8 s, and clearing when one
//! succeeds. The driver never gives up and never turns itself off; `stop` is what ends it. While
//! recovering, health is degraded and `start` is refused with `BUSY`. A failed `start` is reported
//! to its caller, who retries it.
//!
//! Untested on board: written from the vendor driver's source; the checks are
//! `testing/board-runs/E8.md` and, for `Unpacked16`, `testing/board-runs/H5a.md`.

mod hw;
pub mod route;

#[cfg(test)]
mod tests;

use lemnos_contract_camera_receiver::{
    CameraReceiver, Counters, FrameLayout, FrameStarts, Limits, LinkConfig, Packing,
};
use lemnos_core::{
    Config, Description, Health, Item, Lifecycle, MethodId, Result, Status, StreamHandle, driver,
};
use lemnos_driver_v4l2_capture::{self as capture, Pump, Wanted, fourcc, parse_order};
use lemnos_v4l2::FourCc;

pub use hw::{Hardware, LinuxHardware, Settings, link_on};
pub use route::{IspGraph, IspLink, Plan};

const START: MethodId = MethodId {
    contract: 0,
    method: 5,
};
const STOP: MethodId = MethodId {
    contract: 0,
    method: 6,
};

/// Data lanes the receiver's D-PHY takes (`phy-rockchip-csi2-dphy-hw.c`, data lanes 0 to 3).
pub const MAX_LANES: u8 = 4;
/// Lowest per-lane link rate, Mbit/s: the D-PHY high-speed floor.
pub const MIN_LINK_RATE_MBPS: u32 = 80;
/// Highest per-lane link rate, Mbit/s: the top of the D-PHY's frequency range table
/// (`phy-rockchip-csi2-dphy-hw.c`, last range 2499).
pub const MAX_LINK_RATE_MBPS: u32 = 2500;
/// The receiver's frame size limit in each direction (`capture.c`, `CIF_MAX_WIDTH`,
/// `CIF_MAX_HEIGHT`).
pub const MAX_DIMENSION: u32 = 8192;
/// MIPI channels, one capture node each (`RKCIF_MAX_STREAM_MIPI`).
pub const CHANNELS: u8 = 4;
/// Compact raw lines are aligned to this many bytes (`capture.c`, `ALIGN(width * bpp / 8, 256)`).
pub const COMPACT_LINE_ALIGN: u64 = 256;
/// Lines of one sample per 16-bit word are aligned to this many bytes (`capture.c`,
/// `rkcif_set_fmt`, `ALIGN(width * bpp / CIF_YUV_STORED_BIT_WIDTH, 8)` with `bpp` 16).
pub const WORD_LINE_ALIGN: u64 = 8;

/// [`Wanted::mode`] for the compact layout.
const MODE_COMPACT: u32 = 0;
/// [`Wanted::mode`] for one sample per 16-bit word, low-aligned.
const MODE_WORDS: u32 = 1;

/// First delay between recovery attempts.
const BACKOFF_FIRST_NS: u64 = 250_000_000;
/// How often the route is checked while routed.
const CHECK_NS: u64 = 1_000_000_000;

/// The delay before recovery attempt `attempt` (from 0), doubling to the 8 s cap.
const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// Where the driver is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Stopped,
    Routed {
        /// When the next look at the route is due.
        next_check: Option<u64>,
    },
    Recovering {
        attempt: u32,
        /// When the next attempt is due; `None` until a tick has had the host's clock.
        due: Option<u64>,
    },
}

/// The rkcif camera receiver.
#[driver("rkcif", provides = [CameraReceiver])]
pub struct Rkcif {
    /// Raw frames go out here, when the board gives the stream.
    #[needs(optional, free)]
    frames: Option<StreamHandle>,
    /// The embedded data goes out here, when the board gives the stream and names a metadata node.
    #[needs(optional, free)]
    embedded: Option<StreamHandle>,
    hw: Box<dyn Hardware>,
    /// The capture node, when the board names it.
    node: Option<Box<dyn capture::Hardware>>,
    /// The metadata node the embedded lines come off (`config.embedded`), with the data format
    /// and the line's bytes (`config.embedded_format`, `config.embedded_bytes`).
    embedded_node: Option<Box<dyn capture::Hardware>>,
    embedded_line: Option<(FourCc, u32)>,
    /// What moves the node's frames into the stream.
    pump: Pump,
    raw: Raw,
    link: Option<LinkConfig>,
    packing: Packing,
    state: State,
}

/// The stream raw frames go out on: the need's name, which is the name a consumer opens it by.
pub const STREAM: &str = "frames";
/// The stream the embedded lines go out on: the need's name.
pub const EMBEDDED: &str = "embedded";
/// How often the capture node is read for filled buffers while it streams.
const POLL_NS: u64 = 1_000_000;

/// How the raw path asks the capture node for frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Raw {
    /// The colour order the pixel format carries.
    pub order: lemnos_contract_camera_receiver::BayerOrder,
    /// Buffers the node fills when frames are copied.
    pub buffers: u32,
}

impl Default for Raw {
    fn default() -> Self {
        Self {
            order: lemnos_contract_camera_receiver::BayerOrder::Mono,
            buffers: 4,
        }
    }
}

/// How a raw frame is laid out. Compact: lines of `width * depth / 8` bytes, rounded down as
/// the vendor driver does, then up to [`COMPACT_LINE_ALIGN`]. One sample per 16-bit word: lines
/// of `width * 2` bytes rounded up to [`WORD_LINE_ALIGN`].
fn layout(link: &LinkConfig, packing: Packing) -> Result<FrameLayout> {
    let (line, align) = match packing {
        Packing::Packed if matches!(link.bit_depth, 8 | 10 | 12) => (
            u64::from(link.width) * u64::from(link.bit_depth) / 8,
            COMPACT_LINE_ALIGN,
        ),
        Packing::Unpacked16 if matches!(link.bit_depth, 10 | 12) => {
            (u64::from(link.width) * 2, WORD_LINE_ALIGN)
        }
        _ => return Err(Status::INVALID_ARGUMENT),
    };
    let stride = line.div_ceil(align) * align;
    let size = stride * u64::from(link.height);
    Ok(FrameLayout {
        width: link.width,
        height: link.height,
        bit_depth: link.bit_depth,
        packing,
        stride: u32::try_from(stride).map_err(|_| Status::INVALID_ARGUMENT)?,
        size: u32::try_from(size).map_err(|_| Status::INVALID_ARGUMENT)?,
    })
}

/// Before the node's format: the memory mode `wanted` needs, through the vendor driver's own
/// call. A kernel without the call can only write its compact layout, so that is no failure
/// for compact; the stride the node answers the format with is checked after either way.
fn prepare_node(fd: std::os::fd::BorrowedFd<'_>, wanted: &Wanted) -> Result<()> {
    use lemnos_driver_rkcif_sys::{MemoryMode, set_memory_mode};
    let mode = match wanted.mode {
        MODE_WORDS => MemoryMode::WordLowAlign,
        _ => MemoryMode::Compact,
    };
    match set_memory_mode(fd, mode) {
        Ok(()) => Ok(()),
        Err(e) if mode == MemoryMode::Compact && e.raw_os_error() == Some(libc::ENOTTY) => Ok(()),
        Err(e) => Err(match e.raw_os_error() {
            Some(libc::ENOTTY | libc::EINVAL) => Status::NOT_SUPPORTED,
            Some(libc::EBUSY) => Status::BUSY,
            Some(libc::ENODEV) => Status::NO_DEVICE,
            _ => Status::IO,
        }),
    }
}

impl Lifecycle for Rkcif {
    type Needs = RkcifNeeds;
    const TICKS: bool = true;

    fn start(needs: RkcifNeeds, cfg: &Config<'_>) -> Result<Self> {
        let settings = Settings::new(cfg.str("device"))?;
        let mut raw = Raw::default();
        if let Some(order) = cfg.str("order") {
            raw.order = parse_order(order).ok_or(Status::INVALID_ARGUMENT)?;
        }
        if cfg.get("buffers").is_some() {
            raw.buffers = cfg
                .int("buffers")
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| (2..=32).contains(n))
                .ok_or(Status::INVALID_ARGUMENT)?;
        }
        let node: Option<Box<dyn capture::Hardware>> = match cfg.str("capture") {
            None => None,
            Some(name) => Some(Box::new(
                capture::LinuxHardware::new(capture::Settings::new(name)?)
                    .with_prepare(Box::new(prepare_node)),
            )),
        };
        let (embedded_node, embedded_line) = match cfg.str("embedded") {
            None => (None, None),
            Some(name) => {
                let format = cfg
                    .str("embedded_format")
                    .and_then(|f| f.parse::<FourCc>().ok())
                    .ok_or(Status::INVALID_ARGUMENT)?;
                let bytes = cfg
                    .int("embedded_bytes")
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .ok_or(Status::INVALID_ARGUMENT)?;
                let node: Box<dyn capture::Hardware> = Box::new(capture::LinuxHardware::metadata(
                    capture::Settings::new(name)?,
                ));
                (Some(node), Some((format, bytes)))
            }
        };
        let mut driver = Self::with_hardware(Box::new(LinuxHardware::new(settings)));
        driver.frames = needs.frames;
        driver.embedded = needs.embedded;
        driver.node = node;
        driver.embedded_node = embedded_node;
        driver.embedded_line = embedded_line;
        driver.raw = raw;
        Ok(driver)
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::action("start", START));
        out.add(Item::action("stop", STOP));
    }

    fn health(&self) -> Health {
        match self.state {
            State::Recovering { .. } => {
                Health::Degraded("the channel's route was lost while routed: routing it again")
            }
            _ => Health::Ok,
        }
    }

    /// Checks the route when routed, and runs a due recovery attempt. Returns when next.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        // The capture node is read every tick it streams; a failure is the route's failure
        // too, and both are set up again.
        if matches!(self.state, State::Routed { .. })
            && (self.pump_frames().is_err() || self.pump_embedded().is_err())
        {
            self.state = State::Recovering {
                attempt: 0,
                due: None,
            };
        }
        let next = self.tick_route(now_ns)?;
        Some(
            if self.raw_path() && matches!(self.state, State::Routed { .. }) {
                next.min(now_ns.saturating_add(POLL_NS))
            } else {
                next
            },
        )
    }
}

impl Rkcif {
    /// Checks the route when routed, and runs a due recovery attempt. Returns when next.
    fn tick_route(&mut self, now_ns: u64) -> Option<u64> {
        match self.state {
            State::Stopped => None,
            State::Routed { next_check: None } => {
                let at = now_ns.saturating_add(CHECK_NS);
                self.state = State::Routed {
                    next_check: Some(at),
                };
                Some(at)
            }
            State::Routed {
                next_check: Some(at),
            } if now_ns < at => Some(at),
            State::Routed { .. } => {
                if self.hw.check().is_err() {
                    self.state = State::Recovering {
                        attempt: 0,
                        due: None,
                    };
                    return self.tick_route(now_ns);
                }
                let at = now_ns.saturating_add(CHECK_NS);
                self.state = State::Routed {
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
            State::Recovering { attempt, .. } => {
                // Whatever was open may be stale: a receiver probed again is a new device.
                self.hw.close();
                match self
                    .route()
                    .and_then(|()| self.begin_frames())
                    .and_then(|()| self.begin_embedded())
                {
                    Ok(()) => {
                        let at = now_ns.saturating_add(CHECK_NS);
                        self.state = State::Routed {
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
                }
            }
        }
    }
}

impl Rkcif {
    /// A driver over `hw`, for a simulated board or a test.
    #[must_use]
    pub fn with_hardware(hw: Box<dyn Hardware>) -> Self {
        Self {
            frames: None,
            embedded: None,
            hw,
            node: None,
            embedded_node: None,
            embedded_line: None,
            pump: Pump::new(),
            raw: Raw::default(),
            link: None,
            packing: Packing::Packed,
            state: State::Stopped,
        }
    }

    /// The driver with its embedded path: lines from the metadata node `node`, `bytes` long in
    /// data format `format`, into `stream`. For a simulated board or a test.
    #[must_use]
    pub fn with_embedded(
        mut self,
        node: Box<dyn capture::Hardware>,
        stream: StreamHandle,
        format: FourCc,
        bytes: u32,
    ) -> Self {
        self.embedded_node = Some(node);
        self.embedded = Some(stream);
        self.embedded_line = Some((format, bytes));
        self
    }

    /// The driver with its raw path: frames from `node` into `frames`, asked for as `raw`
    /// says. For a simulated board or a test.
    #[must_use]
    pub fn with_frames(
        mut self,
        node: Box<dyn capture::Hardware>,
        frames: StreamHandle,
        raw: Raw,
    ) -> Self {
        self.node = Some(node);
        self.frames = Some(frames);
        self.raw = raw;
        self
    }

    /// Whether raw frames are delivered: the board gave both the stream and the node.
    fn raw_path(&self) -> bool {
        self.frames.is_some() && self.node.is_some()
    }

    /// How raw frames reach the stream since the last start.
    #[must_use]
    pub fn mode(&self) -> capture::Mode {
        self.pump.mode()
    }

    /// Starts the capture node for the configured link, when there is a raw path.
    fn begin_frames(&mut self) -> Result<()> {
        let (Some(node), Some(frames)) = (self.node.as_deref_mut(), self.frames.as_deref()) else {
            return Ok(());
        };
        let link = self.link.ok_or(Status::INVALID_ARGUMENT)?;
        let layout = layout(&link, self.packing)?;
        let wanted = Wanted {
            // The vendor driver takes its input from the route, not from an input index.
            input: None,
            fourcc: fourcc(self.raw.order, link.bit_depth).ok_or(Status::INVALID_ARGUMENT)?,
            width: layout.width,
            height: layout.height,
            stride: layout.stride,
            size: layout.size,
            mode: match self.packing {
                Packing::Packed => MODE_COMPACT,
                Packing::Unpacked16 => MODE_WORDS,
            },
        };
        self.pump.begin(node, frames, &wanted, self.raw.buffers)
    }

    /// Takes what the capture node has filled, when there is a raw path.
    fn pump_frames(&mut self) -> Result<()> {
        match (self.node.as_deref_mut(), self.frames.as_deref_mut()) {
            (Some(node), Some(frames)) => self.pump.pump(node, frames),
            _ => Ok(()),
        }
    }

    /// Starts the metadata node the embedded lines come off, when the board gives both it and
    /// the stream. Whatever it was doing ends first.
    fn begin_embedded(&mut self) -> Result<()> {
        let (Some(node), Some((format, bytes)), Some(_)) = (
            self.embedded_node.as_deref_mut(),
            self.embedded_line,
            self.embedded.as_deref(),
        ) else {
            return Ok(());
        };
        let _ = node.stop();
        capture::start_embedded(node, format, bytes)
    }

    /// Moves the metadata node's filled lines onto the embedded stream, when both are there.
    fn pump_embedded(&mut self) -> Result<()> {
        match (
            self.embedded_node.as_deref_mut(),
            self.embedded.as_deref_mut(),
        ) {
            (Some(node), Some(stream)) => capture::pump_embedded(node, stream).map(|_| ()),
            _ => Ok(()),
        }
    }

    /// Routes the configured link's channel to its capture node and watches the route. Called
    /// with a link configured.
    fn route(&mut self) -> Result<()> {
        let link = self.link.ok_or(Status::INVALID_ARGUMENT)?;
        let topology = self.hw.topology()?;
        let plan = route::plan(&topology, u32::from(link.virtual_channel))?;
        for (source, sink) in &plan.disable {
            self.hw.setup_link(*source, *sink, false)?;
        }
        if let Some((source, sink)) = plan.enable {
            self.hw.setup_link(source, sink, true)?;
        }
        self.hw.watch(plan.link)
    }

    /// Every ISP this receiver is wired to, and whether each link is on: the ISP whose link is on
    /// takes its frames ([`route::feeding`]). Read from the receiver's and the ISPs' graphs each
    /// call. Empty when no ISP's graph names this receiver.
    pub fn feeds(&mut self) -> Result<Vec<IspLink>> {
        let model = self.hw.model()?;
        let isps = self.hw.isps()?;
        Ok(route::feeds(&model, &isps))
    }
}

impl CameraReceiver for Rkcif {
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
        if link.lanes == 0
            || link.lanes > MAX_LANES
            || link.virtual_channel >= CHANNELS
            || !(1..=63).contains(&link.data_type)
            || !(MIN_LINK_RATE_MBPS..=MAX_LINK_RATE_MBPS).contains(&link.link_rate_mbps)
            || link.width == 0
            || link.width > MAX_DIMENSION
            || link.height == 0
            || link.height > MAX_DIMENSION
        {
            return Err(Status::INVALID_ARGUMENT);
        }
        let layout = layout(&link, packing)?;
        // The words are the capture node's to write; without it frames reach only the ISP.
        if packing == Packing::Unpacked16 && !self.raw_path() {
            return Err(Status::NOT_SUPPORTED);
        }
        self.link = Some(link);
        self.packing = packing;
        Ok(layout)
    }

    fn link(&mut self) -> Result<Option<LinkConfig>> {
        Ok(self.link)
    }

    fn frame_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        if !self.raw_path() {
            return Err(Status::NOT_SUPPORTED);
        }
        into.get_mut(..STREAM.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(STREAM.as_bytes());
        Ok(STREAM.len())
    }

    fn start(&mut self) -> Result<()> {
        if self.link.is_none() {
            return Err(Status::INVALID_ARGUMENT);
        }
        if self.state != State::Stopped {
            return Err(Status::BUSY);
        }
        self.route()?;
        if let Err(status) = self.begin_frames().and_then(|()| self.begin_embedded()) {
            if let Some(node) = self.node.as_deref_mut() {
                let _ = node.stop();
            }
            if let Some(node) = self.embedded_node.as_deref_mut() {
                let _ = node.stop();
            }
            return Err(status);
        }
        self.state = State::Routed { next_check: None };
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // The route stays as it is: the ISP may be streaming through it, and only its own driver
        // stops that. Recovery is over either way.
        self.state = State::Stopped;
        // The capture nodes are this driver's own, and end with it.
        if let Some(node) = self.embedded_node.as_deref_mut() {
            let _ = node.stop();
        }
        match self.node.as_deref_mut() {
            Some(node) => node.stop(),
            None => Ok(()),
        }
    }

    fn counters(&mut self) -> Result<Counters> {
        if !self.raw_path() {
            return Err(Status::NOT_SUPPORTED);
        }
        Ok(self.pump.counters())
    }

    /// The ISP whose link from this receiver is on, by its media device driver name
    /// ([`route::feeding`]), read from the graphs at each call. None when the receiver is wired
    /// to no ISP, or to some and every link is off.
    fn feeds(&mut self, into: &mut [u8]) -> Result<usize> {
        let links = Rkcif::feeds(self)?;
        let name = route::feeding(&links).map_or(&[][..], str::as_bytes);
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }

    /// Not answered: the vendor kernel's `rkcif` (rockchip-linux/kernel, develop-5.10,
    /// `drivers/media/platform/rockchip/cif/capture.c`, read to character 300000 of 504477) raises no
    /// `V4L2_EVENT_FRAME_SYNC` and has no `v4l2_event_queue` on frame starts. Its CSI start
    /// interrupt and its DVP SOF subdev are internal to the driver, and nothing reaches user
    /// space as an event or a control, so the frame-start count has no observable source here.
    fn frame_starts(&mut self) -> Result<Option<FrameStarts>> {
        Err(Status::NOT_SUPPORTED)
    }

    /// The name of the stream the embedded lines go to, `embedded`, when the board declares it on
    /// the device and names a metadata node for the lines (`config.embedded`). The vendor
    /// kernel's own `rkcif` carries embedded data as a pixel format on the image node
    /// (`V4L2_PIX_FMT_EBD8`) and exposes no metadata node, so on that kernel no node can be named
    /// and this answers 0; a kernel that exposes one serves it. 0 otherwise.
    fn embedded_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        if self.embedded.is_none() || self.embedded_node.is_none() {
            return Ok(0);
        }
        let name = EMBEDDED.as_bytes();
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }

    /// The sensor the graph has upstream of the configured channel's capture node. Unknown without
    /// a link configured, or when the graph has no sensor on that path.
    fn source(&mut self, into: &mut [u8]) -> Result<usize> {
        let link = self.link.ok_or(Status::NOT_SUPPORTED)?;
        let topology = self.hw.topology()?;
        let name = route::sensor(&topology, u32::from(link.virtual_channel))
            .ok_or(Status::NOT_SUPPORTED)?;
        let name = name.as_bytes();
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }

    fn layout(&mut self) -> Result<Option<FrameLayout>> {
        self.link
            .map(|link| layout(&link, self.packing))
            .transpose()
    }
}
