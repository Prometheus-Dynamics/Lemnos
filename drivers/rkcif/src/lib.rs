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
//! Not served: `frame_stream` and `counters` answer `NOT_SUPPORTED`. Raw frames from the capture
//! node would need a way for a driver to fill a frame stream's buffer, which the stream endpoint
//! doesn't have yet; the vendor driver's counts aren't readable through V4L2. `Unpacked16` needs the
//! vendor driver's private memory-mode call, which `lemnos-v4l2` doesn't carry, so it is refused.
//!
//! Failures. While routed, every second the driver reads the receiver's graph without allocating
//! and checks the channel's link is still on. A link someone turned off, or a graph that can't be
//! read, starts recovery: the next ticks open the media device again and route the channel again,
//! with the delay between attempts doubling from 250 ms and capped at 8 s, and clearing when one
//! succeeds. The driver never gives up and never turns itself off; `stop` is what ends it. While
//! recovering, health is degraded and `start` is refused with `BUSY`. A failed `start` is reported
//! to its caller, who retries it.
//!
//! Untested on board: written from the vendor driver's source; the check is
//! `testing/board-runs/E8.md`.

mod hw;
pub mod route;

#[cfg(test)]
mod tests;

use lemnos_contract_camera_receiver::{
    CameraReceiver, Counters, FrameLayout, Limits, LinkConfig, Packing,
};
use lemnos_core::{Config, Description, Health, Item, Lifecycle, MethodId, Result, Status, driver};

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
    hw: Box<dyn Hardware>,
    link: Option<LinkConfig>,
    state: State,
}

/// How a compact raw frame is laid out: lines of `width * depth / 8` bytes, rounded down as the
/// vendor driver does, then up to [`COMPACT_LINE_ALIGN`].
fn layout(link: &LinkConfig) -> Result<FrameLayout> {
    if !matches!(link.bit_depth, 8 | 10 | 12) {
        return Err(Status::INVALID_ARGUMENT);
    }
    let line = u64::from(link.width) * u64::from(link.bit_depth) / 8;
    let stride = line.div_ceil(COMPACT_LINE_ALIGN) * COMPACT_LINE_ALIGN;
    let size = stride * u64::from(link.height);
    Ok(FrameLayout {
        width: link.width,
        height: link.height,
        bit_depth: link.bit_depth,
        packing: Packing::Packed,
        stride: u32::try_from(stride).map_err(|_| Status::INVALID_ARGUMENT)?,
        size: u32::try_from(size).map_err(|_| Status::INVALID_ARGUMENT)?,
    })
}

impl Lifecycle for Rkcif {
    type Needs = RkcifNeeds;
    const TICKS: bool = true;

    fn start(_needs: RkcifNeeds, cfg: &Config<'_>) -> Result<Self> {
        let settings = Settings::new(cfg.str("device"))?;
        Ok(Self::with_hardware(Box::new(LinuxHardware::new(settings))))
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
                    return self.tick(now_ns);
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
                match self.route() {
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
            hw,
            link: None,
            state: State::Stopped,
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
        if packing != Packing::Packed
            || link.lanes == 0
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
        let layout = layout(&link)?;
        self.link = Some(link);
        Ok(layout)
    }

    fn link(&mut self) -> Result<Option<LinkConfig>> {
        Ok(self.link)
    }

    fn frame_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn start(&mut self) -> Result<()> {
        if self.link.is_none() {
            return Err(Status::INVALID_ARGUMENT);
        }
        if self.state != State::Stopped {
            return Err(Status::BUSY);
        }
        self.route()?;
        self.state = State::Routed { next_check: None };
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // The route stays as it is: the ISP may be streaming through it, and only its own driver
        // stops that. Recovery is over either way.
        self.state = State::Stopped;
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters> {
        Err(Status::NOT_SUPPORTED)
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
}
