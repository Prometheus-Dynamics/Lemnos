//! The block on Linux: its node in `lemnos.ko` (`kernel/lemnos_vip.h`), reached through
//! `lemnos-driver-vip9000-sys`.
//!
//! A [`LinuxSession`] is one frame stream on the block. Each slot is a dma-buf of the session's
//! own, from the board's dma-heap, mapped for the CPU and registered on the stream, which pins it
//! and gives its device address; a run is a job naming the program the module loaded and each
//! slot's first pages, posted on the stream's ring. The module checks the job again on its own
//! and builds the block's tables from the dma-bufs it pinned, so nothing this side says is
//! believed.
//!
//! A refusal by the module ends the stream, and with it the session: every later call fails
//! (`BrokenPipe`), and the claim is dropped and taken again. A job the block faults on or doesn't
//! finish is stopped by a reset and published with no bytes: `Ok(false)`.
//!
//! Running allocates nothing: the job is built on the stack and the frame read off the ring.

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};

use lemnos_driver_vip9000_sys::{self as sys, Control, Region, Stream, StreamParams};
use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::{dma_buf, dma_heap};

use crate::claim::{MAX_SLOTS, Module, PAGE, Session};

/// Bytes of a pool buffer: the pool holds no data, a buffer is only how the module counts a job.
const TOKEN_BYTES: u32 = 4096;
/// Pool buffers. One job is in flight at a time; two let the next take a buffer while the last
/// one's return is still on the ring.
const TOKENS: u32 = 2;
/// The longest a run is waited for. The module stops a job long before (its own limit) and
/// publishes its end; this only bounds a module that never answers.
const RUN_WAIT: Duration = Duration::from_secs(30);

/// Where the block is and what to allocate from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    device: String,
    heap: String,
}

/// Whether `name` can name a device or a heap under its directory: not empty, and with no `/`
/// or NUL, so it cannot name a path elsewhere.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && !name.contains('\0')
}

impl Settings {
    /// The block's device name as the module lists it, and a dma-heap's name under
    /// `/dev/dma_heap`. `InvalidInput` for a name that could name a path elsewhere.
    pub fn new(device: &str, heap: &str) -> io::Result<Self> {
        if !valid_name(device) || !valid_name(heap) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            device: device.to_owned(),
            heap: heap.to_owned(),
        })
    }
}

/// The block through `/dev/lemnos`.
#[derive(Debug)]
pub struct LinuxModule {
    settings: Settings,
}

impl LinuxModule {
    /// Nothing is opened until a claim.
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self { settings }
    }
}

impl Module for LinuxModule {
    type Session = LinuxSession;

    fn open(&mut self) -> io::Result<LinuxSession> {
        let control = Control::open("/dev/lemnos")?;
        let block = control.find_block(&self.settings.device)?;
        let heap = File::open(format!("/dev/dma_heap/{}", self.settings.heap))?;
        let pool = dma_heap::allocate(heap.as_fd(), u64::from(TOKENS * TOKEN_BYTES))?;
        let stream = control.create(
            block,
            &StreamParams {
                buffers: TOKENS,
                buffer_bytes: TOKEN_BYTES,
                frame_slots: TOKENS,
                job_slots: TOKENS,
            },
            pool.as_fd(),
        )?;
        Ok(LinuxSession {
            stream,
            _pool: pool,
            heap,
            slots: std::array::from_fn(|_| None),
            next: 0,
            identity: 0,
        })
    }
}

/// A slot's memory: the dma-buf, the CPU's mapping, and the module's registration.
#[derive(Debug)]
struct Slot {
    map: Mapping,
    fd: OwnedFd,
    region: Region,
}

/// One stream on the block. Dropping it closes the stream: the module stops the block there,
/// and keeps what it cannot show stopped.
#[derive(Debug)]
pub struct LinuxSession {
    // Declared first, so the stream closes before the pool and the slots.
    stream: Stream,
    _pool: OwnedFd,
    heap: File,
    slots: [Option<Slot>; MAX_SLOTS as usize],
    /// The pool buffer the next job is counted by.
    next: u32,
    identity: u64,
}

fn errno(e: i32) -> io::Error {
    io::Error::from_raw_os_error(e)
}

impl LinuxSession {
    /// The stream's state, as an error when it has ended.
    fn open_or_ended(&self) -> io::Result<()> {
        let status = self.stream.status()?;
        if status.state != sys::STATE_OPEN {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                errno(if status.fault > 0 {
                    status.fault
                } else {
                    libc::EIO
                }),
            ));
        }
        Ok(())
    }

    fn sync(&self, slots: usize, flags: u64) -> io::Result<()> {
        for slot in self.slots.iter().take(slots).flatten() {
            dma_buf::sync(
                slot.fd.as_fd(),
                flags | dma_buf::SYNC_READ | dma_buf::SYNC_WRITE,
            )?;
        }
        Ok(())
    }
}

impl Session for LinuxSession {
    fn load(&mut self, program: &[u8]) -> io::Result<u32> {
        self.stream.load(program).map_err(|refused| refused.error)
    }

    fn unload(&mut self, program: u32) -> io::Result<()> {
        self.stream.unload(program)
    }

    fn stage(&mut self, slot: u16, bytes: u32) -> io::Result<()> {
        let s = usize::from(slot);
        if s >= self.slots.len() || bytes == 0 || !bytes.is_multiple_of(PAGE) {
            return Err(errno(libc::EINVAL));
        }
        if self.slots[s]
            .as_ref()
            .is_some_and(|old| old.region.size >= u64::from(bytes))
        {
            return Ok(());
        }
        let fd = dma_heap::allocate(self.heap.as_fd(), u64::from(bytes))?;
        let map = Mapping::new(fd.as_fd(), bytes as usize, 0, true)?;
        let region = self.stream.register(fd.as_fd())?;
        if region.size < u64::from(bytes) || region.addr % u64::from(PAGE) != 0 {
            self.stream.unregister(region.region)?;
            return Err(errno(libc::EINVAL));
        }
        dma_buf::sync(
            fd.as_fd(),
            dma_buf::SYNC_START | dma_buf::SYNC_READ | dma_buf::SYNC_WRITE,
        )?;
        if let Some(old) = self.slots[s].replace(Slot { map, fd, region }) {
            self.stream.unregister(old.region.region)?;
        }
        Ok(())
    }

    fn memory(&mut self, slot: u16) -> Option<&mut [u8]> {
        self.slots
            .get_mut(usize::from(slot))?
            .as_mut()
            .map(|s| s.map.as_mut_slice())
    }

    fn run(&mut self, program: u32, lens: &[u32]) -> io::Result<bool> {
        if lens.len() > sys::SLOTS {
            return Err(errno(libc::EINVAL));
        }
        self.identity = self.identity.wrapping_add(1);
        let mut job = sys::Job {
            identity: self.identity,
            buffer: self.next,
            program,
            slots: lens.len() as u32,
            ..Default::default()
        };
        for (i, &len) in lens.iter().enumerate() {
            let slot = self.slots[i].as_ref().ok_or_else(|| errno(libc::EINVAL))?;
            if u64::from(len) > slot.region.size {
                return Err(errno(libc::EINVAL));
            }
            job.slot[i] = sys::Range {
                addr: slot.region.addr,
                len: u64::from(len),
            };
        }
        self.sync(lens.len(), dma_buf::SYNC_END)?;
        let ran = self.post_and_wait(&job);
        self.sync(lens.len(), dma_buf::SYNC_START)?;
        ran
    }
}

impl LinuxSession {
    fn post_and_wait(&mut self, job: &sys::Job) -> io::Result<bool> {
        self.stream.post(job)?;
        if let Err(error) = self.stream.kick() {
            self.open_or_ended()?;
            return Err(error);
        }
        self.next = (self.next + 1) % TOKENS;
        let until = Instant::now() + RUN_WAIT;
        loop {
            if let Some(frame) = self.stream.next_frame()? {
                self.stream.return_buffer(frame.buffer, frame.fence)?;
                if frame.identity != job.identity {
                    // An earlier job's end: none is outstanding, so the ring is out of step.
                    return Err(errno(libc::EPROTO));
                }
                return Ok(frame.bytes != 0);
            }
            self.open_or_ended()?;
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            self.stream.wait(left.min(Duration::from_millis(100)))?;
        }
    }
}
