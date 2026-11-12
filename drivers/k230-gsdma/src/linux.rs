//! The engine on Linux: its node in `lemnos.ko` (`kernel/lemnos_gsdma.h`), reached through
//! `lemnos-driver-k230-gsdma-sys`.
//!
//! A [`LinuxSession`] is one frame stream on the engine. Each buffer the claim holds is
//! registered on the stream, which pins it and gives its device address; a job is the checked
//! [`Plan`](crate::Plan) as device addresses, posted on the stream's ring. The module checks the
//! job again on its own (every plane's whole extent inside one registered buffer, below the
//! memory the engine reaches, no destination over a source) and builds the descriptors in memory
//! of its own, so nothing this side says is believed. This side checks first anyway, so a job
//! the module would refuse is refused here, where it doesn't end the stream:
//!
//! - every plane's span lies inside the region the module registered for its buffer
//!   (`end <= size`, measured by the module), and
//! - every plane's last byte, as a device address, ends at or below the reach the module gave
//!   when the stream was made.
//!
//! A refusal by the module ends the stream, and with it the session: every later call fails, and
//! the caller takes a new claim. A job that runs too long is stopped by the module, which turns
//! the channel off and on and publishes the job's end with no bytes: [`Finished::Stopped`]. An
//! engine that cannot be shown stopped is wedged: the module keeps the job's memory until a
//! reboot and refuses new streams with `EIO`, which [`Gsdma::claim`](crate::Gsdma::claim)
//! answers through its backoff.
//!
//! Submitting and completing allocate nothing: the job is built on the stack and the frame read
//! off the ring.

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::Duration;

use lemnos_driver_k230_gsdma_sys::{self as sys, Control, Region, Stream, StreamParams};
use lemnos_linux_sys::{dma_buf, dma_heap};

use crate::claim::{Device, Finished, MAX_BUFFERS, Request, Session};
use crate::job::Span;

/// Bytes of a pool buffer. The pool holds no image: a buffer is only how the module counts a job
/// and publishes its end, and each needs a page of its own.
const TOKEN_BYTES: u32 = 4096;
/// Pool buffers. One job is in flight at a time; two let the next take a buffer while the last
/// one's return is still on the ring.
const TOKENS: u32 = 2;

/// Where the engine is and what to allocate the pool from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    device: String,
    heap: String,
}

impl Settings {
    /// The engine's device name as the module lists it, and a dma-heap's name under
    /// `/dev/dma_heap`. `InvalidInput` for a name that could name a path elsewhere.
    pub fn new(device: &str, heap: &str) -> io::Result<Self> {
        let ok = |name: &str| !name.is_empty() && !name.contains('/') && !name.contains('\0');
        if !ok(device) || !ok(heap) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            device: device.to_owned(),
            heap: heap.to_owned(),
        })
    }
}

/// The engine through `/dev/lemnos`.
#[derive(Debug)]
pub struct LinuxDevice {
    settings: Settings,
}

impl LinuxDevice {
    /// Nothing is opened until a claim.
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self { settings }
    }
}

impl Device for LinuxDevice {
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
            regions: [None; MAX_BUFFERS],
            next: 0,
        })
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        dma_buf::size(fd)
    }
}

/// One stream on the engine. Dropping it closes the stream: the module waits there for the
/// engine, and keeps what it cannot show stopped.
#[derive(Debug)]
pub struct LinuxSession {
    // Declared first, so the stream closes before the pool.
    stream: Stream,
    _pool: OwnedFd,
    regions: [Option<Region>; MAX_BUFFERS],
    /// The pool buffer the next job is counted by.
    next: u32,
}

fn refused(errno: i32) -> io::Error {
    io::Error::from_raw_os_error(errno)
}

impl LinuxSession {
    /// The device address of `span`'s first byte, once its whole extent is proved inside the
    /// buffer's region and below the engine's reach.
    fn address(&self, span: &Span) -> io::Result<u64> {
        let region = self
            .regions
            .get(usize::from(span.buffer.slot))
            .copied()
            .flatten()
            .ok_or_else(|| refused(libc::EFAULT))?;
        if span.end > region.size {
            return Err(refused(libc::EFAULT));
        }
        let end = region
            .addr
            .checked_add(span.end)
            .ok_or_else(|| refused(libc::EOVERFLOW))?;
        if end > self.stream.reach() {
            return Err(refused(libc::ENXIO));
        }
        // `start <= end`, so this is below `end` too.
        Ok(region.addr + span.start)
    }

    /// The job's end, if published, with its buffer given back.
    fn take(&mut self) -> io::Result<Option<Finished>> {
        let Some(frame) = self.stream.next_frame()? else {
            return Ok(None);
        };
        self.stream.return_buffer(frame.buffer, frame.fence)?;
        Ok(Some(if frame.bytes == 0 {
            Finished::Stopped
        } else {
            Finished::Done
        }))
    }
}

impl Session for LinuxSession {
    fn attach(&mut self, slot: usize, fd: BorrowedFd<'_>, size: u64) -> io::Result<()> {
        let to = self
            .regions
            .get_mut(slot)
            .ok_or_else(|| refused(libc::EINVAL))?;
        if to.is_some() {
            return Err(refused(libc::EBUSY));
        }
        let region = self.stream.register(fd)?;
        // The claim checks jobs against `size`: the module's region must hold all of it.
        if region.size < size {
            self.stream.unregister(region.region)?;
            return Err(refused(libc::EINVAL));
        }
        *to = Some(region);
        Ok(())
    }

    fn detach(&mut self, slot: usize) -> io::Result<()> {
        let to = self
            .regions
            .get_mut(slot)
            .ok_or_else(|| refused(libc::EINVAL))?;
        if let Some(region) = *to {
            self.stream.unregister(region.region)?;
            *to = None;
        }
        Ok(())
    }

    fn submit(&mut self, request: &Request<'_>) -> io::Result<()> {
        let plan = request.plan();
        let (width, height) = plan.frame_size();
        let mut job = sys::Job {
            identity: plan.identity(),
            buffer: self.next,
            fourcc: plan.format().fourcc(),
            width,
            height,
            // DRM's bits, as the module takes them (pinned to the module's in the tests).
            rotation: plan.rotation().drm(),
            // At most `MAX_PLANES`.
            planes: plan.planes() as u32,
            plane: [sys::Plane::default(); sys::MAX_PLANES],
        };
        for (i, to) in job.plane.iter_mut().enumerate().take(plan.planes()) {
            let (Some(src), Some(dst)) = (plan.source(i), plan.destination(i)) else {
                return Err(refused(libc::EINVAL));
            };
            *to = sys::Plane {
                src_addr: self.address(&src)?,
                dst_addr: self.address(&dst)?,
                src_pitch: u32::from(src.pitch),
                dst_pitch: u32::from(dst.pitch),
            };
        }
        self.stream.post(&job)?;
        self.stream.kick()?;
        self.next = (self.next + 1) % TOKENS;
        Ok(())
    }

    fn wait(&mut self, timeout: Duration) -> io::Result<Option<Finished>> {
        if let Some(finished) = self.take()? {
            return Ok(Some(finished));
        }
        self.stream.wait(timeout)?;
        if let Some(finished) = self.take()? {
            return Ok(Some(finished));
        }
        let status = self.stream.status()?;
        if status.state != sys::STATE_OPEN {
            return Err(refused(if status.fault > 0 {
                status.fault
            } else {
                libc::EIO
            }));
        }
        Ok(None)
    }
}
