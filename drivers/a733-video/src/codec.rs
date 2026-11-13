//! The engine as `codec.block/1` has it. The block's name is `cedar-ve2`.
//!
//! No job that makes the engine encode or decode can be given to it: that takes register
//! writes, and none can be bounded from user space (the crate's docs say why). What a caller
//! gets is what is sound:
//!
//! - **Item [`TAG_READ`]**, exactly one, two `u32`s: the byte offset of the first register in
//!   the window (a multiple of 4) and how many registers, 1 to [`MAX_READ`], all inside the
//!   window. The job claims the engine (its clocks on, the kernel's lock taken), reads them,
//!   and lets the engine go. They come back as the job's read-back, each a little-endian
//!   `u32`.
//!
//! Every other item is refused, and so is any buffer reference: an attached buffer's bus
//! address is of use only to a register write. A caller that needs the engine to run needs a
//! path this driver does not have, in the kernel, where a job's reach can be checked.
//!
//! The engine is claimed for the length of one job and no longer, so a caller that dies never
//! leaves its clocks on. A claim that fails closes the node and fails the job; later jobs
//! open it again after the block's backoff (250 ms doubling to 8 s, never giving up).

use std::io;
use std::path::PathBuf;

use lemnos_contract_codec_block::work::Work;
use lemnos_contract_codec_block::{BlockInfo, CodecBlock, Counters};
use lemnos_core::{Config, Health, Lifecycle, Result, Status, StreamHandle, driver};
use lemnos_driver_codec_jobs::{Block, Buffers, Server, Start, name_into};

use crate::block::{Device, Lock, State, backoff_ns};
use crate::linux::LinuxDevice;
use crate::uapi::{DEVICE, WINDOW_BYTES};

/// The block's name.
pub const NAME: &str = "cedar-ve2";
/// The item that names the registers to read: the first one's byte offset, and how many.
pub const TAG_READ: u32 = 1;
/// The most registers one job reads.
pub const MAX_READ: u32 = 64;
/// What health says while the engine is closed after a failure.
pub const REOPENING: &str = "the video engine failed: it is opened again by a later job";

/// The engine as `codec.block/1` has it, on the device `open` opens.
pub struct EngineBlock<D: Device, F> {
    engine: crate::Block<D, F>,
    lock: Lock,
}

impl<D: Device, F: FnMut() -> io::Result<D>> EngineBlock<D, F> {
    /// The engine `open` opens, claimed with `lock` for each job. Nothing is opened until a
    /// job comes.
    pub fn new(open: F, lock: Lock) -> Self {
        Self {
            engine: crate::Block::new(open),
            lock,
        }
    }
}

/// A little-endian `u32` at `at` of `bytes`.
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let four = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([four[0], four[1], four[2], four[3]]))
}

impl<D: Device, F: FnMut() -> io::Result<D>> Block for EngineBlock<D, F> {
    fn name(&self) -> &str {
        NAME
    }

    fn limits(&self) -> BlockInfo {
        BlockInfo {
            max_work_bytes: 64,
            max_refs: 0,
            max_items: 1,
            max_read_back: 4 * MAX_READ,
            max_buffers: 0,
        }
    }

    fn start(
        &mut self,
        work: &Work<'_>,
        _buffers: &Buffers<'_>,
        read_back: &mut [u8],
        now_ns: u64,
    ) -> Start {
        let mut items = work.items();
        let (Some((TAG_READ, bytes)), None, 0) = (items.next(), items.next(), work.ref_count())
        else {
            return Start::Refused;
        };
        let (Some(first), Some(count), 8) = (word(bytes, 0), word(bytes, 4), bytes.len()) else {
            return Start::Refused;
        };
        // Every register named lies inside the window, on a register boundary.
        let end = u64::from(first) + 4 * u64::from(count);
        if !(1..=MAX_READ).contains(&count) || !first.is_multiple_of(4) || end > WINDOW_BYTES as u64
        {
            return Start::Refused;
        }
        let bytes = 4 * count as usize;
        let Some(into) = read_back.get_mut(..bytes) else {
            return Start::Failed;
        };
        if self.engine.claim(now_ns, self.lock).is_err() {
            return Start::Failed;
        }
        let mut read = true;
        for (at, four) in into.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            match self.engine.read(first as usize + 4 * at) {
                Ok(value) => *four = value.to_le_bytes(),
                Err(_) => read = false,
            }
        }
        let released = self.engine.release(now_ns).is_ok();
        if read && released {
            Start::Done(bytes)
        } else {
            Start::Failed
        }
    }

    fn release(&mut self) {
        // Nothing of a consumer's is held between jobs.
    }

    fn health(&self) -> Health {
        match self.engine.state() {
            State::BackingOff { .. } => Health::Degraded(REOPENING),
            _ => Health::Ok,
        }
    }
}

/// The stream jobs come on: the need's name, and the name `job_stream` answers.
pub const JOB_STREAM: &str = "jobs";
/// What health says while the job stream fails.
pub const STREAM_FAILED: &str = "the job stream failed: trying it again";

/// The engine's block as the driver holds it.
pub type BoxedBlock = Box<dyn Block + Send>;

/// The second video engine as `codec.block/1`.
///
/// Settings, from the device's `config` in the board file:
///
/// - `route` (string, default `vendor`): `vendor` reaches the engine through the vendor
///   kernel's node, for register reads only (this module's docs); `module` through `lemnos.ko`,
///   which runs encode jobs it has checked ([`crate::module`]).
/// - `device` (string, default `/dev/cedar_dev_ve2`), vendor route: the engine's node.
/// - `lock` (string, default `encoder`), vendor route: the kernel lock a job takes, `decoder`,
///   `encoder` or `jpeg`.
/// - `block` (string, required), module route: the engine's platform device name, as the
///   module lists it.
/// - `node` (string, default `/dev/lemnos`), module route: the module's control node.
///
/// The stream `jobs` is required. On the vendor route its buffers can be plain memory, since no
/// job references one; on the module route a job's buffers are the stream's dma-bufs. Nothing is
/// opened at bind.
#[driver("a733-video", provides = [CodecBlock])]
pub struct A733Video {
    #[needs(job)]
    jobs: StreamHandle,
    block: BoxedBlock,
    server: Server,
    /// Stream errors in a row.
    stream_failures: u32,
    /// When the stream is tried again after an error.
    retry_at_ns: u64,
}

impl A733Video {
    /// The driver over `block`, for a test or a host that builds it directly.
    #[must_use]
    pub fn on(needs: A733VideoNeeds, block: BoxedBlock) -> Self {
        Self {
            jobs: needs.jobs,
            block,
            server: Server::new(),
            stream_failures: 0,
            retry_at_ns: 0,
        }
    }
}

impl Lifecycle for A733Video {
    type Needs = A733VideoNeeds;
    const TICKS: bool = true;

    fn start(needs: A733VideoNeeds, cfg: &Config<'_>) -> Result<Self> {
        match cfg.str("route") {
            None | Some("vendor") => {}
            Some("module") => {
                let node = PathBuf::from(cfg.str("node").unwrap_or(crate::module::NODE));
                let Some(name) = cfg.str("block").filter(|name| !name.is_empty()) else {
                    return Err(Status::INVALID_ARGUMENT);
                };
                let name = name.to_owned();
                let block = crate::module::ModuleBlock::new(move || {
                    crate::module::ModuleEngine::open(&node, &name)
                });
                return Ok(Self::on(needs, Box::new(block)));
            }
            Some(_) => return Err(Status::INVALID_ARGUMENT),
        }
        let path = PathBuf::from(cfg.str("device").unwrap_or(DEVICE));
        if path.as_os_str().is_empty() {
            return Err(Status::INVALID_ARGUMENT);
        }
        let lock = match cfg.str("lock") {
            None | Some("encoder") => Lock::Encoder,
            Some("decoder") => Lock::Decoder,
            Some("jpeg") => Lock::Jpeg,
            Some(_) => return Err(Status::INVALID_ARGUMENT),
        };
        let block = EngineBlock::new(move || LinuxDevice::open(&path), lock);
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
                self.retry_at_ns = now_ns.saturating_add(backoff_ns(self.stream_failures));
                self.stream_failures = self.stream_failures.saturating_add(1);
                Some(self.retry_at_ns)
            }
        }
    }
}

impl CodecBlock for A733Video {
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

#[cfg(test)]
mod tests;
