//! [`VgliteDriver`]: the GPU as `image.scaler/1` and `image.convert/1`, each contract's jobs
//! served by a [`Lane`] from its own stream.
//!
//! # The board file
//!
//! ```toml
//! [[devices]]
//! id = "gpu2d"
//! driver = "vglite"
//! config = { device = "/dev/vg_lite" }  # the default
//! [devices.streams.scale]               # image.scaler/1 jobs; optional
//! memory = "dma-buf"                    # required: jobs name dma-bufs
//! buffers = 8
//! buffer_bytes = 8294400
//! job_slots = 16
//! job_bytes = 49
//! [devices.streams.convert]             # image.convert/1 jobs; optional
//! memory = "dma-buf"
//! buffers = 8
//! buffer_bytes = 8294400
//! job_slots = 16
//! job_bytes = 48
//! ```
//!
//! - `device`: the GPU's node, [`DEVICE`] when left out; an empty or non-text value is
//!   `INVALID_ARGUMENT`. Nothing is opened at bind: the first job opens it (see [`Vglite`]).
//! - The streams [`SCALE_STREAM`] and [`CONVERT_STREAM`] are each optional, and at least one is
//!   needed (`NOT_FOUND` otherwise: a device with neither has nothing to do). `job_stream`
//!   answers a stream's name, and `NOT_SUPPORTED` for one the board doesn't give. Each stream's
//!   buffers must be dma-bufs; a stream of plain shared memory is refused at bind with
//!   `NOT_SUPPORTED`, so the device shows as failing to bind until the board file says
//!   `memory = "dma-buf"`.
//!
//! # Ticks
//!
//! Each tick runs the jobs waiting on each stream, up to [`MAX_JOBS_PER_ROUND`] each, then asks
//! again after [`POLL_NS`], or at once when a stream had a full round. A job runs to its end
//! inside the tick (the GPU's wait), so a published output is never one the GPU still writes.
//!
//! # Failures
//!
//! The GPU: see [`Vglite`] (closed and reopened, 250 ms doubling to 8 s, never given up); a job
//! that meets it backing off completes as failed. A stream: an error from it is retried on the
//! same backoff, for that stream alone, and health is degraded meanwhile. The driver never
//! turns itself off.
//!
//! # Not provided
//!
//! - `image.blend/1`: its layers are premultiplied, and the GPU's blend here is straight alpha
//!   only (a premultiplied blend is `Unsupported`; see the crate docs).
//! - Crop, flips, quarter turns, fills, palettes and strokes: no contract carries them. They
//!   stay the [`Vglite`] calls for a caller that opens the GPU itself.

use std::path::PathBuf;

use lemnos_contract_image_convert::{self as convert, ConversionPair, ConvertInfo, ImageConvert};
use lemnos_contract_image_scaler::{self as scaler, ImageScaler, ScalerInfo};
use lemnos_core::{Config, Health, Lifecycle, Result, Status, StreamHandle, driver};
use lemnos_hal::blocks::BufferKind;

use crate::gpu::{State, Vglite, backoff_ns};
use crate::serve::{Contract, FORMATS, Failure, Gpu, Lane, MAX_JOBS_PER_ROUND, conversion};
use crate::{DEVICE, MAX_SIDE};

/// The stream `image.scaler/1` jobs come on.
pub const SCALE_STREAM: &str = "scale";

/// The stream `image.convert/1` jobs come on.
pub const CONVERT_STREAM: &str = "convert";

/// How often the streams are looked at.
pub const POLL_NS: u64 = 1_000_000;

/// The GPU, as the driver holds it.
pub type BoxedGpu = Box<dyn Gpu + Send>;

/// A stream's lane, and its own backoff after a stream error.
#[derive(Debug, Default)]
struct Served {
    lane: Lane,
    /// Stream errors in a row.
    failures: u32,
    /// When the stream is tried again after an error.
    retry_at_ns: u64,
}

/// The VGLite GPU as `image.scaler/1` and `image.convert/1`.
#[driver("vglite", provides = [ImageScaler, ImageConvert])]
pub struct VgliteDriver {
    /// `image.scaler/1` jobs.
    #[needs(optional)]
    scale: Option<StreamHandle>,
    /// `image.convert/1` jobs.
    #[needs(optional)]
    convert: Option<StreamHandle>,
    gpu: BoxedGpu,
    lanes: [Served; 2],
}

impl Lifecycle for VgliteDriver {
    type Needs = VgliteDriverNeeds;
    const TICKS: bool = true;

    fn start(needs: VgliteDriverNeeds, cfg: &Config<'_>) -> Result<Self> {
        let path = match (cfg.str("device"), cfg.get("device")) {
            (Some(""), _) => return Err(Status::INVALID_ARGUMENT),
            (Some(path), _) => PathBuf::from(path),
            (None, Some(_)) => return Err(Status::INVALID_ARGUMENT),
            (None, None) => PathBuf::from(DEVICE),
        };
        if needs.scale.is_none() && needs.convert.is_none() {
            return Err(Status::NOT_FOUND);
        }
        // A job names its images by dma-buf: known now, before any consumer comes.
        let plain = [&needs.scale, &needs.convert]
            .into_iter()
            .flatten()
            .any(|stream| stream.buffer_kind() != BufferKind::Handles);
        if plain {
            return Err(Status::NOT_SUPPORTED);
        }
        Ok(Self::with_gpu(needs, Box::new(Vglite::new(path))))
    }

    fn health(&self) -> Health {
        if self.lanes.iter().any(|served| served.failures > 0) {
            return Health::Degraded("a job stream failed: trying it again");
        }
        if matches!(self.gpu.state(), State::BackingOff { .. }) {
            return Health::Degraded("the GPU failed: reopening it");
        }
        Health::Ok
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let mut next = now_ns.saturating_add(POLL_NS);
        let streams = [
            (Contract::Scale, &mut self.scale),
            (Contract::Convert, &mut self.convert),
        ];
        for ((contract, stream), served) in streams.into_iter().zip(&mut self.lanes) {
            let Some(stream) = stream.as_deref_mut() else {
                continue;
            };
            if served.failures > 0 && now_ns < served.retry_at_ns {
                next = next.min(served.retry_at_ns);
                continue;
            }
            match served.lane.serve(contract, stream, &mut *self.gpu, now_ns) {
                Ok(read) => {
                    served.failures = 0;
                    if read >= MAX_JOBS_PER_ROUND {
                        next = now_ns;
                    }
                }
                Err(_) => {
                    served.retry_at_ns = now_ns.saturating_add(backoff_ns(served.failures));
                    served.failures = served.failures.saturating_add(1);
                }
            }
        }
        // A stream backing off is looked at when it is due, unless another wants a tick first.
        let due = self
            .lanes
            .iter()
            .filter(|served| served.failures > 0)
            .map(|served| served.retry_at_ns)
            .min();
        let polled = [self.scale.is_some(), self.convert.is_some()]
            .iter()
            .zip(&self.lanes)
            .any(|(given, served)| *given && served.failures == 0);
        Some(match (polled, due) {
            (true, _) | (false, None) => next,
            (false, Some(due)) => due,
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

impl VgliteDriver {
    /// The driver over `gpu`, for a test or a host that builds it directly. Nothing is opened
    /// here.
    #[must_use]
    pub fn with_gpu(needs: VgliteDriverNeeds, gpu: BoxedGpu) -> Self {
        Self {
            scale: needs.scale,
            convert: needs.convert,
            gpu,
            lanes: Default::default(),
        }
    }

    /// Why the last failed job on the scale (`Contract::Scale`) or convert stream failed.
    pub fn last_failure(&self, contract: Contract) -> Option<Failure> {
        self.lane(contract).last_failure()
    }

    fn lane(&self, contract: Contract) -> &Lane {
        match contract {
            Contract::Scale => &self.lanes[0].lane,
            Contract::Convert => &self.lanes[1].lane,
        }
    }
}

impl ImageScaler for VgliteDriver {
    fn info(&mut self) -> Result<ScalerInfo> {
        Ok(ScalerInfo {
            max_width: MAX_SIDE,
            max_height: MAX_SIDE,
        })
    }

    fn format_count(&mut self) -> Result<u16> {
        Ok(FORMATS.len() as u16)
    }

    fn format(&mut self, index: u16) -> Result<u32> {
        FORMATS
            .get(usize::from(index))
            .map(|(code, _)| *code)
            .ok_or(Status::NOT_FOUND)
    }

    fn job_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        stream_name(self.scale.as_ref(), SCALE_STREAM, into)
    }

    fn counters(&mut self) -> Result<scaler::Counters> {
        let c = self.lane(Contract::Scale).counts();
        Ok(scaler::Counters {
            jobs: c.jobs,
            errors: c.errors,
        })
    }
}

impl ImageConvert for VgliteDriver {
    fn info(&mut self) -> Result<ConvertInfo> {
        Ok(ConvertInfo {
            max_width: MAX_SIDE,
            max_height: MAX_SIDE,
        })
    }

    fn conversion_count(&mut self) -> Result<u16> {
        Ok(crate::serve::conversions())
    }

    fn conversion(&mut self, index: u16) -> Result<ConversionPair> {
        conversion(index)
            .map(|(from, to)| ConversionPair { from, to })
            .ok_or(Status::NOT_FOUND)
    }

    fn job_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        stream_name(self.convert.as_ref(), CONVERT_STREAM, into)
    }

    fn counters(&mut self) -> Result<convert::Counters> {
        let c = self.lane(Contract::Convert).counts();
        Ok(convert::Counters {
            jobs: c.jobs,
            errors: c.errors,
        })
    }
}
