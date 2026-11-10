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
//! - [`Rga`]: the driver, providing `image.scaler/1`, `image.convert/1` and `image.blend/1`, its
//!   jobs run by [`jobs::Jobs`] on the CPU route.
//!
//! No vendor library: nothing here links or loads one, and the block's work is the caller's.
//!
//! # The driver and its job streams
//!
//! [`Rga`] answers what the block takes (sizes, formats, conversions) and counts its jobs, but
//! answers `job_stream` with `NOT_SUPPORTED`. A job's input is a buffer of the job stream that
//! the consumer filled, and the producer's end of a stream (`StreamEndpoint`) gives a driver no
//! way to read a buffer, nor its dma-buf for a unit to read: it can only publish, take returned
//! buffers, read jobs, and write into buffers it holds. Until it can, the job path is
//! [`jobs::Jobs`], which takes the buffers' bytes from whoever holds them; its tests run it over
//! the frame stream's ring with the buffers in the test's hands.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `max_width`, `max_height`: the largest image in a job, 1 to 65535, default 8192.
//! - `matrix`: `bt601-limited` (default), `bt601-full` or `bt709-limited`, for conversions
//!   between RGB and YUV.
//! - `sampling`: the bilinear phase, `centres` (default), `corners` or `origin`.
//!
//! The driver opens no device and keeps no state that can fail, so it has nothing to retry.
//!
//! # Status
//!
//! Untested on board: the mainline route is tested on a played-back node (and on the kernel's
//! `vim2m` where a test run turns it on), the `/dev/rga` route by the requests it builds, and
//! the CPU route against a reference written per pixel from its definitions, on x86-64 and under
//! qemu's riscv64 with V at VLEN 128, 256 and 512. The board checks are
//! `testing/board-runs/E2.md`.
//!
//! Provenance: ported from the dev branch (`crates/lemnos-drivers-rga`).

#![forbid(unsafe_code)]

pub mod caps;
pub mod cpu;
mod format;
pub mod jobs;
mod multirga;
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
use lemnos_core::{Config, Lifecycle, Result, Status, driver};

use cpu::{Matrix, Sampling};
use jobs::{Jobs, Settings};

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
    jobs: Jobs,
}

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

    fn start(_needs: RgaNeeds, cfg: &Config<'_>) -> Result<Self> {
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
        Ok(Self::new(settings))
    }
}

impl Rga {
    /// The driver with `settings`, for a test or a host that builds it directly.
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self {
            jobs: Jobs::new(settings),
        }
    }

    /// The jobs this block runs, and their counters.
    pub fn jobs(&mut self) -> &mut Jobs {
        &mut self.jobs
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
        let s = self.jobs.settings();
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

    fn job_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        // See the crate docs: the stream endpoint can't hand a driver a job's input.
        Err(Status::NOT_SUPPORTED)
    }

    fn counters(&mut self) -> Result<scaler::Counters> {
        let c = self.jobs.scale_counts();
        Ok(scaler::Counters {
            jobs: c.jobs,
            errors: c.errors,
        })
    }
}

impl ImageConvert for Rga {
    fn info(&mut self) -> Result<ConvertInfo> {
        let s = self.jobs.settings();
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

    fn job_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn counters(&mut self) -> Result<convert::Counters> {
        let c = self.jobs.convert_counts();
        Ok(convert::Counters {
            jobs: c.jobs,
            errors: c.errors,
        })
    }
}

impl ImageBlend for Rga {
    fn info(&mut self) -> Result<BlendInfo> {
        let s = self.jobs.settings();
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

    fn job_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn counters(&mut self) -> Result<blend::Counters> {
        let c = self.jobs.blend_counts();
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
