//! [`Nonai2dDriver`]: the engine as `image.convert/1`, its jobs served by [`Converter`] from the
//! device's job stream.
//!
//! # The board file
//!
//! ```toml
//! [[devices]]
//! id = "convert"
//! driver = "nonai2d"
//! config = { device = "/dev/video3" }   # optional: found by its driver name when left out
//! [devices.streams.jobs]
//! memory = "dma-buf"                    # required: the engine takes nothing else
//! buffers = 8
//! buffer_bytes = 8294400
//! job_slots = 16
//! job_bytes = 48
//! ```
//!
//! - `device`: the engine's node. When left out, each open looks for the node whose V4L2
//!   driver is [`DRIVER`](crate::DRIVER) ([`find`](crate::find)); a node that isn't there is a
//!   failed open, retried on the backoff. An empty string is `INVALID_ARGUMENT`. Nothing is
//!   opened at bind: the first job opens it, for that job's layouts (see [`Nonai2d`]).
//! - The stream named [`JOB_STREAM`] carries the jobs and holds the images. Its buffers must be
//!   dma-bufs, since a frame is handed to the engine by descriptor. The driver asks the stream
//!   what its buffers are when it binds (`StreamEndpoint::buffer_kind`), and a stream of plain
//!   shared memory is refused there with `NOT_SUPPORTED`: the device shows as failing to bind,
//!   retried as any failed bind is, until the board file gives the stream `memory = "dma-buf"`.
//!
//! # Ticks
//!
//! Each tick takes the stream's free buffers and runs the jobs waiting, then asks again after
//! [`POLL_NS`]. A conversion runs to its end inside the tick (the session waits for the
//! frame, up to [`CONVERT_TIMEOUT`](crate::CONVERT_TIMEOUT)), so a published output is never
//! one the engine still writes, short of the timeout case the crate docs describe.
//!
//! # Failures
//!
//! The engine: see [`Nonai2d`] (closed and reopened, 250 ms doubling to 8 s, never given up).
//! The stream: an error from it is retried on the same backoff, and health is degraded
//! meanwhile. The driver never turns itself off.
//!
//! # Not provided
//!
//! - `image.scaler/1`: the engine converts at one size only (`nonai_2d_vidioc_try_fmt` gives the
//!   capture side the output's size, and the conversion mode is `K_2D_CALC_MODE_CSC`).
//! - `image.blend/1`: the OSD mode blends from a separate overlay buffer the contract has no
//!   words for; it stays [`Device::draw`](crate::Device::draw) for a caller that opens the node.

use std::io;
use std::path::PathBuf;

use lemnos_contract_image_convert::{
    ConversionPair, ConvertInfo, Counters, ImageConvert, MAX_STREAM_NAME,
};
use lemnos_core::{Config, Health, Lifecycle, Result, Status, StreamHandle, driver};
use lemnos_hal::blocks::BufferKind;

use crate::convert::{CONVERSIONS, Converter, conversion};
use crate::engine::{Engine, Nonai2d, State, backoff_ns};
use crate::layout::{Layout, MAX_HEIGHT, MAX_WIDTH};
use crate::{Device, find};

/// The stream jobs come on: the need's name, and the name `job_stream` answers.
pub const JOB_STREAM: &str = "jobs";

/// How often the stream is looked at.
pub const POLL_NS: u64 = 1_000_000;

/// An engine, as the driver holds it.
pub type BoxedEngine = Box<dyn Engine + Send>;

/// What opens the engine for a source and a destination layout, as the driver holds it.
pub type Opener = Box<dyn FnMut(Layout, Layout) -> io::Result<BoxedEngine> + Send>;

/// The non-AI 2D engine as `image.convert/1`.
#[driver("nonai2d", provides = [ImageConvert])]
pub struct Nonai2dDriver {
    #[needs(job)]
    jobs: StreamHandle,
    converter: Converter<BoxedEngine, Opener>,
    /// Stream errors in a row.
    stream_failures: u32,
    /// When the stream is tried again after an error.
    retry_at_ns: u64,
}

impl Lifecycle for Nonai2dDriver {
    type Needs = Nonai2dDriverNeeds;
    const TICKS: bool = true;

    fn start(needs: Nonai2dDriverNeeds, cfg: &Config<'_>) -> Result<Self> {
        let path = match cfg.str("device") {
            Some("") => return Err(Status::INVALID_ARGUMENT),
            Some(path) => Some(PathBuf::from(path)),
            None if cfg.get("device").is_some() => return Err(Status::INVALID_ARGUMENT),
            None => None,
        };
        // The engine takes a frame only as a dma-buf: known now, before any consumer comes.
        if needs.jobs.buffer_kind() != BufferKind::Handles {
            return Err(Status::NOT_SUPPORTED);
        }
        let open: Opener = Box::new(move |src, dst| {
            let path = match &path {
                Some(path) => path.clone(),
                None => find().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "no nonai-2d node on this system")
                })?,
            };
            Device::open(&path, src, dst).map(|device| Box::new(device) as BoxedEngine)
        });
        Ok(Self::with_engine(needs, open))
    }

    fn health(&self) -> Health {
        if self.stream_failures > 0 {
            return Health::Degraded("the job stream failed: trying it again");
        }
        if matches!(self.converter.engine().state(), State::BackingOff { .. }) {
            return Health::Degraded("the engine failed: reopening it");
        }
        Health::Ok
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if self.stream_failures > 0 && now_ns < self.retry_at_ns {
            return Some(self.retry_at_ns);
        }
        match self.converter.serve(&mut *self.jobs, now_ns) {
            Ok(_) => {
                self.stream_failures = 0;
                Some(now_ns.saturating_add(POLL_NS))
            }
            Err(_) => {
                self.retry_at_ns = now_ns.saturating_add(backoff_ns(self.stream_failures));
                self.stream_failures = self.stream_failures.saturating_add(1);
                Some(self.retry_at_ns)
            }
        }
    }
}

impl Nonai2dDriver {
    /// The driver over the engine `open` opens, for a test or a host that builds it directly.
    /// Nothing is opened here.
    #[must_use]
    pub fn with_engine(needs: Nonai2dDriverNeeds, open: Opener) -> Self {
        Self {
            jobs: needs.jobs,
            converter: Converter::new(Nonai2d::new(open)),
            stream_failures: 0,
            retry_at_ns: 0,
        }
    }

    /// The job server.
    pub fn converter(&self) -> &Converter<BoxedEngine, Opener> {
        &self.converter
    }
}

impl ImageConvert for Nonai2dDriver {
    fn info(&mut self) -> Result<ConvertInfo> {
        Ok(ConvertInfo {
            max_width: MAX_WIDTH,
            max_height: MAX_HEIGHT,
        })
    }

    fn conversion_count(&mut self) -> Result<u16> {
        Ok(CONVERSIONS)
    }

    fn conversion(&mut self, index: u16) -> Result<ConversionPair> {
        conversion(index).ok_or(Status::NOT_FOUND)
    }

    fn job_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        let name = JOB_STREAM.as_bytes();
        debug_assert!(name.len() <= MAX_STREAM_NAME);
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.converter.counters())
    }
}
