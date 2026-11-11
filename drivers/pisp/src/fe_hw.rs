//! The front end as the driver sees it, and the Linux implementation over the kernel module.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use lemnos_contract_camera_isp::Counters;
use lemnos_core::{Result, Status};
use lemnos_driver_pisp_fe_sys::{self as sys, Node, Stream, StreamParams};
pub use lemnos_driver_pisp_fe_sys::{Job, Range, Start};
use lemnos_linux_sys::dma_heap;
use lemnos_linux_sys::mmap::Mapping;

use crate::fe_job::{ALIGN, CONFIG_BYTES, STATS_BYTES};

/// What the module said to a buffer's memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Set {
    /// The buffer's memory is that dma-buf now (or already was).
    Done,
    /// A job names the buffer or the block is writing it: ask again when its frame is finished.
    Busy,
    /// Not memory the block can take: not a dma-buf it can map, or smaller than output 0.
    Refused,
}

/// What a stream needs set aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// The stream's buffers, which are output 0's: as many as the `frames` stream has. Their
    /// memory is that stream's, given a buffer at a time with [`Hardware::set_buffer`].
    pub buffers: u32,
    /// The buffers of output 1.
    pub output1_buffers: u32,
    /// Job slots, and configuration and statistics slots: a power of two.
    pub job_slots: u32,
    /// Bytes of output 0: what each of the stream's buffers must hold.
    pub output0_bytes: u32,
    /// Bytes per buffer of output 1, when it's on.
    pub output1_bytes: Option<u32>,
}

/// Where the stream's memory is, as device addresses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Placed {
    /// Configuration slot 0.
    pub config: u64,
    /// Statistics slot 0.
    pub stats: u64,
    /// Output 1's buffer 0, when it's on.
    pub output1: Option<u64>,
}

/// A finished job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finished {
    /// The buffer output 0 is in.
    pub buffer: u32,
    /// Bytes.
    pub bytes: u32,
    /// Counts frames from the start of the stream.
    pub sequence: u64,
    /// The job's identity.
    pub identity: u64,
    /// When the frame started.
    pub timestamp_ns: u64,
}

/// What the driver asks of the block. The real one is [`LinuxHardware`]; tests use a stand-in.
pub trait Hardware: Send {
    /// The block's version register; 0 before the node has been opened.
    fn hw_version(&self) -> u32;
    /// Makes the stream and the configuration, statistics and output 1 memory, starting
    /// nothing. No buffer of the stream has memory yet.
    fn make(&mut self, plan: &Plan) -> Result<Placed>;
    /// Gives `buffer` its memory, or swaps it: the module pins `memory` and, from then on,
    /// that is where the block writes output 0 of a job on that buffer. An error means the
    /// stream is over.
    fn set_buffer(&mut self, buffer: u32, memory: BorrowedFd<'_>) -> Result<Set>;
    /// Starts the block on the stream, which must hold its first job.
    fn go(&mut self, start: &Start) -> Result<()>;
    /// Ends the stream. The module returns once the block's DMA has stopped, or keeps every
    /// buffer it could not prove idle.
    fn stop(&mut self);
    /// Copies configuration slot `slot`.
    fn read_config(&mut self, slot: u32, out: &mut [u8; CONFIG_BYTES]) -> Result<()>;
    /// Posts a job and has the module take it. An error ends the stream.
    fn post(&mut self, job: &Job) -> Result<()>;
    /// The next finished job.
    fn next_finished(&mut self) -> Result<Option<Finished>>;
    /// Gives a finished job's buffer back.
    fn give_back(&mut self, buffer: u32) -> Result<()>;
    /// Whether the stream is still running.
    fn check(&mut self) -> Result<()>;
    /// The module's counters since the last start.
    fn counters(&mut self) -> Result<Counters>;
}

/// Where the block is and what to allocate from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    device: String,
    heap: String,
}

impl Settings {
    /// `INVALID_ARGUMENT` for a name that could name a path elsewhere.
    pub fn new(device: &str, heap: &str) -> Result<Self> {
        let ok = |name: &str| !name.is_empty() && !name.contains('/') && !name.contains('\0');
        if !ok(device) || !ok(heap) {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self {
            device: device.to_owned(),
            heap: heap.to_owned(),
        })
    }
}

fn status(e: &io::Error) -> Status {
    match e.raw_os_error() {
        Some(19) => Status::NO_DEVICE,
        Some(16) => Status::BUSY,
        Some(12) => Status::NO_MEMORY,
        Some(22) => Status::INVALID_ARGUMENT,
        _ => Status::IO,
    }
}

/// What a dma-heap allocation is rounded up to.
const PAGE: u64 = 4096;

/// `v` rounded up to whole pages.
fn page_align(v: u64) -> u64 {
    v.div_ceil(PAGE) * PAGE
}

/// Bytes between two buffers of `bytes` in a region the block's addresses point into.
#[must_use]
pub const fn slot_bytes(bytes: u32) -> u64 {
    (bytes as u64 + ALIGN - 1) & !(ALIGN - 1)
}

struct Running {
    // Declared first, so it goes first: the module stops the block when the stream ends.
    stream: Stream,
    node: Node,
    configs: Mapping,
    /// Registered memory, held as long as the stream is.
    _memory: Vec<OwnedFd>,
    /// What each buffer's return must carry; a frame gives its buffer a new one.
    fences: Vec<u32>,
}

/// The block through its receiver's node. Opened as it's needed, so a block not yet bound is a
/// failed call rather than a failed bind.
pub struct LinuxHardware {
    settings: Settings,
    running: Option<Running>,
    hw_version: u32,
}

impl LinuxHardware {
    /// Nothing is opened until a start.
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            running: None,
            hw_version: 0,
        }
    }

    fn open(&mut self, plan: &Plan) -> io::Result<Placed> {
        let node = Node::open(format!("/dev/lemnos-rp1-csi2-{}", self.settings.device))?;
        let heap = std::fs::File::open(format!("/dev/dma_heap/{}", self.settings.heap))?;
        let buffers = u64::from(plan.output1_buffers);
        let slots = u64::from(plan.job_slots);
        let stream = node.stream_create(&StreamParams {
            buffers: plan.buffers,
            buffer_bytes: plan.output0_bytes,
            frame_slots: plan.buffers.next_power_of_two(),
            job_slots: plan.job_slots,
        })?;
        let mut memory = Vec::new();
        let region = |bytes: u64, memory: &mut Vec<OwnedFd>| -> io::Result<u64> {
            let fd = dma_heap::allocate(heap.as_fd(), page_align(bytes))?;
            let addr = stream.register(fd.as_fd())?;
            memory.push(fd);
            Ok(addr)
        };
        let config_bytes = slots * CONFIG_BYTES as u64;
        let config = region(config_bytes, &mut memory)?;
        let stats = region(slots * slot_bytes(STATS_BYTES as u32), &mut memory)?;
        let output1 = match plan.output1_bytes {
            Some(bytes) => Some(region(buffers * slot_bytes(bytes), &mut memory)?),
            None => None,
        };
        let len = usize::try_from(page_align(config_bytes))
            .map_err(|_| io::Error::from_raw_os_error(7))?;
        let fd = memory
            .first()
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        let configs = Mapping::new(fd.as_fd(), len, 0, false)?;
        self.hw_version = node.info()?.hw_version;
        self.running = Some(Running {
            stream,
            node,
            configs,
            _memory: memory,
            fences: vec![0; plan.buffers as usize],
        });
        Ok(Placed {
            config,
            stats,
            output1,
        })
    }

    fn running(&mut self) -> Result<&mut Running> {
        self.running.as_mut().ok_or(Status::NO_DEVICE)
    }
}

impl Hardware for LinuxHardware {
    fn hw_version(&self) -> u32 {
        self.hw_version
    }

    fn make(&mut self, plan: &Plan) -> Result<Placed> {
        self.stop();
        self.open(plan).map_err(|e| status(&e))
    }

    fn set_buffer(&mut self, buffer: u32, memory: BorrowedFd<'_>) -> Result<Set> {
        match self.running()?.stream.set_buffer(buffer, memory) {
            Ok(_) => Ok(Set::Done),
            Err(e) => match e.raw_os_error() {
                // EBUSY: a job names it or the block has it, or the dma-buf is another buffer's.
                Some(16) => Ok(Set::Busy),
                // EINVAL, EBADF.
                Some(22 | 9) => Ok(Set::Refused),
                _ => Err(status(&e)),
            },
        }
    }

    fn go(&mut self, start: &Start) -> Result<()> {
        let r = self.running()?;
        let args = Start {
            stream_fd: r.stream.raw_fd(),
            ..*start
        };
        r.node.start(&args).map_err(|e| status(&e))
    }

    fn stop(&mut self) {
        if let Some(r) = self.running.take() {
            // Failing only means the node has gone, and dropping the stream stops it either way.
            let _ = r.node.stop();
        }
    }

    fn read_config(&mut self, slot: u32, out: &mut [u8; CONFIG_BYTES]) -> Result<()> {
        let r = self.running()?;
        let at = slot as usize * CONFIG_BYTES;
        let from = r
            .configs
            .as_slice()
            .get(at..at + CONFIG_BYTES)
            .ok_or(Status::INVALID_ARGUMENT)?;
        out.copy_from_slice(from);
        Ok(())
    }

    fn post(&mut self, job: &Job) -> Result<()> {
        let stream = &mut self.running()?.stream;
        stream.post(job).map_err(|e| status(&e))?;
        stream.kick().map_err(|e| status(&e))
    }

    fn next_finished(&mut self) -> Result<Option<Finished>> {
        let r = self.running()?;
        let Some(frame) = r.stream.next_frame().map_err(|e| status(&e))? else {
            return Ok(None);
        };
        if let Some(fence) = r.fences.get_mut(frame.buffer as usize) {
            *fence = frame.fence;
        }
        Ok(Some(Finished {
            buffer: frame.buffer,
            bytes: frame.bytes,
            sequence: frame.sequence,
            identity: frame.identity,
            timestamp_ns: frame.timestamp_ns,
        }))
    }

    fn give_back(&mut self, buffer: u32) -> Result<()> {
        let r = self.running()?;
        let fence = *r
            .fences
            .get(buffer as usize)
            .ok_or(Status::INVALID_ARGUMENT)?;
        r.stream
            .return_buffer(buffer, fence)
            .map_err(|e| status(&e))
    }

    fn check(&mut self) -> Result<()> {
        let s = self.running()?.stream.status().map_err(|e| status(&e))?;
        if s.state == sys::STATE_OPEN {
            Ok(())
        } else {
            Err(Status::IO)
        }
    }

    fn counters(&mut self) -> Result<Counters> {
        let info = self.running()?.node.info().map_err(|e| status(&e))?;
        Ok(Counters {
            jobs: info.written,
            dropped: info.dropped,
            errors: info.errors,
        })
    }
}
