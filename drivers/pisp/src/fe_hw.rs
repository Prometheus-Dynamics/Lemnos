//! The front end as the driver sees it, and the Linux implementation over the kernel module.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use lemnos_contract_camera_isp::Counters;
use lemnos_core::{Result, Status};
use lemnos_driver_pisp_fe_sys::{self as sys, Node, Stream, StreamParams};
pub use lemnos_driver_pisp_fe_sys::{Job, Range, Start};
use lemnos_linux_sys::dma_heap;
use lemnos_linux_sys::mmap::Mapping;

use crate::fe_job::{ALIGN, CONFIG_BYTES};

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
    /// The stream's buffers, which are output 0's: as many as the `output0` stream has. Their
    /// memory is that stream's, given a buffer at a time with [`Hardware::set_buffer`].
    pub buffers: u32,
    /// Job slots on the module's ring: a power of two.
    pub job_slots: u32,
    /// Bytes of output 0: what each of the stream's buffers must hold.
    pub output0_bytes: u32,
}

/// Where the driver's own memory is, as device addresses. Statistics and output 1 are the
/// clients', registered job by job.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Placed {
    /// Configuration slot 0. There is a slot per buffer of the stream, [`slot_bytes`] of
    /// [`CONFIG_BYTES`] apart; a job on buffer `n` has its configuration in slot `n`.
    pub config: u64,
}

/// A dma-buf the block may write, as the module pinned it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registered {
    /// What [`Hardware::unregister`] takes.
    pub region: u32,
    /// Its first byte, as a device address.
    pub addr: u64,
    /// Its real size in bytes, which may be more than asked for: a heap rounds up.
    pub size: u64,
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
    /// Makes the stream and the configuration slots, starting nothing. No buffer of the stream
    /// has memory yet.
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
    /// Writes `config`, the driver's own checked copy of a job's configuration, into
    /// configuration slot `slot`. The module copies it from there once, when it takes the job.
    fn write_config(&mut self, slot: u32, config: &[u8; CONFIG_BYTES]) -> Result<()>;
    /// Lets the block reach `memory`, a dma-buf, until it is unregistered or the stream ends.
    /// The module checks every range a job names against what is registered, and refuses an
    /// unregister while a job still uses it. An error refuses this memory only.
    fn register(&mut self, memory: BorrowedFd<'_>) -> Result<Registered>;
    /// Lets go of what `register` gave. An error means the stream is over.
    fn unregister(&mut self, region: u32) -> Result<()>;
    /// Posts a job and has the module take it. `BUSY` when the job ring is full: nothing was
    /// posted and the stream goes on. Any other error ends the stream.
    fn post(&mut self, job: &Job) -> Result<()>;
    /// The next finished job.
    fn next_finished(&mut self) -> Result<Option<Finished>>;
    /// Gives a finished job's buffer back.
    fn give_back(&mut self, buffer: u32) -> Result<()>;
    /// Whether the stream is still running.
    fn check(&mut self) -> Result<()>;
    /// The module's counters since the last start.
    fn counters(&mut self) -> Result<Counters>;
    /// Frame starts since the last start at which the block still had the job before queued
    /// (the module's `stalled`). 0 when nothing is running.
    fn stalled(&mut self) -> u64 {
        0
    }
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

impl Settings {
    /// The platform device name of the block, as the board file gives it.
    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }
}

/// The hardware a simulated board stands in, in place of the module's node: built for each
/// driver the host binds. Compiled only with the `sim-hardware` feature, so a daemon built
/// without it cannot be given any other block than the real one.
#[cfg(feature = "sim-hardware")]
type MakeHardware = fn(&Settings) -> Box<dyn Hardware>;

#[cfg(feature = "sim-hardware")]
static SIMULATED: std::sync::OnceLock<MakeHardware> = std::sync::OnceLock::new();

/// Makes every driver this crate binds from now on use `make` for its block instead of the
/// module's node. `false` if something was installed already. For a simulated board only.
#[cfg(feature = "sim-hardware")]
pub fn install_simulated(make: MakeHardware) -> bool {
    SIMULATED.set(make).is_ok()
}

/// The block the driver bound with `settings` talks to: the module's, or the simulated one.
pub(crate) fn hardware(settings: Settings) -> Box<dyn Hardware> {
    #[cfg(feature = "sim-hardware")]
    if let Some(make) = SIMULATED.get() {
        return make(&settings);
    }
    Box::new(LinuxHardware::new(settings))
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
    /// The configuration slots, written by the driver only.
    configs: Mapping,
    /// The configuration slots' memory, held as long as the stream is.
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
        // The front end's part: its calls drive only what it claims. A claim another file holds
        // is EBUSY, so a second front end on this block is refused, by the module.
        node.claim()?;
        let heap = std::fs::File::open(format!("/dev/dma_heap/{}", self.settings.heap))?;
        let stream = node.stream_create(&StreamParams {
            buffers: plan.buffers,
            buffer_bytes: plan.output0_bytes,
            frame_slots: plan.buffers.next_power_of_two(),
            job_slots: plan.job_slots,
        })?;
        let config_bytes = page_align(u64::from(plan.buffers) * slot_bytes(CONFIG_BYTES as u32));
        let fd = dma_heap::allocate(heap.as_fd(), config_bytes)?;
        let config = stream.register(fd.as_fd())?;
        let len = usize::try_from(config_bytes).map_err(|_| io::Error::from_raw_os_error(7))?;
        let configs = Mapping::new(fd.as_fd(), len, 0, true)?;
        self.hw_version = node.info()?.hw_version;
        self.running = Some(Running {
            stream,
            node,
            configs,
            _memory: vec![fd],
            fences: vec![0; plan.buffers as usize],
        });
        Ok(Placed { config })
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

    fn write_config(&mut self, slot: u32, config: &[u8; CONFIG_BYTES]) -> Result<()> {
        let r = self.running()?;
        let at = usize::try_from(u64::from(slot) * slot_bytes(CONFIG_BYTES as u32))
            .map_err(|_| Status::INVALID_ARGUMENT)?;
        r.configs
            .write_bytes(at, config)
            .ok_or(Status::INVALID_ARGUMENT)
    }

    fn register(&mut self, memory: BorrowedFd<'_>) -> Result<Registered> {
        let r = self
            .running()?
            .stream
            .register_region(memory)
            .map_err(|e| status(&e))?;
        Ok(Registered {
            region: r.region,
            addr: r.addr,
            size: r.size,
        })
    }

    fn unregister(&mut self, region: u32) -> Result<()> {
        self.running()?
            .stream
            .unregister(region)
            .map_err(|e| status(&e))
    }

    fn post(&mut self, job: &Job) -> Result<()> {
        let stream = &mut self.running()?.stream;
        // Past the room check, every failure ends the stream: `BUSY` means only a full ring.
        let ended = |e: io::Error| match status(&e) {
            Status::BUSY => Status::IO,
            other => other,
        };
        match stream.post(job) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Err(Status::BUSY),
            Err(e) => return Err(ended(e)),
        }
        stream.kick().map_err(ended)
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

    fn stalled(&mut self) -> u64 {
        self.running()
            .ok()
            .and_then(|r| r.node.info().ok())
            .map_or(0, |info| info.stalled)
    }
}
