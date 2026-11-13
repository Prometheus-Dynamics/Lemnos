//! The VIP9000's node in `lemnos.ko` (`kernel/lemnos_vip.h`, `kernel/lemnos.h`): its uAPI and safe
//! wrappers over its ioctls and the stream's shared ring, nothing else.
//!
//! Nothing here checks what it carries, because nothing here needs to be believed. A program is
//! copied into the module and checked there at load; a job's slots are proved inside dma-bufs
//! registered on the stream, and the block's MMU is given those pages and nothing else while the
//! job runs. A wrong address from this crate is a refused job, which ends the stream, or a fault
//! on the block, which publishes the job's frame with 0 bytes.

#![cfg(target_os = "linux")]

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::{ior, iow, iowr};

/// `LEMNOS_VIP_SLOTS`: the slots a job may name.
pub const SLOTS: usize = 13;
/// `LEMNOS_VIP_PROGRAMS`: the programs a stream holds at once.
pub const PROGRAMS: u32 = 8;
/// `LEMNOS_VIP_MAX_PROGRAM_BYTES`.
pub const MAX_PROGRAM_BYTES: u32 = 65535 * 8 - 64;
/// `LEMNOS_VIP_PROGRAM_ADDR`: where the block sees the program while a job runs.
pub const PROGRAM_ADDR: u32 = 0x0010_0000;
/// `LEMNOS_VIP_SLOT_SPAN`: the most a slot may hold.
pub const SLOT_SPAN: u64 = 1 << 28;
/// The page a slot's address and length are whole numbers of.
pub const PAGE: u64 = 4096;
/// `LEMNOS_VIP_MMU_COMMAND`.
pub const MMU_COMMAND: u32 = 1;
/// `LEMNOS_VIP_MMU_PD`.
pub const MMU_PD: u32 = 2;
/// `LEMNOS_RING_STATE_OPEN`.
pub const STATE_OPEN: u32 = 0;

/// `LEMNOS_VIP_SLOT_ADDR(n)`: where the block sees slot `n` while a job runs.
#[must_use]
pub const fn slot_addr(n: u32) -> u32 {
    (n + 1) << 28
}

const FRAME_WORDS: usize = 5;
const WORD_JOB_SLOTS: usize = 5;
const WORD_FRAMES_HEAD: usize = 16;
const WORD_FRAMES_TAIL: usize = 24;
const WORD_RETURNS_HEAD: usize = 32;
const WORD_RETURNS_TAIL: usize = 40;
const WORD_JOBS_HEAD: usize = 48;
const WORD_JOBS_TAIL: usize = 56;
const WORD_FRAMES: usize = 64;
/// A job on the ring, in 64-bit words.
const JOB_WORDS: usize = size_of::<Job>() / 8;

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

/// `struct lemnos_vip_create`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct VipCreate {
    stream: StreamCreate,
    chip: u32,
    mmu: u32,
}

/// `struct lemnos_stream_range`: device addresses in a registered dma-buf.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Range {
    /// The first byte, page-aligned.
    pub addr: u64,
    /// Whole pages, at most [`SLOT_SPAN`].
    pub len: u64,
}

/// `struct lemnos_vip_job`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Job {
    /// Carried to the frame.
    pub identity: u64,
    /// The pool buffer the frame is counted by.
    pub buffer: u32,
    /// From [`Stream::load`].
    pub program: u32,
    /// Slots used, from 0.
    pub slots: u32,
    /// Zero.
    pub reserved: u32,
    /// The slots; unused ones zero.
    pub slot: [Range; SLOTS],
}

impl Job {
    /// The words the job takes on the ring, little-endian as the module reads them.
    #[must_use]
    pub fn words(&self) -> [u64; JOB_WORDS] {
        let pair = |lo: u32, hi: u32| u64::from(lo) | u64::from(hi) << 32;
        let mut w = [0u64; JOB_WORDS];
        w[0] = self.identity;
        w[1] = pair(self.buffer, self.program);
        w[2] = pair(self.slots, self.reserved);
        for (i, r) in self.slot.iter().enumerate() {
            w[3 + 2 * i] = r.addr;
            w[4 + 2 * i] = r.len;
        }
        w
    }
}

/// `struct lemnos_vip_load`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Load {
    data: u64,
    bytes: u32,
    program: u32,
    refused_at: u32,
    reserved: u32,
}

/// `struct lemnos_vip_unload`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Unload {
    program: u32,
    reserved: u32,
}

/// `struct lemnos_vip_status`: the block, counted since it was bound.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockStatus {
    /// Nonzero once the block could not be shown reset: buffers are held until a reboot.
    pub wedged: u32,
    /// The block's customer id.
    pub chip: u32,
    /// Jobs the block finished.
    pub finished: u64,
    /// Jobs refused by the module's checks.
    pub refused: u64,
    /// Jobs stopped by an MMU or bus fault.
    pub faults: u64,
    /// Jobs stopped for taking too long.
    pub timeouts: u64,
    /// The interrupt status of the last fault.
    pub last_fault: u32,
    /// Resets since bound.
    pub resets: u32,
}

/// A program the module refused at load.
#[derive(Debug)]
pub struct Refused {
    /// `EPERM`: a command a program may not hold; `EINVAL`: one running past the end.
    pub error: io::Error,
    /// The byte offset of the command, when the module named one.
    pub at: Option<u32>,
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

/// A dma-buf the block may be given, as the module pinned it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    /// What [`Stream::unregister`] takes.
    pub region: u32,
    /// Its first byte's device address.
    pub addr: u64,
    /// Its bytes, as the module found them.
    pub size: u64,
}

const IOC_NEXT: u32 = iowr::<Next>(b'L', 0xcf);
const IOC_CREATE: u32 = iowr::<VipCreate>(b'L', 0xe0);
const IOC_LOAD: u32 = iowr::<Load>(b'L', 0xe1);
const IOC_UNLOAD: u32 = iow::<Unload>(b'L', 0xe2);
const IOC_BLOCK_STATUS: u32 = ior::<BlockStatus>(b'L', 0xe3);
const IOC_KICK: u32 = lemnos_linux_sys::ioc(0, b'L', 0x81, 0);
const IOC_REGISTER: u32 = iowr::<Register>(b'L', 0x82);
const IOC_UNREGISTER: u32 = iow::<Unregister>(b'L', 0x83);
const IOC_STATUS: u32 = ior::<StreamStatus>(b'L', 0x84);

/// What a stream asks of the module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamParams {
    /// Buffers in the pool: jobs that may be outstanding at once.
    pub buffers: u32,
    /// Bytes per buffer. The pool holds no data, so a page is enough.
    pub buffer_bytes: u32,
    /// Slots on the frame ring, a power of two.
    pub frame_slots: u32,
    /// Slots on the job ring, a power of two.
    pub job_slots: u32,
}

/// One frame the module published: a job's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The pool buffer.
    pub buffer: u32,
    /// The pool buffer's bytes when the job finished; 0 when it faulted or was stopped.
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
    /// Opens the node at `path`: the process's one open file on it, shared with whatever else in
    /// the process has it open (`lemnos_linux_sys::module::open_shared`).
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

    /// Makes the block's stream on block `block`, its pool in `pool`. `EBUSY` while another
    /// stream has the block, `EIO` once it is wedged.
    pub fn create(
        &self,
        block: u32,
        params: &StreamParams,
        pool: BorrowedFd<'_>,
    ) -> io::Result<Stream> {
        let mut create = VipCreate {
            stream: StreamCreate {
                block,
                flags: 0,
                buffers: params.buffers,
                buffer_bytes: params.buffer_bytes,
                frame_slots: params.frame_slots,
                job_slots: params.job_slots,
                job_bytes: size_of::<Job>() as u32,
                pool_fd: pool.as_raw_fd(),
                doorbell_fd: -1,
                fd: -1,
                ring_bytes: 0,
                pool_addr: 0,
            },
            chip: 0,
            mmu: 0,
        };
        ioctl(self.fd.as_fd(), IOC_CREATE, (&raw mut create).cast())?;
        // SAFETY: a successful create returns a new close-on-exec descriptor that nothing else
        // owns.
        let fd = unsafe { OwnedFd::from_raw_fd(create.stream.fd) };
        let bytes = usize::try_from(create.stream.ring_bytes)
            .map_err(|_| io::Error::from_raw_os_error(libc::E2BIG))?;
        let ring = Mapping::new(fd.as_fd(), bytes, 0, true)?;
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
            chip: create.chip,
            mmu: create.mmu,
        };
        // Checks the mapping is what the module says it made, before anything relies on it.
        if stream.word(WORD_JOB_SLOTS)?.load(Ordering::Acquire) != u64::from(params.job_slots) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(stream)
    }
}

/// The block's stream: what it maps and where it is on the rings. Closing it (dropping it) ends
/// the stream; the module stops the block there, and keeps what it cannot show stopped.
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
    chip: u32,
    mmu: u32,
}

impl Stream {
    /// The block's customer id.
    #[must_use]
    pub fn chip(&self) -> u32 {
        self.chip
    }

    /// How the module points the block at its tables: [`MMU_PD`] or [`MMU_COMMAND`].
    #[must_use]
    pub fn mmu(&self) -> u32 {
        self.mmu
    }

    /// Copies `program` into the module, which checks it once there. Its number for
    /// [`Job::program`]; `EBUSY` while [`PROGRAMS`] are loaded.
    pub fn load(&self, program: &[u8]) -> Result<u32, Refused> {
        let bytes = u32::try_from(program.len()).map_err(|_| Refused {
            error: io::Error::from_raw_os_error(libc::EINVAL),
            at: None,
        })?;
        let mut arg = Load {
            data: program.as_ptr() as u64,
            bytes,
            ..Default::default()
        };
        match ioctl(self.fd.as_fd(), IOC_LOAD, (&raw mut arg).cast()) {
            Ok(_) => Ok(arg.program),
            Err(error) => {
                let named = matches!(error.raw_os_error(), Some(libc::EPERM | libc::EINVAL))
                    && bytes % 8 == 0
                    && bytes != 0
                    && bytes <= MAX_PROGRAM_BYTES;
                Err(Refused {
                    error,
                    at: named.then_some(arg.refused_at),
                })
            }
        }
    }

    /// Lets go of a program. Jobs already started keep their copy.
    pub fn unload(&self, program: u32) -> io::Result<()> {
        let mut arg = Unload {
            program,
            reserved: 0,
        };
        ioctl(self.fd.as_fd(), IOC_UNLOAD, (&raw mut arg).cast())?;
        Ok(())
    }

    fn word(&self, at: usize) -> io::Result<&AtomicU64> {
        self.ring
            .as_atomic_words()
            .and_then(|w| w.get(at))
            .ok_or_else(|| io::ErrorKind::InvalidData.into())
    }

    /// Pins a dma-buf jobs may name, until [`unregister`](Self::unregister) or the stream's end.
    /// The module refuses one already registered or overlapping one that is.
    pub fn register(&self, memory: BorrowedFd<'_>) -> io::Result<Region> {
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

    /// Posts a job. `WouldBlock` when the job ring is full.
    pub fn post(&mut self, job: &Job) -> io::Result<()> {
        let tail = self.word(WORD_JOBS_TAIL)?.load(Ordering::Acquire);
        if self.jobs_head.wrapping_sub(tail) >= self.job_slots as u64 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let slot = self.jobs_head as usize & (self.job_slots - 1);
        let at = self.jobs_at + slot * (1 + JOB_WORDS);
        self.word(at)?
            .store(size_of::<Job>() as u64, Ordering::Relaxed);
        for (i, w) in job.words().iter().enumerate() {
            self.word(at + 1 + i)?.store(*w, Ordering::Relaxed);
        }
        self.jobs_head += 1;
        self.word(WORD_JOBS_HEAD)?
            .store(self.jobs_head, Ordering::Release);
        Ok(())
    }

    /// Has the module take what has been posted now; its refusal is the error, and ends the
    /// stream.
    pub fn kick(&self) -> io::Result<()> {
        ioctl(self.fd.as_fd(), IOC_KICK, std::ptr::null_mut()).map(|_| ())
    }

    /// Waits up to `timeout` until a frame waits or the stream has ended. `true` when it did.
    pub fn wait(&self, timeout: Duration) -> io::Result<bool> {
        lemnos_linux_sys::poll::poll_one(
            self.fd.as_fd(),
            lemnos_linux_sys::poll::POLLIN,
            Some(timeout),
        )
        .map(|events| events != 0)
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
    pub fn block_status(&self) -> io::Result<BlockStatus> {
        let mut status = BlockStatus::default();
        ioctl(self.fd.as_fd(), IOC_BLOCK_STATUS, (&raw mut status).cast())?;
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
    /// `kernel/test/lemnos_vip_check.c`, so the two sides are pinned to each other.
    #[test]
    fn layouts_and_requests_match_the_kernel_headers() {
        assert_eq!(size_of::<Range>(), 16);
        assert_eq!(size_of::<Job>(), 232);
        assert_eq!(offset_of!(Job, buffer), 8);
        assert_eq!(offset_of!(Job, program), 12);
        assert_eq!(offset_of!(Job, slots), 16);
        assert_eq!(offset_of!(Job, slot), 24);
        assert_eq!(size_of::<StreamCreate>(), 56);
        assert_eq!(size_of::<VipCreate>(), 64);
        assert_eq!(offset_of!(VipCreate, chip), 56);
        assert_eq!(offset_of!(VipCreate, mmu), 60);
        assert_eq!(size_of::<Load>(), 24);
        assert_eq!(offset_of!(Load, bytes), 8);
        assert_eq!(offset_of!(Load, program), 12);
        assert_eq!(offset_of!(Load, refused_at), 16);
        assert_eq!(size_of::<Unload>(), 8);
        assert_eq!(size_of::<BlockStatus>(), 48);
        assert_eq!(offset_of!(BlockStatus, finished), 8);
        assert_eq!(offset_of!(BlockStatus, refused), 16);
        assert_eq!(offset_of!(BlockStatus, faults), 24);
        assert_eq!(offset_of!(BlockStatus, timeouts), 32);
        assert_eq!(offset_of!(BlockStatus, last_fault), 40);
        assert_eq!(offset_of!(BlockStatus, resets), 44);
        assert_eq!(size_of::<Register>(), 24);
        assert_eq!(size_of::<Unregister>(), 8);
        assert_eq!(size_of::<StreamStatus>(), 40);
        assert_eq!(size_of::<Next>(), 72);
        assert_eq!(IOC_CREATE, 0xc040_4ce0);
        assert_eq!(IOC_LOAD, 0xc018_4ce1);
        assert_eq!(IOC_UNLOAD, 0x4008_4ce2);
        assert_eq!(IOC_BLOCK_STATUS, 0x8030_4ce3);
        assert_eq!(IOC_KICK, 0x0000_4c81);
        assert_eq!(IOC_REGISTER, 0xc018_4c82);
        assert_eq!(IOC_UNREGISTER, 0x4008_4c83);
        assert_eq!(IOC_STATUS, 0x8028_4c84);
        assert_eq!(IOC_NEXT, 0xc048_4ccf);
        assert_eq!(slot_addr(0), 0x1000_0000);
        assert_eq!(slot_addr(12), 0xd000_0000);
    }

    #[test]
    fn a_jobs_words_are_its_bytes_as_the_module_reads_them() {
        let mut job = Job {
            identity: 0x0102_0304_0506_0708,
            buffer: 1,
            program: 3,
            slots: 2,
            ..Default::default()
        };
        job.slot[0] = Range {
            addr: 0x1000,
            len: 0x2000,
        };
        job.slot[1] = Range {
            addr: 0x8000,
            len: 0x1000,
        };
        let mut bytes = Vec::new();
        for w in job.words() {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        let mut expect = Vec::new();
        expect.extend_from_slice(&job.identity.to_le_bytes());
        for v in [job.buffer, job.program, job.slots, job.reserved] {
            expect.extend_from_slice(&v.to_le_bytes());
        }
        for r in &job.slot {
            expect.extend_from_slice(&r.addr.to_le_bytes());
            expect.extend_from_slice(&r.len.to_le_bytes());
        }
        assert_eq!(bytes, expect);
        assert_eq!(bytes.len(), size_of::<Job>());
    }
}
