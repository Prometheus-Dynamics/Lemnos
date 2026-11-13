//! One codec block of the vendor kernel's `/dev/mpp_service`, given to a caller as
//! `codec.block/1`. No vendor library: a job is the register values the caller wants on the
//! block and the buffers they run on, and the driver carries them without reading what they
//! mean.
//!
//! The driver is three things put together, and adds nothing of its own to what a job may
//! do:
//!
//! - `lemnos-driver-codec-jobs` serves the job stream: buffers held, jobs taken, work read
//!   once and checked to be work, completions written.
//! - `lemnos-driver-rk-mpp`'s [`MppBlock`] turns the work into a job on its claim, with every
//!   descriptor, offset, index and length checked there (its `codec` and `job` modules list
//!   them), and names the work's item tags and slots.
//! - `lemnos-driver-rk-mpp-sys` issues the requests on the real node.
//!
//! # The board file
//!
//! - `block` (string, required): `vdpu1`, `vdpu2`, `vepu1`, `vepu2`, `jpeg-decoder` or
//!   `jpeg-encoder`. A device serves one block; a board with several names several devices,
//!   and only one of them can hold the service's session at a time, since the claim that
//!   confines a job is the only session on the node.
//! - `node` (string, default `/dev/mpp_service`): the service node.
//! - The stream `jobs` (`[devices.streams.jobs]`), required, with `memory = "dma-buf"`: the
//!   block takes buffers only as dma-bufs, so a stream of plain memory fails the bind with
//!   `NOT_SUPPORTED`. `job_bytes` at least the contract's `JOB_BYTES`.
//!
//! Nothing is opened at bind. The block is claimed when a consumer's first job comes and let
//! go when that consumer goes.
//!
//! # Failure
//!
//! A claim that can't be taken fails the job that needed it and is tried again by the next,
//! behind the service's own backoff (250 ms doubling to 8 s; it never gives up and never
//! turns itself off). Health is `Degraded` with the reason meanwhile, and whenever the job
//! stream itself fails, which is retried the same way.
//!
//! Hardware status: untested on board. The check is `testing/board-runs/E37.md`.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

use lemnos_contract_codec_block::{BlockInfo, CodecBlock, Counters};
use lemnos_core::{
    Config, ConfigError, Health, Key, Kind, Lifecycle, Result, Status, StreamHandle, driver,
};
use lemnos_driver_codec_jobs::{Block as _, Server, name_into};
use lemnos_driver_rk_mpp::codec::block_named;
use lemnos_driver_rk_mpp::{Block, BoxedDevice, MppBlock, backoff_ns};
use lemnos_driver_rk_mpp_sys::ServiceNode;
use lemnos_hal::blocks::BufferKind;

/// The stream jobs come on: the need's name, and the name `job_stream` answers.
pub const JOB_STREAM: &str = "jobs";
/// The service node when the board file names none.
pub const NODE: &str = "/dev/mpp_service";
/// What health says while the job stream fails.
pub const STREAM_FAILED: &str = "the job stream failed: trying it again";

/// One block of the codec service as `codec.block/1`.
#[driver("rk-mpp", provides = [CodecBlock])]
pub struct RkMpp {
    #[needs(job)]
    jobs: StreamHandle,
    block: MppBlock<BoxedDevice>,
    server: Server,
    /// Stream errors in a row.
    stream_failures: u32,
    /// When the stream is tried again after an error.
    retry_at_ns: u64,
}

impl RkMpp {
    /// The driver for `block` on `device`, for a test or a host that builds it directly.
    /// Nothing is opened here.
    #[must_use]
    pub fn on(needs: RkMppNeeds, device: BoxedDevice, block: Block) -> Self {
        Self {
            jobs: needs.jobs,
            block: MppBlock::new(device, block),
            server: Server::new(),
            stream_failures: 0,
            retry_at_ns: 0,
        }
    }
}

/// The settings a board file gives the block.
const KEYS: &[Key] = &[
    Key::required("block", Kind::Str),
    Key::optional("node", Kind::Str),
];

impl Lifecycle for RkMpp {
    type Needs = RkMppNeeds;
    const TICKS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        if let Some(name) = cfg.str("block")
            && block_named(name).is_none()
        {
            return Err(ConfigError::new(
                "block",
                "must be vdpu1, vdpu2, vepu1, vepu2, jpeg-decoder or jpeg-encoder",
            ));
        }
        if cfg.str("node") == Some("") {
            return Err(ConfigError::new("node", "must not be empty"));
        }
        Ok(())
    }

    fn start(needs: RkMppNeeds, cfg: &Config<'_>) -> Result<Self> {
        let block = cfg
            .str("block")
            .and_then(block_named)
            .ok_or(Status::INVALID_ARGUMENT)?;
        let node = cfg.str("node").unwrap_or(NODE);
        // The block takes a buffer only as a dma-buf: known now, before any consumer comes.
        if needs.jobs.buffer_kind() != BufferKind::Handles {
            return Err(Status::NOT_SUPPORTED);
        }
        Ok(Self::on(
            needs,
            BoxedDevice::new(ServiceNode::new(node)),
            block,
        ))
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

impl CodecBlock for RkMpp {
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

#[cfg(test)]
mod config_tests {
    use lemnos_core::{Config, ConfigError, ConfigValue, Lifecycle};

    use super::RkMpp;

    fn check<'a>(
        entries: &'a [(&'a str, ConfigValue<'a>)],
    ) -> core::result::Result<(), ConfigError<'a>> {
        <RkMpp as Lifecycle>::check_config(&Config::new(entries))
    }

    #[test]
    fn a_full_config_passes_and_unknown_or_misspelled_keys_are_named() {
        let full = [
            ("block", ConfigValue::Str("vepu2")),
            ("node", ConfigValue::Str("/dev/mpp_service")),
        ];
        assert!(check(&full).is_ok());
        assert_eq!(
            check(&[("bogus", ConfigValue::Int(1))]).unwrap_err().key,
            "bogus"
        );
        let near = check(&[("blok", ConfigValue::Str("vepu2"))]).unwrap_err();
        assert_eq!(near.suggestion, Some("block"));
    }

    #[test]
    fn a_wrong_kind_a_missing_block_and_the_value_checks_are_refused() {
        assert_eq!(
            check(&[("block", ConfigValue::Int(2))]).unwrap_err().key,
            "block"
        );
        assert_eq!(
            check(&[("node", ConfigValue::Int(2))]).unwrap_err().key,
            "node"
        );
        let missing = check(&[]).unwrap_err();
        assert_eq!((missing.key, missing.reason), ("block", "is required"));
        assert_eq!(
            check(&[("block", ConfigValue::Str("vepu9"))])
                .unwrap_err()
                .key,
            "block"
        );
        assert_eq!(
            check(&[("block", ConfigValue::Str("jpeg-encoder"))]).err(),
            None
        );
        let empty = check(&[
            ("block", ConfigValue::Str("vdpu1")),
            ("node", ConfigValue::Str("")),
        ]);
        assert_eq!(empty.unwrap_err().key, "node");
        assert!(
            check(&[
                ("block", ConfigValue::Str("vdpu1")),
                ("node", ConfigValue::Str("x"))
            ])
            .is_ok()
        );
    }
}
