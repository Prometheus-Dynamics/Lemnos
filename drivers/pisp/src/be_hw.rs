//! The back end as the driver sees it, and the Linux implementation over the kernel module.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use lemnos_core::{Result, Status};
use lemnos_driver_pisp_sys::{self as sys, BeStream, CONFIG_BYTES, Control, Job, StreamParams};
use lemnos_linux_sys::dma_heap;

use crate::be_job::KernelJob;

/// What a stream needs set aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// Jobs the block may hold at once: the module's pool buffers, one per job.
    pub buffers: u32,
    /// Job and configuration slots, a power of two.
    pub job_slots: u32,
    /// Bytes of each of the two temporal denoise averages, when it's on.
    pub tdn_bytes: Option<u32>,
}

/// Where the driver's own memory is, as device addresses. Images are the clients' and are
/// registered job by job; what is left is the block's own working memory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Placed {
    /// The two temporal denoise averages, when it's on.
    pub tdn: Option<[u64; 2]>,
}

/// A finished job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finished {
    /// The pool buffer the job was counted by.
    pub buffer: u32,
    /// Bytes, as the module reports them; 0 when the block flagged the job as failed.
    pub bytes: u32,
    /// Counts frames from the start of the stream.
    pub sequence: u64,
    /// The job's identity.
    pub identity: u64,
    /// When it was published.
    pub timestamp_ns: u64,
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

/// What the driver asks of the block. The real one is [`LinuxHardware`]; tests use a stand-in.
pub trait Hardware: Send {
    /// The block's version register, once a stream has run; 0 before.
    fn hw_version(&self) -> u32;
    /// Makes the memory and the stream.
    fn start(&mut self, plan: &Plan) -> Result<Placed>;
    /// Ends the stream. The module returns once the block's DMA has stopped, or keeps every
    /// buffer it could not prove idle.
    fn stop(&mut self);
    /// Lets the block reach `memory`, a dma-buf, until it is unregistered or the stream ends.
    /// The module checks every range a job names against what is registered, and refuses an
    /// unregister while a job still uses it.
    fn register(&mut self, memory: BorrowedFd<'_>) -> Result<Registered>;
    /// Lets go of what `register` gave.
    fn unregister(&mut self, region: u32) -> Result<()>;
    /// Puts `config`, the driver's own checked copy of the job's configuration, in a
    /// configuration slot, posts the job naming that slot, and has the module take it. `BUSY`
    /// when the job ring is full: nothing was posted and the stream goes on. Any other error ends
    /// the stream.
    fn post(&mut self, job: &KernelJob, config: &[u8; CONFIG_BYTES]) -> Result<()>;
    /// The next finished job.
    fn next_finished(&mut self) -> Result<Option<Finished>>;
    /// Gives a finished job's buffer back.
    fn give_back(&mut self, buffer: u32) -> Result<()>;
    /// Whether the stream is still running.
    fn check(&mut self) -> Result<()>;
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

const PAGE: u64 = 4096;
/// Bytes of a pool buffer. The pool holds no image: a buffer is only how the module counts a job
/// and publishes its end. The module's stream still needs a pool, at least a byte per buffer,
/// each buffer on its own page.
const TOKEN_BYTES: u32 = PAGE as u32;

fn page_align(v: u64) -> u64 {
    v.div_ceil(PAGE) * PAGE
}

struct Running {
    stream: BeStream,
    /// The pool and the temporal denoise averages, held as long as the stream is.
    _memory: Vec<OwnedFd>,
    /// What each buffer's return must carry; a job gives its buffer a new one.
    fences: Vec<u32>,
}

/// The block through `/dev/lemnos`. Opened as it's needed, so a block not yet bound is a failed
/// call rather than a failed bind, and a call can succeed once it is.
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
        let control = Control::open("/dev/lemnos")?;
        let block = control.find_block(&self.settings.device)?;
        let heap = std::fs::File::open(format!("/dev/dma_heap/{}", self.settings.heap))?;
        let pool = dma_heap::allocate(
            heap.as_fd(),
            u64::from(plan.buffers) * u64::from(TOKEN_BYTES),
        )?;
        let stream = control.create(
            block,
            &StreamParams {
                buffers: plan.buffers,
                buffer_bytes: TOKEN_BYTES,
                frame_slots: plan.buffers.next_power_of_two(),
                job_slots: plan.job_slots,
            },
            pool.as_fd(),
        )?;
        let mut memory = vec![pool];
        let tdn = match plan.tdn_bytes {
            Some(bytes) => {
                let fd = dma_heap::allocate(heap.as_fd(), page_align(2 * u64::from(bytes)))?;
                let base = stream.register(fd.as_fd())?;
                memory.push(fd);
                Some([base, base + u64::from(bytes)])
            }
            None => None,
        };
        self.hw_version = stream.hw_version();
        self.running = Some(Running {
            stream,
            _memory: memory,
            fences: vec![0; plan.buffers as usize],
        });
        Ok(Placed { tdn })
    }

    fn running(&mut self) -> Result<&mut Running> {
        self.running.as_mut().ok_or(Status::NO_DEVICE)
    }
}

impl Hardware for LinuxHardware {
    fn hw_version(&self) -> u32 {
        self.hw_version
    }

    fn start(&mut self, plan: &Plan) -> Result<Placed> {
        self.stop();
        self.open(plan).map_err(|e| status(&e))
    }

    fn stop(&mut self) {
        // Dropping the stream closes it: the module waits for the block's DMA there, and keeps
        // what it can't prove idle.
        self.running = None;
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

    fn post(&mut self, job: &KernelJob, config: &[u8; CONFIG_BYTES]) -> Result<()> {
        let stream = &mut self.running()?.stream;
        let Some(slot) = stream.next_config_slot().map_err(|e| status(&e))? else {
            return Err(Status::BUSY);
        };
        // Past the room check, every failure ends the stream: `BUSY` means only a full ring.
        let ended = |e: io::Error| match status(&e) {
            Status::BUSY => Status::IO,
            other => other,
        };
        stream.write_config(slot, config).map_err(ended)?;
        stream
            .post(&Job {
                identity: job.identity,
                buffer: job.buffer,
                config: slot,
                addr: job.addr,
            })
            .map_err(ended)?;
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
}
