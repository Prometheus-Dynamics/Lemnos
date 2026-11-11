//! The PiSP front end's kernel interface (`kernel/lemnos_pisp_fe.h`, `kernel/lemnos.h`): its uAPI
//! and safe wrappers over its ioctls and shared mappings, nothing else.
//!
//! The front end shares the receiver's node, `/dev/lemnos-rp1-csi2-<device>`. A job names device
//! addresses and nothing here checks them, because nothing here needs to be believed: the module
//! reads a job's configuration once, into its own memory, and proves every field and range there
//! before the block sees any of it. A wrong address from this crate is a refused job. Output 0
//! is never an address at all: it is the job's buffer, whose memory is the dma-buf
//! [`Stream::set_buffer`] gave the module to pin.
//!
//! Provenance: written for the frame stream, from `crates/lemnos-linux-sys` on the dev branch.

#![cfg(target_os = "linux")]

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::{ior, iow, iowr};

/// `LEMNOS_PISP_FE_ABI`: the interface revision this crate speaks.
pub const ABI: u32 = 1;
/// `LEMNOS_PISP_FE_INFO_WEDGED`: a stop couldn't be confirmed; nothing runs until a new bind.
pub const INFO_WEDGED: u32 = 1;
/// `LEMNOS_RING_STATE_OPEN`.
pub const STATE_OPEN: u32 = 0;
/// `LEMNOS_STREAM_JOB_RANGES`.
pub const JOB_RANGES: usize = 4;
/// `LEMNOS_STREAM_MAX_OWN_BUFFERS`: the most buffers a stream of dma-bufs may have.
pub const MAX_BUFFERS: u32 = 64;
/// `LEMNOS_STREAM_CREATE_BUFFERS`: no pool, each buffer a dma-buf of its own.
const CREATE_BUFFERS: u32 = 1;

const FRAME_WORDS: usize = 5;
const WORD_JOB_SLOTS: usize = 5;
const WORD_FRAMES_HEAD: usize = 16;
const WORD_FRAMES_TAIL: usize = 24;
const WORD_RETURNS_HEAD: usize = 32;
const WORD_RETURNS_TAIL: usize = 40;
const WORD_JOBS_HEAD: usize = 48;
const WORD_JOBS_TAIL: usize = 56;
const WORD_FRAMES: usize = 64;
const JOB_WORDS: usize = 10;

/// `struct lemnos_pisp_fe_info`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// `LEMNOS_PISP_FE_ABI` of the loaded module.
    pub abi: u32,
    /// The front end's version register.
    pub hw_version: u32,
    /// `LEMNOS_PISP_FE_INFO_*`.
    pub flags: u32,
    /// Nonzero between START and STOP.
    pub streaming: u32,
    /// Frame starts since START.
    pub frames: u64,
    /// Frames completed.
    pub written: u64,
    /// Frame starts with no job queued.
    pub dropped: u64,
    /// Frames with no frame end before the next start.
    pub errors: u64,
    /// Jobs the block was not ready to queue.
    pub stalled: u64,
}

/// `struct lemnos_pisp_fe_start`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Start {
    /// The stream's descriptor, made on this node.
    pub stream_fd: i32,
    /// Data lanes.
    pub lanes: u32,
    /// Per-lane rate, Mbit/s.
    pub mbps: u32,
    /// Virtual channel, 0 to 3.
    pub vc: u32,
    /// CSI-2 data type, 1 to 63.
    pub dt: u32,
    /// The frame's width; a job's input must be exactly this.
    pub width: u32,
    /// The frame's height.
    pub height: u32,
    /// Output 0's width, fixed for the stream.
    pub output_width: u32,
    /// Output 0's height.
    pub output_height: u32,
    /// Output 0's bytes per line, a multiple of 16.
    pub output_stride: u32,
    /// Output 0's bytes per frame, within a pool buffer.
    pub output_size: u32,
    /// Output 0's `PISP_IMAGE_FORMAT_` bits.
    pub output_format: u32,
    /// Zero.
    pub flags: u32,
    /// Zero.
    pub reserved: [u32; 2],
}

/// `struct lemnos_stream_create`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct StreamCreate {
    block: u32,
    flags: u32,
    buffers: u32,
    buffer_bytes: u32,
    frame_slots: u32,
    job_slots: u32,
    job_bytes: u32,
    pool_fd: i32,
    doorbell_fd: i32,
    fd: i32,
    ring_bytes: u64,
    pool_addr: u64,
}

/// `struct lemnos_stream_range`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Range {
    /// The device address.
    pub addr: u64,
    /// Bytes.
    pub len: u64,
}

/// `struct lemnos_stream_job`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Job {
    /// Carried to the frame.
    pub identity: u64,
    /// The pool buffer the frame fills, which is output 0.
    pub buffer: u32,
    /// Ranges in use.
    pub count: u32,
    /// In the order `kernel/lemnos_pisp_fe.h` gives.
    pub range: [Range; JOB_RANGES],
}

/// `struct lemnos_stream_register`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Register {
    fd: i32,
    region: u32,
    addr: u64,
    size: u64,
}

/// `struct lemnos_stream_unregister`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Unregister {
    region: u32,
    reserved: u32,
}

/// A dma-buf the block may be sent to, as the module pinned it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    /// What [`Stream::unregister`] takes.
    pub region: u32,
    /// Its device address.
    pub addr: u64,
    /// Its bytes, as the module found them.
    pub size: u64,
}

/// `struct lemnos_stream_buffer`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct StreamBuffer {
    buffer: u32,
    fd: i32,
    addr: u64,
    size: u64,
}

/// A buffer's memory as the module pinned it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferMemory {
    /// The buffer's device address.
    pub addr: u64,
    /// Bytes of it the block may reach.
    pub size: u64,
}

/// `struct lemnos_stream_status`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStatus {
    /// `LEMNOS_RING_STATE_*`.
    pub state: u32,
    /// The errno that ended the stream, or 0.
    pub fault: i32,
    /// Frames published.
    pub frames: u64,
    /// No room on the ring.
    pub dropped: u64,
    /// Buffers given back.
    pub returned: u64,
    /// Jobs accepted.
    pub jobs: u64,
}

const IOC_STREAM_CREATE: u32 = iowr::<StreamCreate>(b'L', 0xc1);
const IOC_FE_INFO: u32 = ior::<Info>(b'L', 0xc8);
const IOC_FE_START: u32 = iow::<Start>(b'L', 0xc9);
const IOC_FE_STOP: u32 = lemnos_linux_sys::ioc(0, b'L', 0xca, 0);
const IOC_KICK: u32 = lemnos_linux_sys::ioc(0, b'L', 0x81, 0);
const IOC_REGISTER: u32 = iowr::<Register>(b'L', 0x82);
const IOC_UNREGISTER: u32 = iow::<Unregister>(b'L', 0x83);
const IOC_STATUS: u32 = ior::<StreamStatus>(b'L', 0x84);
const IOC_BUFFER: u32 = iowr::<StreamBuffer>(b'L', 0x86);

/// What a stream asks of the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamParams {
    /// Buffers, at most [`MAX_BUFFERS`].
    pub buffers: u32,
    /// Bytes per buffer.
    pub buffer_bytes: u32,
    /// Slots on the frame ring, a power of two.
    pub frame_slots: u32,
    /// Slots on the job ring, a power of two.
    pub job_slots: u32,
}

/// One frame the module published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The buffer it is in.
    pub buffer: u32,
    /// Bytes.
    pub bytes: u32,
    /// Counts frames.
    pub sequence: u64,
    /// The job's identity.
    pub identity: u64,
    /// When the frame started.
    pub timestamp_ns: u64,
    /// What its return must carry.
    pub fence: u32,
}

/// The receiver's node, used for the front end. One opener at a time; closing it stops the block.
#[derive(Debug)]
pub struct Node {
    file: File,
}

impl Node {
    /// Opens the node at `path`, refusing another interface revision. `ENOTTY` from a module
    /// with no front end is an error here too.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)?;
        let node = Self { file };
        if node.info()?.abi != ABI {
            return Err(io::ErrorKind::Unsupported.into());
        }
        Ok(node)
    }

    /// The front end's counters, from the kernel's own state.
    pub fn info(&self) -> io::Result<Info> {
        let mut info = Info::default();
        ioctl(self.file.as_fd(), IOC_FE_INFO, (&raw mut info).cast())?;
        Ok(info)
    }

    /// Makes a stream on this block with no pool: each buffer is a dma-buf of its own, given
    /// with [`Stream::set_buffer`], and a job on a buffer without memory is refused.
    pub fn stream_create(&self, params: &StreamParams) -> io::Result<Stream> {
        let mut create = StreamCreate {
            flags: CREATE_BUFFERS,
            buffers: params.buffers,
            buffer_bytes: params.buffer_bytes,
            frame_slots: params.frame_slots,
            job_slots: params.job_slots,
            job_bytes: u32::try_from(size_of::<Job>()).unwrap_or(0),
            pool_fd: -1,
            doorbell_fd: -1,
            fd: -1,
            ..Default::default()
        };
        ioctl(
            self.file.as_fd(),
            IOC_STREAM_CREATE,
            (&raw mut create).cast(),
        )?;
        // SAFETY: a successful create returns a new close-on-exec descriptor that nothing else
        // owns.
        let fd = unsafe { OwnedFd::from_raw_fd(create.fd) };
        let len = usize::try_from(create.ring_bytes)
            .map_err(|_| io::Error::from_raw_os_error(libc::E2BIG))?;
        let ring = Mapping::new(fd.as_fd(), len, 0, true)?;
        let frame_slots = params.frame_slots as usize;
        let returns_at = WORD_FRAMES + frame_slots * FRAME_WORDS;
        let stream = Stream {
            fd,
            ring,
            frame_slots,
            job_slots: params.job_slots as usize,
            buffers: params.buffers,
            returns_at,
            jobs_at: returns_at + frame_slots,
            jobs_head: 0,
            frames_tail: 0,
            returns_head: 0,
        };
        // Checks the mapping is what the module says it made, before anything relies on it.
        if stream.word(WORD_JOB_SLOTS)?.load(Ordering::Acquire) != u64::from(params.job_slots) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(stream)
    }

    /// Starts the front end on the stream. The stream must hold its first job; a refusal of that
    /// job is the error.
    pub fn start(&self, start: &Start) -> io::Result<()> {
        let mut args = *start;
        ioctl(self.file.as_fd(), IOC_FE_START, (&raw mut args).cast()).map(|_| ())
    }

    /// Stops. Returns once the block's DMA has stopped, or could not be confirmed.
    pub fn stop(&self) -> io::Result<()> {
        ioctl(self.file.as_fd(), IOC_FE_STOP, std::ptr::null_mut()).map(|_| ())
    }
}

/// A stream: its mapped rings and where it is on them.
#[derive(Debug)]
pub struct Stream {
    fd: OwnedFd,
    ring: Mapping,
    frame_slots: usize,
    job_slots: usize,
    buffers: u32,
    returns_at: usize,
    jobs_at: usize,
    jobs_head: u64,
    frames_tail: u64,
    returns_head: u64,
}

impl Stream {
    /// The descriptor, for [`Start::stream_fd`].
    #[must_use]
    pub fn raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }

    fn word(&self, at: usize) -> io::Result<&AtomicU64> {
        self.ring
            .as_atomic_words()
            .and_then(|w| w.get(at))
            .ok_or_else(|| io::ErrorKind::InvalidData.into())
    }

    /// Pins a dma-buf the block may be sent to. Its device address.
    pub fn register(&self, memory: BorrowedFd<'_>) -> io::Result<u64> {
        self.register_region(memory).map(|r| r.addr)
    }

    /// Pins a dma-buf the block may be sent to, until [`unregister`](Self::unregister) or the
    /// stream's end. The module refuses one already registered or overlapping one that is, and
    /// one past the stream's regions.
    pub fn register_region(&self, memory: BorrowedFd<'_>) -> io::Result<Region> {
        let mut reg = Register {
            fd: memory.as_raw_fd(),
            ..Default::default()
        };
        ioctl(self.fd.as_fd(), IOC_REGISTER, (&raw mut reg).cast())?;
        Ok(Region {
            region: reg.region,
            addr: reg.addr,
            size: reg.size,
        })
    }

    /// Lets go of a registered dma-buf. The module refuses (`EBUSY`) while a job uses it.
    pub fn unregister(&self, region: u32) -> io::Result<()> {
        let mut arg = Unregister {
            region,
            reserved: 0,
        };
        ioctl(self.fd.as_fd(), IOC_UNREGISTER, (&raw mut arg).cast()).map(|_| ())
    }

    /// Gives `buffer` the dma-buf `memory`, which the module pins for as long as the buffer has
    /// it, or swaps it for what the buffer had. Naming the dma-buf the buffer already has
    /// changes nothing and succeeds. `EBUSY` while a job names the buffer or the block is
    /// writing it, or when the dma-buf is another buffer's; `EINVAL` for one that isn't a
    /// dma-buf the block can take or is smaller than a buffer; `EIO` once the block's DMA could
    /// not be seen to stop.
    pub fn set_buffer(&self, buffer: u32, memory: BorrowedFd<'_>) -> io::Result<BufferMemory> {
        let mut arg = StreamBuffer {
            buffer,
            fd: memory.as_raw_fd(),
            ..Default::default()
        };
        ioctl(self.fd.as_fd(), IOC_BUFFER, (&raw mut arg).cast())?;
        Ok(BufferMemory {
            addr: arg.addr,
            size: arg.size,
        })
    }

    /// Posts a job. `WouldBlock` when the job ring is full.
    pub fn post(&mut self, job: &Job) -> io::Result<()> {
        let tail = self.word(WORD_JOBS_TAIL)?.load(Ordering::Acquire);
        if self.jobs_head.wrapping_sub(tail) >= self.job_slots as u64 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let slot = self.jobs_head as usize & (self.job_slots - 1);
        let at = self.jobs_at + slot * (1 + JOB_WORDS);
        let mut words = [0u64; JOB_WORDS];
        words[0] = job.identity;
        words[1] = u64::from(job.buffer) | u64::from(job.count) << 32;
        for (i, r) in job.range.iter().enumerate() {
            words[2 + 2 * i] = r.addr;
            words[3 + 2 * i] = r.len;
        }
        self.word(at)?
            .store(size_of::<Job>() as u64, Ordering::Relaxed);
        for (i, w) in words.iter().enumerate() {
            self.word(at + 1 + i)?.store(*w, Ordering::Relaxed);
        }
        self.jobs_head += 1;
        self.word(WORD_JOBS_HEAD)?
            .store(self.jobs_head, Ordering::Release);
        Ok(())
    }

    /// Has the module take what's been posted now; its refusal is the error.
    pub fn kick(&self) -> io::Result<()> {
        ioctl(self.fd.as_fd(), IOC_KICK, std::ptr::null_mut()).map(|_| ())
    }

    /// The next frame published, if any.
    pub fn next_frame(&mut self) -> io::Result<Option<Frame>> {
        let head = self.word(WORD_FRAMES_HEAD)?.load(Ordering::Acquire);
        if head == self.frames_tail {
            return Ok(None);
        }
        let at = WORD_FRAMES + (self.frames_tail as usize & (self.frame_slots - 1)) * FRAME_WORDS;
        let word = self.word(at)?.load(Ordering::Relaxed);
        let buffer = word as u32;
        if buffer >= self.buffers {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let frame = Frame {
            buffer,
            bytes: (word >> 32) as u32,
            sequence: self.word(at + 1)?.load(Ordering::Relaxed),
            identity: self.word(at + 2)?.load(Ordering::Relaxed),
            timestamp_ns: self.word(at + 3)?.load(Ordering::Relaxed),
            fence: self.word(at + 4)?.load(Ordering::Relaxed) as u32,
        };
        self.frames_tail += 1;
        self.word(WORD_FRAMES_TAIL)?
            .store(self.frames_tail, Ordering::Release);
        Ok(Some(frame))
    }

    /// Gives a frame's buffer back. `WouldBlock` when the return ring is full.
    pub fn return_buffer(&mut self, buffer: u32, fence: u32) -> io::Result<()> {
        let tail = self.word(WORD_RETURNS_TAIL)?.load(Ordering::Acquire);
        if self.returns_head.wrapping_sub(tail) >= self.frame_slots as u64 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let slot = self.returns_head as usize & (self.frame_slots - 1);
        self.word(self.returns_at + slot)?.store(
            u64::from(buffer) | u64::from(fence) << 32,
            Ordering::Relaxed,
        );
        self.returns_head += 1;
        self.word(WORD_RETURNS_HEAD)?
            .store(self.returns_head, Ordering::Release);
        Ok(())
    }

    /// How the stream is doing.
    pub fn status(&self) -> io::Result<StreamStatus> {
        let mut status = StreamStatus::default();
        ioctl(self.fd.as_fd(), IOC_STATUS, (&raw mut status).cast())?;
        Ok(status)
    }
}

fn ioctl(fd: BorrowedFd<'_>, request: u32, arg: *mut libc::c_void) -> io::Result<libc::c_int> {
    loop {
        // SAFETY: every `request` in this crate is paired, by the callers above, with an `arg`
        // that points at the structure the kernel expects (or is null for one taking none), live
        // for the call. `fd` is live.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg) };
        if ret >= 0 {
            return Ok(ret);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    #[test]
    fn layouts_and_requests_match_the_kernel_headers() {
        assert_eq!(size_of::<Info>(), 56);
        assert_eq!(offset_of!(Info, frames), 16);
        assert_eq!(offset_of!(Info, stalled), 48);
        assert_eq!(size_of::<Start>(), 60);
        assert_eq!(offset_of!(Start, output_format), 44);
        assert_eq!(offset_of!(Start, flags), 48);
        assert_eq!(size_of::<Job>(), 80);
        assert_eq!(offset_of!(Job, buffer), 8);
        assert_eq!(offset_of!(Job, count), 12);
        assert_eq!(offset_of!(Job, range), 16);
        assert_eq!(size_of::<StreamCreate>(), 56);
        assert_eq!(size_of::<Register>(), 24);
        assert_eq!(size_of::<Unregister>(), 8);
        assert_eq!(IOC_UNREGISTER, 0x4008_4c83);
        assert_eq!(size_of::<StreamStatus>(), 40);
        assert_eq!(size_of::<StreamBuffer>(), 24);
        assert_eq!(offset_of!(StreamBuffer, fd), 4);
        assert_eq!(offset_of!(StreamBuffer, addr), 8);
        assert_eq!(offset_of!(StreamBuffer, size), 16);
        assert_eq!(IOC_BUFFER, 0xc018_4c86);
        assert_eq!(IOC_STREAM_CREATE, 0xc038_4cc1);
        assert_eq!(IOC_FE_INFO, 0x8038_4cc8);
        assert_eq!(IOC_FE_START, 0x403c_4cc9);
        assert_eq!(IOC_FE_STOP, 0x0000_4cca);
        assert_eq!(IOC_KICK, 0x0000_4c81);
        assert_eq!(IOC_REGISTER, 0xc018_4c82);
        assert_eq!(IOC_STATUS, 0x8028_4c84);
    }
}
