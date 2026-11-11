//! The Rockchip IEP2 deinterlacer, behind the vendor kernel's `/dev/mpp_service`, given to a
//! caller as `codec.block/1`. No vendor library: the caller chooses the mode and the detectors'
//! settings, and the driver carries them to the block without reading a pixel.
//!
//! The driver is put together from:
//!
//! - [`Job`]: three input frames, two outputs and the block's two scratch buffers, each placed
//!   in a buffer the claim holds and checked against its measured size, so a job cannot reach
//!   memory the caller does not hold (the [`job`] module lists every check), made into the
//!   parameter structure and the descriptor words' addresses.
//! - `lemnos-driver-rk-mpp`'s parameter-block job ([`lemnos_driver_rk_mpp::ParamJob`]): the
//!   claim writes every descriptor word itself with a buffer it holds, keeps offsets inside the
//!   buffers, checks the words the kernel uses as indices, sizes and counts, and sends the
//!   structure and reads the results at their exact sizes. `lemnos-driver-rk-mpp-sys` issues it
//!   on the real node and checks the chain's shape again.
//! - [`IepBlock`] ([`codec`]): the work format, and `lemnos-driver-codec-jobs` serving the job
//!   stream.
//! - [`Params`], [`Results`] and [`regs`]: the layouts the vendor kernel takes, with their
//!   sources, pinned by the layout tests against `lemnos-driver-rk-mpp`'s facts.
//!
//! # The board file
//!
//! - `node` (string, default `/dev/mpp_service`): the service node.
//! - The stream `jobs` (`[devices.streams.jobs]`), required, with `memory = "dma-buf"`: the
//!   block takes buffers only as dma-bufs, so a stream of plain memory fails the bind with
//!   `NOT_SUPPORTED`. `job_bytes` at least the contract's `JOB_BYTES`.
//!
//! Nothing is opened at bind. The block is claimed when a consumer's first job comes and let go
//! when that consumer goes. The claim is the only session on the service, so this device and an
//! `rk-mpp` device on the same node take turns.
//!
//! # Failure
//!
//! A claim that can't be taken fails the job that needed it and is tried again by the next,
//! behind the service's own backoff (250 ms doubling to 8 s; it never gives up and never turns
//! itself off). Health is `Degraded` with the reason meanwhile, and whenever the job stream
//! itself fails, which is retried the same way.
//!
//! Left out: the `I1O1T` mode (the block reads past its buffer), the planar output (it has no
//! third address), the `P0` chroma layout, the output-mode bit and the ROI.
//!
//! Hardware status: untested on board. Written from the vendor driver's source; the checks are
//! `testing/board-runs/E17.md` and `testing/board-runs/E40.md`.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

pub mod codec;
pub mod job;
mod params;
pub mod regs;

#[cfg(test)]
mod layout_tests;
#[cfg(test)]
mod tests;

use lemnos_contract_codec_block::{BlockInfo, CodecBlock, Counters};
use lemnos_core::{Config, Health, Lifecycle, Result, Status, StreamHandle, driver};
use lemnos_driver_codec_jobs::{Block as _, Server, name_into};
use lemnos_driver_rk_mpp::{BoxedDevice, backoff_ns};
use lemnos_driver_rk_mpp_sys::ServiceNode;
use lemnos_hal::blocks::BufferKind;

pub use codec::{CONFIG_BYTES, IepBlock, MAX_WORK_BYTES, TAG_CONFIG, TAG_TUNING};
pub use job::{
    At, Buffer, Chroma, Frame, Input, Job, JobError, MAX_HEIGHT, MAX_OSD_AREAS, MAX_STRIDE,
    MAX_WIDTH, Mode, Output, SLOTS, STRIDE_ALIGN, Slot, TILE_HEIGHT, TILE_WIDTH, scratch_len,
};
pub use params::{
    Addr, DilMode, FieldOrder, MTN_TAB_LEN, MV_HIST_BINS, OSD_AREAS, PARAMS_BYTES, Params, PdComp,
    RESULTS_BYTES, Results, Sampling, Swap, TUNING_BYTES, TUNING_START, Tuning,
};

/// The stream jobs come on: the need's name, and the name `job_stream` answers.
pub const JOB_STREAM: &str = "jobs";
/// The service node when the board file names none.
pub const NODE: &str = "/dev/mpp_service";
/// What health says while the job stream fails.
pub const STREAM_FAILED: &str = "the job stream failed: trying it again";

/// The IEP2 deinterlacer as `codec.block/1`.
#[driver("rk-iep", provides = [CodecBlock])]
pub struct RkIep {
    #[needs]
    jobs: StreamHandle,
    block: IepBlock<BoxedDevice>,
    server: Server,
    /// Stream errors in a row.
    stream_failures: u32,
    /// When the stream is tried again after an error.
    retry_at_ns: u64,
}

impl RkIep {
    /// The driver on `device`, for a test or a host that builds it directly. Nothing is opened
    /// here.
    #[must_use]
    pub fn on(needs: RkIepNeeds, device: BoxedDevice) -> Self {
        Self {
            jobs: needs.jobs,
            block: IepBlock::new(device),
            server: Server::new(),
            stream_failures: 0,
            retry_at_ns: 0,
        }
    }
}

impl Lifecycle for RkIep {
    type Needs = RkIepNeeds;
    const TICKS: bool = true;

    fn start(needs: RkIepNeeds, cfg: &Config<'_>) -> Result<Self> {
        let node = cfg.str("node").unwrap_or(NODE);
        if node.is_empty() {
            return Err(Status::INVALID_ARGUMENT);
        }
        // The block takes a buffer only as a dma-buf: known now, before any consumer comes.
        if needs.jobs.buffer_kind() != BufferKind::Handles {
            return Err(Status::NOT_SUPPORTED);
        }
        Ok(Self::on(needs, BoxedDevice::new(ServiceNode::new(node))))
    }

    fn health(&self) -> Health {
        if self.stream_failures > 0 {
            return Health::Degraded(STREAM_FAILED);
        }
        self.block.health()
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if self.stream_failures > 0 && now_ns < self.retry_at_ns {
            return Some(self.retry_at_ns);
        }
        match self.server.serve(&mut *self.jobs, &mut self.block, now_ns) {
            Ok(more) => {
                self.stream_failures = 0;
                Some(Server::next_tick_ns(more, now_ns))
            }
            Err(_) => {
                self.stream_failures = self.stream_failures.saturating_add(1);
                self.retry_at_ns = now_ns.saturating_add(backoff_ns(self.stream_failures));
                Some(self.retry_at_ns)
            }
        }
    }
}

impl CodecBlock for RkIep {
    fn info(&mut self) -> Result<BlockInfo> {
        Ok(Server::info(&self.block))
    }

    fn block(&mut self, into: &mut [u8]) -> Result<usize> {
        name_into(self.block.name(), into)
    }

    fn job_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        name_into(JOB_STREAM, into)
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.server.counters())
    }
}
