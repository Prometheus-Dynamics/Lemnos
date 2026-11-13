//! Taking a frame stream: the descriptors arrive once, and after that frames come off the shared
//! rings with no socket involved.
//!
//! The rings, the buffer pool, the holds and the doorbell are the shm family's: the client maps
//! them with the shm crates, which need no `unsafe` here. What only the operating system can do is
//! the rest: receiving the descriptors, and telling the kernel around each CPU access to a dma-buf.
//! So those are the [`StreamPlatform`] trait, and the program brings the implementation: on Linux,
//! `lemnos-linux-stream`.
//!
//! A consumer reads frames, and on a stream its contract has it fill it also writes buffers: a
//! job's input image, a configuration. Such a stream is taken writable
//! ([`Session::open_stream_writable`](crate::Session::open_stream_writable)); every other is
//! mapped read-only for its frames, so a frame can't be scribbled on by mistake.

use std::fmt::Debug;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_wire::Planes;
use lemnos_wire::ring::{Consumer, FrameInfo, Layout, RingError, STATE_OPEN};
use shm_ring::WriteMap;
use shm_ring_buffers::{BufferIndex, Held, NextBatch};
use shm_ring_holds::Marks;

use crate::error::Error;
use crate::holds::Hold;

/// The one kind of access a platform brackets for the kernel on a dma-buf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// How a stream reaches the operating system.
pub trait StreamPlatform: Debug + Send + Sync {
    /// Reads from `socket` into `buf`, appending any descriptors that arrived to `fds`. Returns
    /// the bytes read, 0 at end of file, and the socket's own timeout applies. Implementors own
    /// what they append from the moment they append it.
    fn recv_with_descriptors(
        &self,
        socket: BorrowedFd<'_>,
        buf: &mut [u8],
        fds: &mut Vec<OwnedFd>,
    ) -> io::Result<usize>;

    /// Tells the kernel the CPU is starting (`begin`) or has finished an access of `access` to the
    /// dma-buf `fd`, so a device whose memory isn't cache-coherent sees what was written and the
    /// CPU sees what the device wrote. Called only for the buffers of a stream of dma-bufs, around
    /// each access: a frame is bracketed from when it is taken until it is released, and a write or
    /// read is bracketed around its copy. Nothing is done by default, which is right for memory a
    /// device doesn't share.
    fn dma_sync(&self, fd: BorrowedFd<'_>, access: Access, begin: bool) -> io::Result<()> {
        let _ = (fd, access, begin);
        Ok(())
    }
}

/// One frame taken from a stream, held until it's given back with [`FrameStream::release`]. Only
/// [`FrameStream`] can make one, and it can't be cloned, so a frame is given back once.
#[derive(Debug)]
pub struct HeldFrame {
    buffer: u32,
    bytes: u32,
    sequence: u64,
    identity: u64,
    timestamp_ns: u64,
    /// The buffers of the frame, held while this lives: their marks go when it is dropped.
    _held: Held<Marks<WriteMap>>,
}

impl HeldFrame {
    /// The buffer the frame is in, as the producer numbers it.
    #[must_use]
    pub const fn buffer(&self) -> u32 {
        self.buffer
    }

    /// The frame's length, across its planes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.bytes as usize
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bytes == 0
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub const fn identity(&self) -> u64 {
        self.identity
    }

    /// `CLOCK_MONOTONIC` nanoseconds, or 0 when the producer had no time for the frame.
    #[must_use]
    pub const fn timestamp_ns(&self) -> u64 {
        self.timestamp_ns
    }
}

/// The consumer's end of a stream. A malformed peer is an error here and only here.
#[derive(Debug)]
pub struct FrameStream {
    device: u32,
    name: String,
    consumer: Consumer,
    /// Whether the buffers are dma-bufs, each its own descriptor, around which each access is
    /// bracketed for the kernel.
    dma: bool,
    /// Whether the buffers were mapped to be written too.
    writable: bool,
    /// Each buffer's planes: one, unless the stream's buffers are a descriptor per plane.
    planes: Planes,
    platform: Arc<dyn StreamPlatform>,
    /// The session's record that this stream is open. Dropping the stream without closing it
    /// queues the close.
    hold: Option<Hold>,
}

/// The most descriptors one answer brings: a stream with a descriptor per buffer, at its limit.
pub(crate) const MAX_DESCRIPTORS: usize =
    (lemnos_wire::HANDLE_DESCRIPTORS_BEFORE + lemnos_wire::MAX_HANDLE_BUFFERS) as usize;

impl FrameStream {
    /// Builds the stream from what the answer carried: the descriptors in the order of
    /// [`lemnos_wire::ring::fds`] and then the pool's buffers, the layout, and each buffer's
    /// planes. `shared` says the pool is one descriptor; otherwise each plane of each buffer is its
    /// own dma-buf. Any failure closes every descriptor.
    pub(crate) fn new(
        device: u32,
        name: &str,
        (layout, planes): (Layout, Planes),
        shared: bool,
        fds: Vec<OwnedFd>,
        writable: bool,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<Self, Error> {
        let lengths: &[u32] = if planes.count() == 1 {
            &[]
        } else {
            planes.lengths()
        };
        let consumer =
            Consumer::attach(fds, layout, lengths, shared, writable).map_err(Error::Stream)?;
        Ok(Self {
            device,
            name: name.to_string(),
            consumer,
            dma: !shared,
            writable,
            planes,
            platform,
            hold: None,
        })
    }

    /// Ties the stream to the session's record of it, so that dropping it queues its close.
    pub(crate) fn with_hold(mut self, hold: Hold) -> Self {
        self.hold = Some(hold);
        self
    }

    /// Ends the hold: the stream is being closed explicitly, which the session sends itself.
    pub(crate) fn release_hold(&self) {
        if let Some(hold) = &self.hold {
            hold.release();
        }
    }

    /// Each buffer's planes: one, unless the stream's buffers are a descriptor per plane.
    /// [`write`](Self::write) and [`read`](Self::read) address a buffer's planes end to end.
    #[must_use]
    pub const fn planes(&self) -> &Planes {
        &self.planes
    }

    /// Whether each buffer is a dma-buf of its own (a stream of dma-bufs). Frames read the same
    /// either way.
    #[must_use]
    pub const fn has_buffer_handles(&self) -> bool {
        self.dma
    }

    #[must_use]
    pub const fn device(&self) -> u32 {
        self.device
    }

    /// The length of each buffer, as the stream was opened with.
    #[must_use]
    pub const fn buffer_len(&self) -> usize {
        self.consumer.layout().buffer_bytes as usize
    }

    /// How many buffers the stream has.
    #[must_use]
    pub const fn buffer_count(&self) -> u32 {
        self.consumer.layout().buffers
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn layout(&self) -> &Layout {
        self.consumer.layout()
    }

    /// Whether the host has ended this stream.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.consumer.state() == STATE_OPEN
    }

    /// The pool index of plane `plane` of `buffer`.
    fn index(&self, buffer: u32, plane: u32) -> Option<u32> {
        buffer
            .checked_mul(self.planes.count())?
            .checked_add(plane)
            .filter(|_| plane < self.planes.count())
    }

    /// Brackets the access of every plane of `index`'s buffer for the kernel, on a stream of
    /// dma-bufs. A failure changes nothing about where the bytes are, so it isn't one here.
    fn sync_plane(&self, index: u32, access: Access, begin: bool) {
        if !self.dma {
            return;
        }
        if let Some((fd, _)) = self.consumer.frames().pool().descriptor(BufferIndex(index)) {
            let _ = self.platform.dma_sync(fd, access, begin);
        }
    }

    /// The next frame, if one has arrived. Never waits and allocates nothing.
    pub fn try_next(&mut self) -> Result<Option<HeldFrame>, Error> {
        let planes = self.planes.count();
        let buffer_bytes = self.layout().buffer_bytes as usize;
        let taken = match self.consumer.frames_mut().next() {
            NextBatch::Batch(batch) => {
                let info = FrameInfo::decode(batch.meta).ok_or(Error::Stream(RingError::Broken))?;
                let first = batch
                    .held
                    .buffer(0)
                    .ok_or(Error::Stream(RingError::Broken))?;
                let buffer = first.0 / planes;
                let timestamp_ns = batch.time.as_nanos();
                let held = batch.held;
                if info.bytes as usize > buffer_bytes {
                    return Err(Error::Stream(RingError::Broken));
                }
                Some(HeldFrame {
                    buffer,
                    bytes: info.bytes,
                    sequence: info.sequence,
                    identity: info.identity,
                    timestamp_ns,
                    _held: held,
                })
            }
            NextBatch::Empty | NextBatch::Skipped(_) | NextBatch::Limited => None,
            NextBatch::PoolChanged { .. } | NextBatch::CutOff => {
                return Err(Error::Stream(RingError::Broken));
            }
        };
        if let Some(frame) = taken.as_ref() {
            // The frame is read in place from here until it is given back.
            for plane in 0..planes {
                self.sync_plane(frame.buffer * planes + plane, Access::Read, true);
            }
        }
        Ok(taken)
    }

    /// The next frame, or `None` after `timeout` passes or the host ends the stream.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<HeldFrame>, Error> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(frame) = self.try_next()? {
                return Ok(Some(frame));
            }
            if !self.is_open() {
                return Ok(None);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            if self.consumer.wait(left).is_err() {
                return Ok(None);
            }
        }
    }

    /// The bytes of a frame this stream handed out. On a stream whose buffers are several
    /// planes this is the first plane's part of the frame: read each with
    /// [`plane`](Self::plane). A frame from another stream reads as whatever its numbers fall
    /// on here, and within this stream's memory.
    #[must_use]
    pub fn bytes(&self, frame: &HeldFrame) -> &[u8] {
        self.plane(frame, 0)
    }

    /// The part of a frame in plane `plane` of its buffer: the planes hold the frame's bytes
    /// end to end, so a plane past the frame's length is empty, and one it ends in is cut
    /// there. Empty for a plane the buffers don't have. On a stream of one plane this is
    /// [`bytes`](Self::bytes).
    #[must_use]
    pub fn plane(&self, frame: &HeldFrame, plane: u32) -> &[u8] {
        let (Some(start), Some(length)) = (
            self.planes.offset(plane),
            self.planes.lengths().get(plane as usize),
        ) else {
            return &[];
        };
        let held = (frame.bytes as usize)
            .saturating_sub(start)
            .min(*length as usize);
        let Some(index) = self.index(frame.buffer, plane) else {
            return &[];
        };
        self.consumer
            .frames()
            .pool()
            .bytes(BufferIndex(index))
            .and_then(|bytes| bytes.get(..held))
            .unwrap_or(&[])
    }

    /// The descriptor of `buffer`, on a stream whose buffers are a descriptor each (dma-bufs),
    /// for handing the buffer to something else that takes memory that way: a GPU importing a
    /// frame, an encoder. On a stream whose buffers are in planes this is plane 0's; see
    /// [`plane_handle`](Self::plane_handle). `None` for a stream of shared memory, whose buffers
    /// have no descriptor of their own, and for a buffer the stream doesn't have.
    ///
    /// The descriptor stays the stream's and closes with it; clone it
    /// ([`BorrowedFd::try_clone_to_owned`]) to keep the memory past that. Whatever is handed the
    /// buffer is under the same rule as a write: a frame is the consumer's from when it is taken
    /// until it is released, and a buffer the consumer fills is its own only until it posts the
    /// job that names it.
    #[must_use]
    pub fn buffer_handle(&self, buffer: u32) -> Option<BorrowedFd<'_>> {
        self.plane_handle(buffer, 0)
    }

    /// The descriptor of plane `plane` of `buffer`, as [`buffer_handle`](Self::buffer_handle)
    /// lends plane 0. `None` for a plane the buffers don't have, and for a stream of shared
    /// memory.
    #[must_use]
    pub fn plane_handle(&self, buffer: u32, plane: u32) -> Option<BorrowedFd<'_>> {
        if !self.dma {
            return None;
        }
        let index = self.index(buffer, plane)?;
        self.consumer
            .frames()
            .pool()
            .descriptor(BufferIndex(index))
            .map(|(fd, _)| fd)
    }

    /// Whether the stream was taken to be filled ([`Session::open_stream_writable`]), so that
    /// [`write`](Self::write) works.
    ///
    /// [`Session::open_stream_writable`]: crate::Session::open_stream_writable
    #[must_use]
    pub const fn is_writable(&self) -> bool {
        self.writable
    }

    /// Refuses a buffer the stream doesn't have, and a range that isn't inside the buffer.
    fn check(&self, buffer: u32, offset: usize, len: usize) -> Result<(), Error> {
        let layout = self.layout();
        if buffer >= layout.buffers {
            return Err(Error::Stream(RingError::BadBuffer));
        }
        let inside = offset
            .checked_add(len)
            .is_some_and(|end| end <= layout.buffer_bytes as usize);
        if inside {
            Ok(())
        } else {
            Err(Error::Stream(RingError::BadLength))
        }
    }

    /// Copies `data` into `buffer` at `offset`: a job's input image, a configuration, a
    /// program. Only on a stream taken with [`Session::open_stream_writable`]; on any other it
    /// is [`Error::StreamReadOnly`]. A buffer the stream doesn't have, or a range that isn't
    /// inside the buffer, is refused and nothing is written. Never waits and allocates nothing.
    ///
    /// The memory is shared with the producer, and nothing here knows which buffer is whose at
    /// the moment: the stream's contract does. Write a buffer before posting the job that names
    /// it, and not again until that job has completed; never write a frame the producer may
    /// still be filling. The producer takes nothing in a buffer on trust, so a write at the
    /// wrong time costs the writer its own result and nothing else.
    ///
    /// On a stream of dma-bufs the kernel is told around the copy, so a device that is handed
    /// the buffer next sees what was written.
    ///
    /// [`Session::open_stream_writable`]: crate::Session::open_stream_writable
    pub fn write(&mut self, buffer: u32, offset: usize, data: &[u8]) -> Result<(), Error> {
        if !self.writable {
            return Err(Error::StreamReadOnly);
        }
        self.check(buffer, offset, data.len())?;
        let (mut at, mut left, mut from) = (offset, data.len(), 0);
        let mut written = true;
        while left > 0 {
            let (plane, place, room) = self
                .planes
                .locate(at)
                .ok_or(Error::Stream(RingError::BadLength))?;
            let take = left.min(room);
            let index = self
                .index(buffer, plane)
                .ok_or(Error::Stream(RingError::BadBuffer))?;
            let part = data.get(from..from + take);
            self.sync_plane(index, Access::Write, true);
            written &= match (part, self.consumer.fills()) {
                (Some(part), Some(fills)) => fills
                    .bytes_mut(BufferIndex(index))
                    .and_then(|bytes| bytes.get_mut(place..place + take))
                    .map(|dst| dst.copy_from_slice(part))
                    .is_some(),
                _ => false,
            };
            self.sync_plane(index, Access::Write, false);
            at += take;
            left -= take;
            from += take;
        }
        if written {
            Ok(())
        } else {
            Err(Error::Stream(RingError::BadLength))
        }
    }

    /// Copies from `buffer` at `offset` into `out`, for a buffer that isn't a frame: what a
    /// job left in a buffer the consumer named (a program's outputs, say). A frame is read in
    /// place with [`bytes`](Self::bytes). Refused as [`write`](Self::write) is for a buffer or
    /// a range the stream doesn't have. Never waits and allocates nothing.
    pub fn read(&self, buffer: u32, offset: usize, out: &mut [u8]) -> Result<(), Error> {
        self.check(buffer, offset, out.len())?;
        let (mut at, mut left, mut to) = (offset, out.len(), 0);
        let mut copied = true;
        while left > 0 {
            let (plane, place, room) = self
                .planes
                .locate(at)
                .ok_or(Error::Stream(RingError::BadLength))?;
            let take = left.min(room);
            let index = self
                .index(buffer, plane)
                .ok_or(Error::Stream(RingError::BadBuffer))?;
            self.sync_plane(index, Access::Read, true);
            let source = self
                .consumer
                .frames()
                .pool()
                .bytes(BufferIndex(index))
                .and_then(|bytes| bytes.get(place..place + take));
            match (source, out.get_mut(to..to + take)) {
                (Some(source), Some(into)) => into.copy_from_slice(source),
                _ => copied = false,
            }
            self.sync_plane(index, Access::Read, false);
            at += take;
            left -= take;
            to += take;
        }
        if copied {
            Ok(())
        } else {
            Err(Error::Stream(RingError::BadLength))
        }
    }

    /// Gives a frame's buffer back to the producer. The frame is gone either way: its marks are
    /// taken off when it is dropped, and a dma-buf is bracketed for the kernel first.
    pub fn release(&mut self, frame: HeldFrame) -> Result<(), Error> {
        let planes = self.planes.count();
        for plane in 0..planes {
            self.sync_plane(frame.buffer * planes + plane, Access::Read, false);
        }
        drop(frame);
        Ok(())
    }

    /// Posts a job for the producer. `false` means too many are waiting.
    pub fn post_job(&mut self, job: &[u8]) -> Result<bool, Error> {
        self.consumer.jobs().post(job).map_err(Error::Stream)
    }
}
