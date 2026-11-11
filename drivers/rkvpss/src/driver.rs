//! [`Rkvpss`]: the offline scaler as `image.scaler/1`, its jobs served by [`Jobs`] from the
//! device's job stream on one output channel.
//!
//! # The board file
//!
//! ```toml
//! [[devices]]
//! id = "vpss"
//! driver = "rkvpss"
//! config = { channel = 2 }              # required: a channel the camera path doesn't use
//! [devices.streams.jobs]
//! memory = "dma-buf"                    # required: the block takes nothing else
//! buffers = 8
//! buffer_bytes = 3110400
//! job_slots = 16
//! job_bytes = 64
//! ```
//!
//! - `channel`: the output channel, 0 to 5, that writes every job. Required, because selecting
//!   a channel for offline mode takes it from the block's camera path. Its formats are those it
//!   writes that the block also reads (RGB only on channel 1).
//! - `node`: the offline node's path. Left out, the node is found by name (see
//!   [`find`](crate::find)) each time it is opened, so it is found again after a rebind gives it
//!   a new number. Nothing is opened at bind: the first job opens it.
//! - The stream named [`JOB_STREAM`] carries the jobs and holds the images. Its buffers must be
//!   dma-bufs. The driver asks the stream what its buffers are when it binds
//!   (`StreamEndpoint::buffer_kind`), and a stream of plain shared memory is refused there with
//!   `NOT_SUPPORTED`: the device shows as failing to bind, retried as any failed bind is, until
//!   the board file gives the stream `memory = "dma-buf"`.
//!
//! # Ticks
//!
//! Each tick takes the stream's free buffers and runs the jobs waiting, then asks again after
//! [`POLL_NS`]. The kernel's request waits for its frame (500 ms at most), so a tick that runs
//! jobs lasts as long as their frames; see [`Jobs`] for what happens when one times out.
//!
//! # Failures
//!
//! The node: see [`Scaler`] (closed and reopened, 250 ms doubling to 8 s, never given up). The
//! stream: an error from it is retried on the same backoff. Health is degraded while either
//! backs off and while an output is held back. The driver never turns itself off.

use std::io;
use std::path::PathBuf;

use lemnos_contract_image_scaler::{Counters, ImageScaler, MAX_STREAM_NAME, ScalerInfo};
use lemnos_core::{Config, Health, Lifecycle, Result, Status, StreamHandle, driver};
use lemnos_driver_rkvpss_sys::{Channel, MAX_HEIGHT, MAX_WIDTH, Node};
use lemnos_hal::blocks::BufferKind;

use crate::device::{Device, find};
use crate::jobs::Jobs;
use crate::scaler::{Scaler, State, backoff_ns};

/// The stream jobs come on: the need's name, and the name `job_stream` answers.
pub const JOB_STREAM: &str = "jobs";

/// How often the stream is looked at.
pub const POLL_NS: u64 = 1_000_000;

/// A node, as the driver holds it.
pub type BoxedDevice = Box<dyn Device + Send>;

/// What opens the node, as the driver holds it.
pub type Opener = Box<dyn FnMut() -> io::Result<BoxedDevice> + Send>;

/// The rkvpss offline scaler as `image.scaler/1`.
#[driver("rkvpss", provides = [ImageScaler])]
pub struct Rkvpss {
    #[needs]
    jobs: StreamHandle,
    server: Jobs<BoxedDevice, Opener>,
    /// Stream errors in a row.
    stream_failures: u32,
    /// When the stream is tried again after an error.
    retry_at_ns: u64,
}

/// The `channel` setting.
fn channel(cfg: &Config<'_>) -> Result<Channel> {
    cfg.int("channel")
        .and_then(|n| usize::try_from(n).ok())
        .and_then(|n| Channel::ALL.get(n).copied())
        .ok_or(Status::INVALID_ARGUMENT)
}

impl Lifecycle for Rkvpss {
    type Needs = RkvpssNeeds;
    const TICKS: bool = true;

    fn start(needs: RkvpssNeeds, cfg: &Config<'_>) -> Result<Self> {
        let channel = channel(cfg)?;
        // The block takes an image only as a dma-buf: known now, before any consumer comes.
        if needs.jobs.buffer_kind() != BufferKind::Handles {
            return Err(Status::NOT_SUPPORTED);
        }
        let node = match cfg.str("node") {
            Some("") => return Err(Status::INVALID_ARGUMENT),
            Some(path) => Some(PathBuf::from(path)),
            None => None,
        };
        let open: Opener = Box::new(move || {
            let path = node
                .clone()
                .or_else(find)
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            Node::open(path).map(|node| Box::new(node) as BoxedDevice)
        });
        Ok(Self::with_device(needs, open, channel))
    }

    fn health(&self) -> Health {
        if self.stream_failures > 0 {
            return Health::Degraded("the job stream failed: trying it again");
        }
        if self.server.holds_taken() > 0 {
            return Health::Degraded(
                "a frame failed while the block may still write it: its output is held back",
            );
        }
        if matches!(self.server.scaler().state(), State::BackingOff { .. }) {
            return Health::Degraded("the scaler failed: reopening its node");
        }
        Health::Ok
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if self.stream_failures > 0 && now_ns < self.retry_at_ns {
            return Some(self.retry_at_ns);
        }
        match self.server.serve(&mut *self.jobs, now_ns) {
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

impl Rkvpss {
    /// The driver over the node `open` opens, writing with `channel`, for a test or a host that
    /// builds it directly. Nothing is opened here.
    #[must_use]
    pub fn with_device(needs: RkvpssNeeds, open: Opener, channel: Channel) -> Self {
        Self {
            jobs: needs.jobs,
            server: Jobs::new(Scaler::new(open, channel.bit()), channel),
            stream_failures: 0,
            retry_at_ns: 0,
        }
    }

    /// The job server.
    pub fn server(&self) -> &Jobs<BoxedDevice, Opener> {
        &self.server
    }
}

impl ImageScaler for Rkvpss {
    fn info(&mut self) -> Result<ScalerInfo> {
        Ok(ScalerInfo {
            max_width: MAX_WIDTH,
            max_height: MAX_HEIGHT,
        })
    }

    fn format_count(&mut self) -> Result<u16> {
        Ok(self.server.formats().count() as u16)
    }

    fn format(&mut self, index: u16) -> Result<u32> {
        self.server
            .formats()
            .nth(usize::from(index))
            .map(|f| f.fourcc())
            .ok_or(Status::NOT_FOUND)
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
        Ok(self.server.counters())
    }
}
