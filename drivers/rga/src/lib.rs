//! A 2D image block (Rockchip's RGA, and the routes around it): scaling, cropping, pixel-format
//! conversion, flips and quarter turns, fills, masks, blends and colour keys, on the hardware.
//!
//! - [`caps`]: which unit each chip has, by which route, and what it does with each filter;
//!   [`caps::route_for`] answers which route serves an operation, and [`route_on_system`] also
//!   checks that the device node is there. `None` means no hardware route does it.
//! - [`M2mRga`] (Linux): the mainline `rockchip-rga` V4L2 mem2mem driver, through
//!   `lemnos-driver-m2m-decode`'s session. Convert, scale (area downscale, bilinear upscale),
//!   crop, flips and turns, between byte buffers or dma-bufs.
//! - [`MultiRga`] (Linux): the vendor kernel's `/dev/rga` node, through
//!   `lemnos-driver-rga-sys`. Fill, mask ROP, blend, colour key, gray input, channel to gray,
//!   flips and turns, between dma-bufs only, each checked to hold its image.
//! - [`Rga`]: the driver, providing `image.scaler/1`, `image.convert/1` and `image.blend/1`,
//!   serving their job streams.
//!
//! No vendor library: nothing here links or loads one, and the block's work is the caller's.
//!
//! # A job runs on the hardware, or fails
//!
//! Lemnos gives the caller the hardware; it doesn't do the hardware's work itself. A job runs on
//! the hardware route [`caps::route_for`] picks for it on the configured chip, or it completes
//! as failed with its reason in [`Rga::last_failure`] and its contract's `errors` counted.
//! Nothing is ever computed in software, and a job is never sent to another route because its
//! own failed. The reasons, and how a failed unit is retried, are in [`serve`].
//!
//! # The driver and its job streams
//!
//! Each contract's jobs come on its own stream, the optional needs `scale`, `convert` and
//! `blend` in the board file (`[devices.streams.scale]` and so on, `job_bytes` at least the
//! contract's `JOB_BYTES`); `job_stream` answers the need's name, and `NOT_SUPPORTED` for a stream
//! the board doesn't give. The driver ticks every millisecond while it has a stream, and takes up
//! to [`serve::MAX_JOBS_PER_ROUND`] jobs from each per tick. A job names buffers of its stream;
//! its completion is its output buffer published with the job's identity and the bytes written,
//! 0 for a job that failed. How buffers are held, everything a job is checked for, and what
//! happens when a unit fails: [`serve`].
//!
//! | contract | route `caps` chooses | stream with dma-buf handles | stream without |
//! |---|---|---|---|
//! | `image.scaler/1` | mainline (area downscale, bilinear upscale) | the dma-bufs, or bytes if the device refuses them | bytes, copied through the driver to the device |
//! | `image.convert/1` | mainline (no gray) | the dma-bufs, or bytes if refused | bytes, copied |
//! | `image.convert/1` | `/dev/rga` (gray to `AR24`/`XR24`, both packed) | the dma-bufs | fails: `NoHandles` |
//! | `image.blend/1` | `/dev/rga` (one stride for all three, whole pixels) | the dma-bufs: the background copied into the output, then the foreground blended onto it | fails: `NoHandles` |
//! | any | none | fails: `NoRoute` | fails: `NoRoute` |
//!
//! What each contract lists (`format`, `conversion`) is what the chip's routes take, not every
//! format this crate knows: a format or pair is listed when [`caps::route_for`] has a route for
//! it at [`LISTED_SIZE`] by [`LISTED_SIZE`] (a scale: halved with the area filter or doubled with
//! the bilinear one). A listed job of another size can still have no route (the units' smallest
//! frames and largest factors are in [`caps`]).
//!
//! The mainline device is told the formats only: where a conversion meets YUV, its matrix and
//! range are the device's own. The `/dev/rga` gray input is full-range BT.601, which repeats the
//! byte.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `soc` (required): `rk3566`, `rk3568`, `rk3576`, `rk3588`, `rk3562` or `rv1126b`, whose units
//!   [`caps`] lists. Without it the driver has no route for anything, so binding fails with
//!   `INVALID_ARGUMENT`, as other drivers refuse a missing required key. `k230d` names a chip
//!   whose units have no route here yet (see [`caps`]), so binding to it fails with
//!   `NOT_SUPPORTED`. A unit's node is opened when a job first wants it; a node that isn't there
//!   is looked for again after the same waits as a unit that failed.
//! - `max_width`, `max_height`: the largest image in a job, 1 to 65535, default 8192.
//! - `matrix` and `sampling` are refused (`INVALID_ARGUMENT`): they chose the CPU route's
//!   conversion matrix and bilinear phase, and the hardware uses its own. Ignoring them would
//!   give a board file that asks for one something else without saying so.
//!
//! Health: `Degraded` while a unit that failed a job is closed, waiting to be opened again; while
//! a unit's device node isn't there; and once a job failed because its stream has no dma-bufs
//! (the board file has to give that stream `memory = "dma-buf"`). Each says why.
//!
//! # Status
//!
//! Untested on board: the mainline route is tested on a played-back node (and on the kernel's
//! `vim2m` where a test run turns it on), the `/dev/rga` route by the requests it builds. The job
//! streams are tested through the host's own stream, with sealed memfds standing in for dma-bufs
//! and the played-back node and a request-recording stand-in for the units. The board checks are
//! `testing/board-runs/E2.md`.
//!
//! Provenance: ported from the dev branch (`crates/lemnos-drivers-rga`).

#![forbid(unsafe_code)]

pub mod caps;
mod format;
pub mod jobs;
mod multirga;
pub mod serve;
// The shared checks serve the Linux routes only.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod resize;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod transform;
#[cfg(target_os = "linux")]
mod v4l2;

pub use format::{Layout, PixelFormat, Rect};
#[cfg(target_os = "linux")]
pub use multirga::MultiRga;
pub use multirga::{BlendMode, Channel, Image, RgaFormat, Rop};
pub use resize::Filter;
pub use transform::Transform;
#[cfg(target_os = "linux")]
pub use v4l2::{M2mRga, MAX_SESSIONS, find_m2m_device};

use lemnos_contract_image_blend::{self as blend, BlendInfo, ImageBlend};
use lemnos_contract_image_convert::{self as convert, ConversionPair, ConvertInfo, ImageConvert};
use lemnos_contract_image_scaler::{self as scaler, ImageScaler, ScalerInfo};
use lemnos_core::{
    Config, ConfigError, Health, Key, Kind, Lifecycle, Result, Status, StreamHandle, driver,
};

use jobs::{Contract, Jobs, Settings};
use serve::{Failure, Lane, Routes, Server};

/// The route that serves `op` from `src` to `dst` on `soc` with the devices this system has:
/// [`caps::route_for`], and each route only when its device node exists. `None` means no
/// hardware here does it.
#[cfg(target_os = "linux")]
pub fn route_on_system(
    soc: caps::Soc,
    op: caps::Op,
    src: Layout,
    dst: Layout,
) -> Option<caps::Route> {
    match caps::route_for(soc, op, src, dst)? {
        caps::Route::V4l2 => find_m2m_device("rockchip-rga").map(|_| caps::Route::V4l2),
        caps::Route::MultiRga => MultiRga::find().map(|_| caps::Route::MultiRga),
        _ => None,
    }
}

/// [`caps::route_for`] on a system without the Linux routes: none.
#[cfg(not(target_os = "linux"))]
pub fn route_on_system(
    _soc: caps::Soc,
    _op: caps::Op,
    _src: Layout,
    _dst: Layout,
) -> Option<caps::Route> {
    None
}

/// The formats a blend takes: premultiplied B, G, R, A.
const BLEND_FORMATS: [PixelFormat; 1] = [PixelFormat::Abgr32];

/// The side of the frame the contracts' format lists are asked of [`caps::route_for`] at.
pub const LISTED_SIZE: u32 = 256;

/// What health says while a unit that failed a job is closed.
pub const UNIT_FAILED: &str =
    "a 2D unit failed a job: it is closed and opened again after a wait; its jobs fail meanwhile";

/// What health says while a unit's device node isn't there.
pub const UNIT_MISSING: &str =
    "a 2D unit's device node is not there: looked for again after a wait; its jobs fail meanwhile";

/// What health says once a job failed because its stream has no dma-bufs.
pub const NO_HANDLES: &str = "a job's route takes dma-bufs only and its stream has none: give \
                              that stream memory = \"dma-buf\" in the board file";

/// Whether `soc` has a route for a scale of `format`: halved with the area filter, or doubled
/// with the bilinear one.
fn scales(soc: caps::Soc, format: PixelFormat) -> bool {
    let (big, small) = (LISTED_SIZE, LISTED_SIZE / 2);
    let at = |side| Layout::packed(format, side, side);
    caps::route_for(soc, caps::Op::Resize(Filter::Area), at(big), at(small)).is_some()
        || caps::route_for(soc, caps::Op::Resize(Filter::Bilinear), at(small), at(big)).is_some()
}

/// Whether `soc` has a route for a conversion from `from` to `to`.
fn converts(soc: caps::Soc, from: PixelFormat, to: PixelFormat) -> bool {
    let at = |format| Layout::packed(format, LISTED_SIZE, LISTED_SIZE);
    caps::route_for(soc, caps::Op::Convert, at(from), at(to)).is_some()
}

/// Whether `soc` has a route for a blend of `format`.
fn blends(soc: caps::Soc, format: PixelFormat) -> bool {
    let at = Layout::packed(format, LISTED_SIZE, LISTED_SIZE);
    caps::route_for(soc, caps::Op::Fixed(caps::FixedOp::Blend), at, at).is_some()
}

/// Every pair of formats, `from` major.
fn pairs() -> impl Iterator<Item = (PixelFormat, PixelFormat)> {
    PixelFormat::ALL
        .into_iter()
        .flat_map(|from| PixelFormat::ALL.into_iter().map(move |to| (from, to)))
}

/// How many of `items` there are, as a contract's count.
fn count<T>(items: impl Iterator<Item = T>) -> u16 {
    u16::try_from(items.count()).unwrap_or(u16::MAX)
}

/// Entry `index` of `items`, or `NOT_FOUND`.
fn nth<T>(mut items: impl Iterator<Item = T>, index: u16) -> Result<T> {
    items.nth(usize::from(index)).ok_or(Status::NOT_FOUND)
}

/// The 2D block as the image contracts.
#[driver("rga", provides = [ImageScaler, ImageConvert, ImageBlend])]
pub struct Rga {
    /// `image.scaler/1` jobs.
    #[needs(optional, job)]
    scale: Option<StreamHandle>,
    /// `image.convert/1` jobs.
    #[needs(optional, job)]
    convert: Option<StreamHandle>,
    /// `image.blend/1` jobs.
    #[needs(optional, job)]
    blend: Option<StreamHandle>,
    soc: caps::Soc,
    lanes: [Lane; 3],
    server: Server,
}

/// The `soc` names of the config.
const SOCS: [(&str, caps::Soc); 7] = [
    ("rk3566", caps::Soc::Rk3566),
    ("rk3568", caps::Soc::Rk3568),
    ("rk3576", caps::Soc::Rk3576),
    ("rk3588", caps::Soc::Rk3588),
    ("rk3562", caps::Soc::Rk3562),
    ("rv1126b", caps::Soc::Rv1126b),
    ("k230d", caps::Soc::K230d),
];

/// The settings a board file gives the block.
const KEYS: &[Key] = &[
    Key::required("soc", Kind::Str),
    Key::optional("max_width", Kind::Int),
    Key::optional("max_height", Kind::Int),
    // The CPU route's settings, refused below: the hardware has its own.
    Key::optional("matrix", Kind::Str),
    Key::optional("sampling", Kind::Str),
];

/// The chip `name` names, or `None` when it is none of [`SOCS`].
fn soc_named(name: &str) -> Option<caps::Soc> {
    SOCS.iter().find(|(n, _)| *n == name).map(|&(_, soc)| soc)
}

/// The chip `name` names: `INVALID_ARGUMENT` when there is no name or it is none of [`SOCS`]. Its
/// route is checked by `check_config`.
fn soc(name: Option<&str>) -> Result<caps::Soc> {
    name.and_then(soc_named).ok_or(Status::INVALID_ARGUMENT)
}

impl Lifecycle for Rga {
    type Needs = RgaNeeds;
    const TICKS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        for key in ["max_width", "max_height"] {
            if let Some(side) = cfg.int(key)
                && !u32::try_from(side).is_ok_and(|side| (1..=65535).contains(&side))
            {
                return Err(ConfigError::new(key, "must be 1 to 65535"));
            }
        }
        // The CPU route's settings: the hardware has its own, so naming one is refused.
        for key in ["matrix", "sampling"] {
            if cfg.get(key).is_some() {
                return Err(ConfigError::new(
                    key,
                    "is the CPU route's setting: the hardware has its own",
                ));
            }
        }
        if let Some(name) = cfg.str("soc") {
            let Some(soc) = soc_named(name) else {
                return Err(ConfigError::new(
                    "soc",
                    "must be rk3566, rk3568, rk3576, rk3588, rk3562, rv1126b or k230d",
                ));
            };
            if !caps::caps(soc).any(|c| c.implemented) {
                return Err(ConfigError::new("soc", "has no implemented route here"));
            }
        }
        Ok(())
    }

    fn start(needs: RgaNeeds, cfg: &Config<'_>) -> Result<Self> {
        let side = |key| match cfg.int(key) {
            None => Ok(8192),
            Some(v) => u32::try_from(v).map_err(|_| Status::INVALID_ARGUMENT),
        };
        let settings = Settings {
            max_width: side("max_width")?,
            max_height: side("max_height")?,
        };
        let soc = soc(cfg.str("soc"))?;
        Ok(Self::serving(needs, settings, soc, Routes::on_system(soc)))
    }

    fn health(&self) -> Health {
        let routes = self.server.routes();
        if routes.failing() {
            Health::Degraded(UNIT_FAILED)
        } else if routes.missing() {
            Health::Degraded(UNIT_MISSING)
        } else if self.server.needs_handles() {
            Health::Degraded(NO_HANDLES)
        } else {
            Health::Ok
        }
    }

    /// Runs the jobs waiting on each stream. Again in a millisecond, or at once when a stream
    /// had more than one round's jobs.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let mut any = false;
        let mut more = false;
        let streams = [
            (Contract::Scale, &mut self.scale),
            (Contract::Convert, &mut self.convert),
            (Contract::Blend, &mut self.blend),
        ];
        for ((contract, stream), lane) in streams.into_iter().zip(&mut self.lanes) {
            let Some(stream) = stream.as_deref_mut() else {
                continue;
            };
            any = true;
            // A stream's error leaves its jobs where they are, for the next tick.
            if let Ok(read) = self.server.serve(contract, stream, lane, now_ns) {
                more |= read >= serve::MAX_JOBS_PER_ROUND;
            }
        }
        any.then(|| {
            if more {
                now_ns
            } else {
                now_ns.saturating_add(serve::POLL_NS)
            }
        })
    }
}

/// The name a provided stream is opened by: its need's.
fn stream_name(stream: Option<&StreamHandle>, name: &str, into: &mut [u8]) -> Result<usize> {
    if stream.is_none() {
        return Err(Status::NOT_SUPPORTED);
    }
    into.get_mut(..name.len())
        .ok_or(Status::BUFFER_TOO_SMALL)?
        .copy_from_slice(name.as_bytes());
    Ok(name.len())
}

impl Rga {
    /// The driver for `soc` on `needs`' streams, running jobs on `routes`.
    fn serving(needs: RgaNeeds, settings: Settings, soc: caps::Soc, routes: Routes) -> Self {
        Self {
            scale: needs.scale,
            convert: needs.convert,
            blend: needs.blend,
            soc,
            lanes: Default::default(),
            server: Server::new(Jobs::new(settings), routes),
        }
    }

    /// The jobs this block runs, and their counters.
    pub fn jobs(&mut self) -> &mut Jobs {
        &mut self.server.jobs
    }

    /// Why the last job that failed on a stream failed.
    pub fn last_failure(&self) -> Option<Failure> {
        self.server.last_failure()
    }
}

impl ImageScaler for Rga {
    fn info(&mut self) -> Result<ScalerInfo> {
        let s = self.server.jobs.settings();
        Ok(ScalerInfo {
            max_width: s.max_width,
            max_height: s.max_height,
        })
    }

    fn format_count(&mut self) -> Result<u16> {
        let soc = self.soc;
        Ok(count(
            PixelFormat::ALL.into_iter().filter(|&f| scales(soc, f)),
        ))
    }

    fn format(&mut self, index: u16) -> Result<u32> {
        let soc = self.soc;
        let listed = PixelFormat::ALL.into_iter().filter(|&f| scales(soc, f));
        nth(listed, index).map(PixelFormat::fourcc)
    }

    fn job_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        stream_name(self.scale.as_ref(), "scale", into)
    }

    fn counters(&mut self) -> Result<scaler::Counters> {
        let c = self.server.jobs.scale_counts();
        Ok(scaler::Counters {
            jobs: c.jobs,
            errors: c.errors,
        })
    }
}

impl ImageConvert for Rga {
    fn info(&mut self) -> Result<ConvertInfo> {
        let s = self.server.jobs.settings();
        Ok(ConvertInfo {
            max_width: s.max_width,
            max_height: s.max_height,
        })
    }

    fn conversion_count(&mut self) -> Result<u16> {
        let soc = self.soc;
        Ok(count(pairs().filter(|&(from, to)| converts(soc, from, to))))
    }

    fn conversion(&mut self, index: u16) -> Result<ConversionPair> {
        let soc = self.soc;
        let listed = pairs().filter(|&(from, to)| converts(soc, from, to));
        nth(listed, index).map(|(from, to)| ConversionPair {
            from: from.fourcc(),
            to: to.fourcc(),
        })
    }

    fn job_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        stream_name(self.convert.as_ref(), "convert", into)
    }

    fn counters(&mut self) -> Result<convert::Counters> {
        let c = self.server.jobs.convert_counts();
        Ok(convert::Counters {
            jobs: c.jobs,
            errors: c.errors,
        })
    }
}

impl ImageBlend for Rga {
    fn info(&mut self) -> Result<BlendInfo> {
        let s = self.server.jobs.settings();
        Ok(BlendInfo {
            max_width: s.max_width,
            max_height: s.max_height,
        })
    }

    fn format_count(&mut self) -> Result<u16> {
        let soc = self.soc;
        Ok(count(BLEND_FORMATS.into_iter().filter(|&f| blends(soc, f))))
    }

    fn format(&mut self, index: u16) -> Result<u32> {
        let soc = self.soc;
        let listed = BLEND_FORMATS.into_iter().filter(|&f| blends(soc, f));
        nth(listed, index).map(PixelFormat::fourcc)
    }

    fn job_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        stream_name(self.blend.as_ref(), "blend", into)
    }

    fn counters(&mut self) -> Result<blend::Counters> {
        let c = self.server.jobs.blend_counts();
        Ok(blend::Counters {
            jobs: c.jobs,
            errors: c.errors,
        })
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
