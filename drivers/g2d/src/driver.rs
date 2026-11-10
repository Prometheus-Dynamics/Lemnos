//! [`G2dDriver`]: the engine as `image.convert/1`, its jobs served by [`Converter`] from the
//! device's job stream.
//!
//! # The board file
//!
//! ```toml
//! [[devices]]
//! id = "g2d"
//! driver = "g2d"
//! config = { device = "/dev/g2d" }      # the default
//! [devices.streams.jobs]
//! memory = "dma-buf"                    # required: the engine takes nothing else
//! buffers = 8
//! buffer_bytes = 8294400
//! job_slots = 16
//! job_bytes = 48
//! ```
//!
//! - `device`: the engine's node, [`DEVICE`] when left out. Nothing is opened at bind: the
//!   first job opens it (see [`G2d`]).
//! - The stream named [`JOB_STREAM`] carries the jobs and holds the images. Its buffers must be
//!   dma-bufs, since a job is handed to the engine by descriptor. A stream of plain shared
//!   memory can't say so until a consumer attaches (the producer's end answers "nothing behind
//!   this buffer" until then); from the first buffer it lends with a consumer attached, the
//!   driver reports [`Health::Failed`] with [`NO_HANDLES`], which tells the board author what
//!   to change, and every job fails. Its completion reaches the consumer when it names a
//!   buffer taken since the consumer attached; such a stream can't tell the driver which
//!   buffers those are.
//!
//! # Ticks
//!
//! Each tick takes the stream's free buffers and runs the jobs waiting, then asks again after
//! [`POLL_NS`]. A job runs to its end inside the tick (the kernel's request waits for the
//! engine; on a timeout the BSP driver resets the engine before it returns: `g2d_wait_cmd_finish`
//! in `drivers/g2d/g2d_rcq/g2d.c` of radxa/allwinner-bsp `cubie-aiot-v1.4.6`), so a published
//! output is never one the engine still writes.
//!
//! # Failures
//!
//! The engine: see [`G2d`] (closed and reopened, 250 ms doubling to 8 s, never given up). The
//! stream: an error from it is retried on the same backoff, and health is degraded meanwhile.
//! The driver never turns itself off.
//!
//! # Not provided
//!
//! - `image.scaler/1`: the mixer scales with polyphase filters (`lan2coefftab32_full` across,
//!   `linearcoefftab32` down for RGB, `g2d_vsu_para_set` in `g2d_scal.c`), which is neither of
//!   the contract's two filters. Offering it would be a substitute filter.
//! - `image.blend/1`: the contract's layers are premultiplied, and the BSP driver forces every
//!   layer to non-premultiplied (`g2d_image_check` in `g2d.c` sets `bpremul` to 0); its `Src`
//!   operation has no builder either.

use std::io;
use std::path::PathBuf;

use lemnos_contract_image_convert::{
    ConversionPair, ConvertInfo, Counters, ImageConvert, MAX_STREAM_NAME,
};
use lemnos_core::{Config, Health, Lifecycle, Result, Status, StreamHandle, driver};
use lemnos_hal::ErrorKind;

use crate::convert::{Converter, FORMATS, MAX_BUFFERS};
use crate::engine::{Engine, G2d, State, backoff_ns};
use crate::{DEVICE, Device, MAX_SIZE};

/// The stream jobs come on: the need's name, and the name `job_stream` answers.
pub const JOB_STREAM: &str = "jobs";

/// How often the stream is looked at.
pub const POLL_NS: u64 = 1_000_000;

/// What health says about a job stream without dma-bufs.
pub const NO_HANDLES: &str =
    "the job stream's buffers are not dma-bufs: give it memory = \"dma-buf\" in the board file";

/// An engine, as the driver holds it.
pub type BoxedEngine = Box<dyn Engine + Send>;

/// What opens the engine, as the driver holds it.
pub type Opener = Box<dyn FnMut() -> io::Result<BoxedEngine> + Send>;

/// Whether the stream's buffers have handles, as far as the driver has seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handles {
    /// No buffer has been lent with a consumer attached yet.
    Unknown,
    Yes,
    No,
}

/// The G2D engine as `image.convert/1`.
#[driver("g2d", provides = [ImageConvert])]
pub struct G2dDriver {
    #[needs]
    jobs: StreamHandle,
    converter: Converter<BoxedEngine, Opener>,
    handles: Handles,
    /// Stream errors in a row.
    stream_failures: u32,
    /// When the stream is tried again after an error.
    retry_at_ns: u64,
}

impl Lifecycle for G2dDriver {
    type Needs = G2dDriverNeeds;
    const TICKS: bool = true;

    fn start(needs: G2dDriverNeeds, cfg: &Config<'_>) -> Result<Self> {
        let path = PathBuf::from(cfg.str("device").unwrap_or(DEVICE));
        if path.as_os_str().is_empty() {
            return Err(Status::INVALID_ARGUMENT);
        }
        let open: Opener =
            Box::new(move || Device::open(&path).map(|device| Box::new(device) as BoxedEngine));
        Ok(Self::with_engine(needs, open))
    }

    fn health(&self) -> Health {
        if self.handles == Handles::No {
            return Health::Failed(NO_HANDLES);
        }
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
                self.look_for_handles();
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

impl G2dDriver {
    /// The driver over the engine `open` opens, for a test or a host that builds it directly.
    /// Nothing is opened here.
    #[must_use]
    pub fn with_engine(needs: G2dDriverNeeds, open: Opener) -> Self {
        Self {
            jobs: needs.jobs,
            converter: Converter::new(G2d::new(open)),
            handles: Handles::Unknown,
            stream_failures: 0,
            retry_at_ns: 0,
        }
    }

    /// The job server.
    pub fn converter(&self) -> &Converter<BoxedEngine, Opener> {
        &self.converter
    }

    /// Asks the stream for the handle of a buffer the driver holds: an `Unsupported` answer
    /// means its buffers aren't dma-bufs. Nothing is learnt while no consumer is attached.
    fn look_for_handles(&mut self) {
        let Some(buffer) = (0..MAX_BUFFERS).find(|b| self.converter.holds(*b)) else {
            return;
        };
        match self.jobs.buffer_handle(buffer) {
            Ok(Some(_)) => self.handles = Handles::Yes,
            Err(ErrorKind::Unsupported) => self.handles = Handles::No,
            Ok(None) | Err(_) => {}
        }
    }
}

impl ImageConvert for G2dDriver {
    fn info(&mut self) -> Result<ConvertInfo> {
        Ok(ConvertInfo {
            max_width: MAX_SIZE,
            max_height: MAX_SIZE,
        })
    }

    fn conversion_count(&mut self) -> Result<u16> {
        Ok((FORMATS.len() * FORMATS.len()) as u16)
    }

    fn conversion(&mut self, index: u16) -> Result<ConversionPair> {
        let n = FORMATS.len();
        let i = usize::from(index);
        let (Some(from), Some(to)) = (FORMATS.get(i / n), FORMATS.get(i % n)) else {
            return Err(Status::NOT_FOUND);
        };
        Ok(ConversionPair {
            from: from.0,
            to: to.0,
        })
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
