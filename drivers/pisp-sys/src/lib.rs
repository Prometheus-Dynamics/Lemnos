//! The PiSP back end's kernel interface (`kernel/lemnos_pisp_be.h`, `kernel/lemnos.h`): its uAPI
//! and safe wrappers over its ioctls and its two shared mappings, nothing else.
//!
//! A job names device addresses, and nothing here checks them, because nothing here needs to be
//! believed: the module copies a job and its configuration into its own memory and proves every
//! address against the memory it holds before the block sees any of it. A wrong address from this
//! crate is a refused job.
//!
//! Provenance: ported from `crates/lemnos-linux-sys/src/lemnos_pisp_be.rs` on the dev branch, for
//! the frame stream.

#![cfg(target_os = "linux")]

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::{ior, iowr};

/// `LEMNOS_PISP_BE_ADDRS`: addresses a job carries.
pub const JOB_ADDRS: usize = 13;
/// `LEMNOS_RING_STATE_OPEN`.
pub const STATE_OPEN: u32 = 0;
/// `sizeof(struct pisp_be_tiles_config)`, one configuration slot.
pub const CONFIG_BYTES: usize = 16720;

const FRAME_WORDS: usize = 5;
const WORD_JOB_SLOTS: usize = 5;
const WORD_FRAMES_HEAD: usize = 16;
const WORD_FRAMES_TAIL: usize = 24;
const WORD_RETURNS_HEAD: usize = 32;
const WORD_RETURNS_TAIL: usize = 40;
const WORD_JOBS_HEAD: usize = 48;
const WORD_JOBS_TAIL: usize = 56;
const WORD_FRAMES: usize = 64;

/// `struct lemnos_next`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Next {
    after: u32,
    block: u32,
    name: [u8; 64],
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

/// `struct lemnos_pisp_be_create`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct BeCreate {
    stream: StreamCreate,
    config_slot_bytes: u32,
    hw_version: u32,
    config_mmap_offset: u64,
    config_bytes: u64,
}

/// `struct lemnos_pisp_be_job`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Job {
    /// Carried to the frame.
    pub identity: u64,
    /// The pool buffer the frame fills.
    pub buffer: u32,
    /// The configuration slot.
    pub config: u32,
    /// Device addresses, by `LEMNOS_PISP_BE_ADDR_*`; zero where unused.
    pub addr: [u64; JOB_ADDRS],
}

/// `struct lemnos_pisp_be_status`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BeStatus {
    /// The block's version register.
    pub hw_version: u32,
    /// Nonzero once the block may still be using memory: until a reboot.
    pub wedged: u32,
    /// Jobs the block has finished, since it was bound.
    pub finished: u64,
    /// Jobs refused by the module's checks, since it was bound.
    pub refused: u64,
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
    /// What [`BeStream::unregister`] takes.
    pub region: u32,
    /// Its device address.
    pub addr: u64,
    /// Its bytes, as the module found them.
    pub size: u64,
}

const IOC_NEXT: u32 = iowr::<Next>(b'L', 0xcf);
const IOC_BE_CREATE: u32 = iowr::<BeCreate>(b'L', 0xb0);
const IOC_BE_STATUS: u32 = ior::<BeStatus>(b'L', 0xb1);
const IOC_KICK: u32 = lemnos_linux_sys::ioc(0, b'L', 0x81, 0);
const IOC_REGISTER: u32 = iowr::<Register>(b'L', 0x82);
const IOC_UNREGISTER: u32 = lemnos_linux_sys::iow::<Unregister>(b'L', 0x83);
const IOC_STATUS: u32 = ior::<StreamStatus>(b'L', 0x84);

/// What a stream asks of the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamParams {
    /// Buffers in the pool.
    pub buffers: u32,
    /// Bytes per buffer.
    pub buffer_bytes: u32,
    /// Slots on the frame ring, a power of two.
    pub frame_slots: u32,
    /// Slots on the job ring and configuration slots, a power of two.
    pub job_slots: u32,
}

/// One frame the module published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The pool buffer.
    pub buffer: u32,
    /// Bytes.
    pub bytes: u32,
    /// Counts frames from the start of the stream.
    pub sequence: u64,
    /// The job's identity.
    pub identity: u64,
    /// When it was published.
    pub timestamp_ns: u64,
    /// What its return must carry.
    pub fence: u32,
}

/// The control node.
#[derive(Debug)]
pub struct Control {
    fd: Arc<OwnedFd>,
}

impl Control {
    /// Opens the node at `path`: the process's one open file on it, shared with whatever else
    /// in the process has it open (`lemnos_linux_sys::module::open_shared`), since the module
    /// takes one opener at a time.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            fd: lemnos_linux_sys::module::open_shared(path.as_ref())?,
        })
    }

    /// The number of the block whose device name is `name`.
    pub fn find_block(&self, name: &str) -> io::Result<u32> {
        let mut next = Next {
            after: 0,
            block: 0,
            name: [0; 64],
        };
        while ioctl(self.fd.as_fd(), IOC_NEXT, (&raw mut next).cast()).is_ok() {
            let end = next.name.iter().position(|&b| b == 0).unwrap_or(64);
            if next.name.get(..end) == Some(name.as_bytes()) {
                return Ok(next.block);
            }
            next.after = next.block;
        }
        Err(io::ErrorKind::NotFound.into())
    }

    /// Makes the back end's stream on block `block`, its buffers in `pool`.
    pub fn create(
        &self,
        block: u32,
        params: &StreamParams,
        pool: BorrowedFd<'_>,
    ) -> io::Result<BeStream> {
        let mut create = BeCreate {
            stream: StreamCreate {
                block,
                flags: 0,
                buffers: params.buffers,
                buffer_bytes: params.buffer_bytes,
                frame_slots: params.frame_slots,
                job_slots: params.job_slots,
                job_bytes: u32::try_from(size_of::<Job>()).unwrap_or(0),
                pool_fd: pool.as_raw_fd(),
                doorbell_fd: -1,
                fd: -1,
                ring_bytes: 0,
                pool_addr: 0,
            },
            ..Default::default()
        };
        ioctl(self.fd.as_fd(), IOC_BE_CREATE, (&raw mut create).cast())?;
        // SAFETY: a successful create returns a new close-on-exec descriptor that nothing else
        // owns.
        let fd = unsafe { OwnedFd::from_raw_fd(create.stream.fd) };
        let bytes =
            |v: u64| usize::try_from(v).map_err(|_| io::Error::from_raw_os_error(libc::E2BIG));
        let ring = Mapping::new(fd.as_fd(), bytes(create.stream.ring_bytes)?, 0, true)?;
        let configs = Mapping::new(
            fd.as_fd(),
            bytes(create.config_bytes)?,
            create.config_mmap_offset,
            true,
        )?;
        let frame_slots = params.frame_slots as usize;
        let returns_at = WORD_FRAMES + frame_slots * FRAME_WORDS;
        let jobs_at = returns_at + frame_slots;
        let stream = BeStream {
            fd,
            ring,
            configs,
            frame_slots,
            job_slots: params.job_slots as usize,
            buffers: params.buffers,
            returns_at,
            jobs_at,
            job_words: 1 + size_of::<Job>() / 8,
            jobs_head: 0,
            frames_tail: 0,
            returns_head: 0,
            pool_addr: create.stream.pool_addr,
            hw_version: create.hw_version,
        };
        // Checks the mapping is what the module says it made, before anything relies on it.
        if stream.word(WORD_JOB_SLOTS)?.load(Ordering::Acquire) != u64::from(params.job_slots) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(stream)
    }
}

/// A back end stream: what it maps and where it is on the rings.
#[derive(Debug)]
pub struct BeStream {
    fd: OwnedFd,
    ring: Mapping,
    configs: Mapping,
    frame_slots: usize,
    job_slots: usize,
    buffers: u32,
    returns_at: usize,
    jobs_at: usize,
    job_words: usize,
    jobs_head: u64,
    frames_tail: u64,
    returns_head: u64,
    pool_addr: u64,
    hw_version: u32,
}

impl BeStream {
    /// Buffer 0's device address.
    #[must_use]
    pub fn pool_addr(&self) -> u64 {
        self.pool_addr
    }

    /// The block's version register.
    #[must_use]
    pub fn hw_version(&self) -> u32 {
        self.hw_version
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
    /// stream's end. The module refuses one already registered or overlapping one that is.
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
        ioctl(self.fd.as_fd(), IOC_UNREGISTER, (&raw mut arg).cast())?;
        Ok(())
    }

    /// Copies configuration slot `slot` into `out`.
    pub fn read_config(&self, slot: u32, out: &mut [u8; CONFIG_BYTES]) -> io::Result<()> {
        let at = slot as usize * CONFIG_BYTES;
        let from = self
            .configs
            .as_slice()
            .get(at..at + CONFIG_BYTES)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        out.copy_from_slice(from);
        Ok(())
    }

    /// Writes configuration slot `slot`.
    pub fn write_config(&self, slot: u32, config: &[u8; CONFIG_BYTES]) -> io::Result<()> {
        self.configs
            .write_bytes(slot as usize * CONFIG_BYTES, config)
            .ok_or_else(|| io::ErrorKind::InvalidInput.into())
    }

    /// The configuration slot that goes with the next job [`post`](Self::post) puts on the ring,
    /// or `None` while the ring is full. A slot is the ring position's: the module copies it when
    /// it takes the job from that position, so it is not written again until the ring has moved
    /// past it, and a job waiting on the ring keeps the configuration it was posted with.
    pub fn next_config_slot(&self) -> io::Result<Option<u32>> {
        let tail = self.word(WORD_JOBS_TAIL)?.load(Ordering::Acquire);
        if self.jobs_head.wrapping_sub(tail) >= self.job_slots as u64 {
            return Ok(None);
        }
        Ok(u32::try_from(self.jobs_head as usize & (self.job_slots - 1)).ok())
    }

    /// Posts a job. `WouldBlock` when the job ring is full.
    pub fn post(&mut self, job: &Job) -> io::Result<()> {
        let tail = self.word(WORD_JOBS_TAIL)?.load(Ordering::Acquire);
        if self.jobs_head.wrapping_sub(tail) >= self.job_slots as u64 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let slot = self.jobs_head as usize & (self.job_slots - 1);
        let at = self.jobs_at + slot * self.job_words;
        let mut words = [0u64; 15];
        words[0] = job.identity;
        words[1] = u64::from(job.buffer) | u64::from(job.config) << 32;
        words[2..].copy_from_slice(&job.addr);
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

    /// How the block is doing.
    pub fn block_status(&self) -> io::Result<BeStatus> {
        let mut status = BeStatus::default();
        ioctl(self.fd.as_fd(), IOC_BE_STATUS, (&raw mut status).cast())?;
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

    /// The same numbers are asserted in C against the header, in
    /// `kernel/test/lemnos_pisp_be_check.c`, so the two sides are pinned to each other.
    #[test]
    fn layouts_and_requests_match_the_kernel_headers() {
        assert_eq!(size_of::<Job>(), 120);
        assert_eq!(offset_of!(Job, buffer), 8);
        assert_eq!(offset_of!(Job, config), 12);
        assert_eq!(offset_of!(Job, addr), 16);
        assert_eq!(size_of::<StreamCreate>(), 56);
        assert_eq!(size_of::<BeCreate>(), 80);
        assert_eq!(offset_of!(BeCreate, config_slot_bytes), 56);
        assert_eq!(offset_of!(BeCreate, hw_version), 60);
        assert_eq!(offset_of!(BeCreate, config_mmap_offset), 64);
        assert_eq!(offset_of!(BeCreate, config_bytes), 72);
        assert_eq!(size_of::<BeStatus>(), 24);
        assert_eq!(offset_of!(BeStatus, finished), 8);
        assert_eq!(offset_of!(BeStatus, refused), 16);
        assert_eq!(size_of::<Register>(), 24);
        assert_eq!(size_of::<Unregister>(), 8);
        assert_eq!(size_of::<StreamStatus>(), 40);
        assert_eq!(size_of::<Next>(), 72);
        assert_eq!(IOC_BE_CREATE, 0xc050_4cb0);
        assert_eq!(IOC_BE_STATUS, 0x8018_4cb1);
        assert_eq!(IOC_KICK, 0x0000_4c81);
        assert_eq!(IOC_REGISTER, 0xc018_4c82);
        assert_eq!(IOC_UNREGISTER, 0x4008_4c83);
        assert_eq!(IOC_STATUS, 0x8028_4c84);
        assert_eq!(IOC_NEXT, 0xc048_4ccf);
    }

    #[test]
    fn a_job_is_as_the_words_on_the_ring_lay_it_out() {
        // Little-endian: buffer is the low half of the second word.
        let job = Job {
            identity: 1,
            buffer: 2,
            config: 3,
            addr: [4; JOB_ADDRS],
        };
        let w = u64::from(job.buffer) | u64::from(job.config) << 32;
        assert_eq!(w, 0x3_0000_0002);
    }
}
