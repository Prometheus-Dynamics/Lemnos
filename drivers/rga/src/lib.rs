//! A 2D image block (Rockchip's RGA, and the routes around it): scaling, cropping, pixel-format
//! conversion, flips and quarter turns, fills, masks, blends and colour keys, with a CPU route for
//! what no unit serves.
//!
//! - [`caps`]: which unit each chip has, by which route, and what it does with each filter;
//!   [`caps::route_for`] answers which route serves an operation, and [`route_on_system`] also
//!   checks that the device node is there. `None` means the CPU.
//! - [`M2mRga`] (Linux): the mainline `rockchip-rga` V4L2 mem2mem driver, through
//!   `lemnos-driver-m2m-decode`'s session. Convert, scale (area downscale, bilinear upscale),
//!   crop, flips and turns, between byte buffers or dma-bufs.
//! - [`MultiRga`] (Linux): the vendor kernel's `/dev/rga` node, through
//!   `lemnos-driver-rga-sys`. Fill, mask ROP, blend, colour key, gray input, channel to gray,
//!   flips and turns, between dma-bufs only, each checked to hold its image.
//! - [`cpu`]: every conversion between the [`PixelFormat`]s, area and bilinear resizes, blends,
//!   and the per-pixel operations, as scalar loops with RISC-V vector rows on riscv64 Linux.
//! - [`Rga`]: the driver, providing `image.scaler/1`, `image.convert/1` and `image.blend/1`,
//!   serving their job streams.
//!
//! No vendor library: nothing here links or loads one, and the block's work is the caller's.
//!
//! # The driver and its job streams
//!
//! Each contract's jobs come on its own stream, the optional needs `scale`, `convert` and
//! `blend` in the board file (`[devices.streams.scale]` and so on, `job_bytes` at least the
//! contract's `JOB_BYTES`); `job_stream` answers the need's name, and `NOT_SUPPORTED` for a stream
//! the board doesn't give. The driver ticks every millisecond while it has a stream, and takes up
//! to [`serve::MAX_JOBS_PER_ROUND`] jobs from each per tick. A job names buffers of its stream;
//! its completion is its output buffer published with the job's identity and the bytes written,
//! 0 for a job refused. How buffers are held, everything a job is checked for, and what happens
//! when a unit fails: [`serve`].
//!
//! Each job runs where [`caps::route_for`] sends it for the board's `soc`, when that unit's device
//! is there and the stream can give it what it takes; otherwise on the CPU:
//!
//! | contract | route `caps` chooses | stream with dma-buf handles | stream without |
//! |---|---|---|---|
//! | `image.scaler/1` | mainline (area downscale, bilinear upscale) | the dma-bufs, or bytes if the device refuses them | bytes, copied through the driver |
//! | `image.convert/1` | mainline (no gray) | the dma-bufs, or bytes if refused | bytes, copied |
//! | `image.convert/1` | `/dev/rga` (gray to `AR24`/`XR24`, both packed) | the dma-bufs | CPU |
//! | `image.blend/1` | `/dev/rga` (one stride for all three, whole pixels) | the dma-bufs: the background copied into the output, then the foreground blended onto it | CPU |
//! | any | none | CPU | CPU |
//!
//! The mainline device is told the formats only, not `matrix`: where a conversion meets YUV, its
//! matrix is the device's own, and how close it comes to the CPU's is unmeasured (see [`caps`]).
//! The `/dev/rga` gray input is full-range BT.601, which repeats the byte as the CPU does.
//!
//! The CPU route reads each input once from the stream into the driver's memory and works only
//! on that copy, since the consumer shares the buffers and can change them at any time; the
//! result is written back with one copy. The driver's memory for this grows to the largest
//! images it has held, which a stream's buffer bounds, and is kept, so jobs of sizes seen
//! before allocate nothing.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `max_width`, `max_height`: the largest image in a job, 1 to 65535, default 8192.
//! - `matrix`: `bt601-limited` (default), `bt601-full` or `bt709-limited`, for conversions
//!   between RGB and YUV.
//! - `sampling`: the bilinear phase, `centres` (default), `corners` or `origin`.
//! - `soc`: `rk3566`, `rk3568`, `rk3576`, `rk3588`, `rk3562`, `rv1126b` or `k230d`, whose units
//!   [`caps`] lists. Without it no device is opened and every job runs on the CPU. With it, a
//!   unit's node is opened when a job first wants it; a node that isn't there is looked for again
//!   after the same waits as a unit that failed.
//!
//! # Status
//!
//! Untested on board: the mainline route is tested on a played-back node (and on the kernel's
//! `vim2m` where a test run turns it on), the `/dev/rga` route by the requests it builds, and
//! the CPU route against a reference written per pixel from its definitions, on x86-64 and under
//! qemu's riscv64 with V at VLEN 128, 256 and 512. The job streams are tested through the host's
//! own stream, with sealed memfds standing in for dma-bufs and the played-back node and a
//! request-recording stand-in for the units. The board checks are `testing/board-runs/E2.md`.
//!
//! Not delivered: the contracts fault the stream of a consumer that posts bytes that are no job,
//! and the producer's end has no way to; such a job is counted as failed and dropped instead. On a
//! stream without handles a consumer that leaves and another that comes between two ticks is not
//! seen, so jobs of the second naming buffers held from the first complete into nothing (the
//! producer's end can't say which consumer a held buffer was taken under unless it has handles).
//!
//! Provenance: ported from the dev branch (`crates/lemnos-drivers-rga`).

#![forbid(unsafe_code)]

pub mod caps;
pub mod cpu;
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
use lemnos_core::{Config, Health, Lifecycle, Result, Status, StreamHandle, driver};

use cpu::{Matrix, Sampling};
use jobs::{Contract, Jobs, Settings};
use serve::{Failure, Lane, Routes, Server};

/// The route that serves `op` from `src` to `dst` on `soc` with the devices this system has:
/// [`caps::route_for`], and the V4L2 route only when the `rockchip-rga` node exists. `None`
/// means the work goes to the CPU.
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

/// The 2D block as the image contracts.
#[driver("rga", provides = [ImageScaler, ImageConvert, ImageBlend])]
pub struct Rga {
    /// `image.scaler/1` jobs.
    #[needs(optional)]
    scale: Option<StreamHandle>,
    /// `image.convert/1` jobs.
    #[needs(optional)]
    convert: Option<StreamHandle>,
    /// `image.blend/1` jobs.
    #[needs(optional)]
    blend: Option<StreamHandle>,
    lanes: [Lane; 3],
    server: Server,
}

/// The `soc` names of the config.
const SOCS: [(&str, Option<caps::Soc>); 7] = [
    ("rk3566", Some(caps::Soc::Rk3566)),
    ("rk3568", Some(caps::Soc::Rk3568)),
    ("rk3576", Some(caps::Soc::Rk3576)),
    ("rk3588", Some(caps::Soc::Rk3588)),
    ("rk3562", Some(caps::Soc::Rk3562)),
    ("rv1126b", Some(caps::Soc::Rv1126b)),
    ("k230d", Some(caps::Soc::K230d)),
];

/// `INVALID_ARGUMENT` unless `name` is one of `names`.
fn choose<T: Copy>(name: Option<&str>, names: &[(&str, T)], default: T) -> Result<T> {
    match name {
        None => Ok(default),
        Some(name) => names
            .iter()
            .find(|(n, _)| *n == name)
            .map(|&(_, v)| v)
            .ok_or(Status::INVALID_ARGUMENT),
    }
}

impl Lifecycle for Rga {
    type Needs = RgaNeeds;
    const TICKS: bool = true;

    fn start(needs: RgaNeeds, cfg: &Config<'_>) -> Result<Self> {
        let side = |key| match cfg.int(key) {
            None => Ok(8192),
            Some(v) => u32::try_from(v)
                .ok()
                .filter(|v| (1..=65535).contains(v))
                .ok_or(Status::INVALID_ARGUMENT),
        };
        let settings = Settings {
            max_width: side("max_width")?,
            max_height: side("max_height")?,
            matrix: choose(
                cfg.str("matrix"),
                &[
                    ("bt601-limited", Matrix::Bt601Limited),
                    ("bt601-full", Matrix::Bt601Full),
                    ("bt709-limited", Matrix::Bt709Limited),
                ],
                Matrix::Bt601Limited,
            )?,
            sampling: choose(
                cfg.str("sampling"),
                &[
                    ("centres", Sampling::Centres),
                    ("corners", Sampling::Corners),
                    ("origin", Sampling::Origin),
                ],
                Sampling::Centres,
            )?,
        };
        let soc = choose(cfg.str("soc"), &SOCS, None)?;
        Ok(Self::serving(needs, settings, Routes::on_system(soc)))
    }

    fn health(&self) -> Health {
        if self.server.routes().failing() {
            Health::Degraded("a 2D unit failed: its jobs run on the CPU until it is reopened")
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
    /// The driver with `settings`, no streams and no unit, for a test or a host that builds it
    /// directly.
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        let needs = RgaNeeds {
            scale: None,
            convert: None,
            blend: None,
        };
        Self::serving(needs, settings, Routes::cpu())
    }

    /// The driver on `needs`' streams, running jobs on `routes`.
    fn serving(needs: RgaNeeds, settings: Settings, routes: Routes) -> Self {
        Self {
            scale: needs.scale,
            convert: needs.convert,
            blend: needs.blend,
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

/// Entry `index` of `list`, or `NOT_FOUND`.
fn entry<T: Copy>(list: &[T], index: u16) -> Result<T> {
    list.get(usize::from(index))
        .copied()
        .ok_or(Status::NOT_FOUND)
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
        Ok(PixelFormat::ALL.len() as u16)
    }

    fn format(&mut self, index: u16) -> Result<u32> {
        entry(&PixelFormat::ALL, index).map(PixelFormat::fourcc)
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
        Ok((PixelFormat::ALL.len() * PixelFormat::ALL.len()) as u16)
    }

    fn conversion(&mut self, index: u16) -> Result<ConversionPair> {
        let n = PixelFormat::ALL.len();
        let i = usize::from(index);
        if i >= n * n {
            return Err(Status::NOT_FOUND);
        }
        Ok(ConversionPair {
            from: PixelFormat::ALL[i / n].fourcc(),
            to: PixelFormat::ALL[i % n].fourcc(),
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
        Ok(BLEND_FORMATS.len() as u16)
    }

    fn format(&mut self, index: u16) -> Result<u32> {
        entry(&BLEND_FORMATS, index).map(PixelFormat::fourcc)
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
