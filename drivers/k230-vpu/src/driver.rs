//! The driver: the block served as `codec.block/1` on the device's job stream.

use lemnos_contract_codec_block::{BlockInfo, CodecBlock, Counters};
use lemnos_core::{
    Config, ConfigError, Health, Key, Kind, Lifecycle, Result, Status, StreamHandle, driver,
};
use lemnos_driver_codec_jobs::{Block, Server, name_into};
use lemnos_hal::blocks::BufferKind;

use crate::{LinuxDevice, VpuBlock, backoff_ns};

/// The stream jobs come on: the need's name, and the name `job_stream` answers.
pub const JOB_STREAM: &str = "jobs";
/// What health says while the job stream fails.
pub const STREAM_FAILED: &str = "the job stream failed: trying it again";

/// The codec block as the driver holds it: on the real node, or a test's.
pub type BoxedBlock = Box<dyn Block + Send>;

/// The codec block as `codec.block/1`.
#[driver("k230-vpu", provides = [CodecBlock])]
pub struct K230Vpu {
    #[needs(job)]
    jobs: StreamHandle,
    block: BoxedBlock,
    server: Server,
    /// Stream errors in a row.
    stream_failures: u32,
    /// When the stream is tried again after an error.
    retry_at_ns: u64,
}

impl K230Vpu {
    /// The driver over `block`, for a test or a host that builds it directly. Nothing is
    /// opened here.
    #[must_use]
    pub fn on(needs: K230VpuNeeds, block: BoxedBlock) -> Self {
        Self {
            jobs: needs.jobs,
            block,
            server: Server::new(),
            stream_failures: 0,
            retry_at_ns: 0,
        }
    }
}

/// The settings this driver reads from its board file config.
const KEYS: &[Key] = &[Key::required("device", Kind::Str)];

impl Lifecycle for K230Vpu {
    type Needs = K230VpuNeeds;
    const TICKS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        if cfg.str("device").is_some_and(str::is_empty) {
            return Err(ConfigError::new(
                "device",
                "must be a device path, not empty",
            ));
        }
        Ok(())
    }

    fn start(needs: K230VpuNeeds, cfg: &Config<'_>) -> Result<Self> {
        let path = cfg.str("device").ok_or(Status::INVALID_ARGUMENT)?;
        // Both queues take a buffer only as a dma-buf: known now, before any consumer comes.
        if needs.jobs.buffer_kind() != BufferKind::Handles {
            return Err(Status::NOT_SUPPORTED);
        }
        let block = VpuBlock::new(LinuxDevice::new(path));
        Ok(Self::on(needs, Box::new(block)))
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
        match self.server.serve(&mut *self.jobs, &mut *self.block, now_ns) {
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

impl CodecBlock for K230Vpu {
    fn info(&mut self) -> Result<BlockInfo> {
        Ok(Server::info(&*self.block))
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
