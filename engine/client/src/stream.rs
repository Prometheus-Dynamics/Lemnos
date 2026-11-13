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
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_wire::Planes;
use lemnos_wire::ring::{Consumer, FrameInfo, Layout, Mapping, RingError, STATE_OPEN};
use shm_ring::WriteMap;
use shm_ring_buffers::{BufferIndex, Held, NextBatch};
use shm_ring_holds::Marks;

use crate::error::Error;
use crate::holds::Hold;

/// What a stream's buffers are in: one shared pool of memfd memory, or a dma-buf of its own for
/// each buffer's plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryKind {
    SharedMemory,
    DmaBuf,
}

/// One plane of one buffer, as the stream holds it: the descriptor its bytes are in, where they
/// start in it, and how much memory there is. A region is lent for as long as the stream is, and
/// the same for every frame that is in that buffer.
#[derive(Debug, Clone, Copy)]
pub struct Region<'a> {
    /// The descriptor: a memfd of the shared pool, or a dma-buf. Borrowed; the stream closes it.
    /// `try_clone` keeps it past the stream, and then only the memory stays alive, not the
    /// protection on it (see the stream docs in `docs/streams.md`).
    pub fd: BorrowedFd<'a>,
    /// What `fd` is.
    pub kind: MemoryKind,
    /// The pool index of this plane: buffer × planes + plane. The same number the ring's frames
    /// name.
    pub index: u32,
    /// Where the plane starts in `fd`.
    pub offset: u64,
    /// The bytes the stream's contract may use in the plane: the buffer's length, or the plane's.
    pub payload_bytes: u64,
    /// The memory from `offset` that really exists: a shared pool's stride (up to the next buffer)
    /// or a dma-buf's whole size, which a heap may round up. At least `payload_bytes`.
    pub allocated_bytes: u64,
    /// Slack past the payload that a device importing the buffer must be given. Always 0 today:
    /// it will come from the board's per-stream `tail_bytes` key, dma-bufs only (I2d).
    pub tail_bytes: u64,
    /// Whether `fd` allows a writable mapping. On a mapped stream this is probed at open; on a
    /// stream taken with [`Session::open_stream_unmapped`](crate::Session::open_stream_unmapped)
    /// it is read from the descriptor's access mode and seals, with no mapping (see
    /// `docs/streams.md` for what that can and can't tell). The stream maps it writable only when
    /// it was taken with [`Session::open_stream_writable`](crate::Session::open_stream_writable).
    pub writable: bool,
}

/// What the stream knows about one pool index, probed at open.
#[derive(Debug, Clone, Copy)]
struct Slot {
    writable: bool,
    allocated: u64,
}

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

    /// Whether `fd` allows a writable shared mapping, decided from the descriptor alone, without
    /// mapping it. Used only for an unmapped stream's `Region::writable`. The default says
    /// `false`, which is the safe answer where the platform can't tell.
    fn writable_descriptor(&self, fd: BorrowedFd<'_>) -> bool {
        let _ = fd;
        false
    }

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

    /// Waits until `fd` (the stream's doorbell) is readable, or `timeout` passes. Returns `true`
    /// when it is readable. `false` when the time is up, or when a signal interrupted the wait: the
    /// caller then looks at the ring and waits again with what is left of its own deadline, so an
    /// implementation returns early on `EINTR` and must not restart the full timeout. An error is
    /// one the wait can't recover from (a descriptor that failed or hung up).
    fn wait_readable(&self, fd: BorrowedFd<'_>, timeout: Duration) -> io::Result<bool>;
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
    /// Each pool index's descriptor facts, probed at open.
    slots: Box<[Slot]>,
    /// The producer's working set, as the answer gave it.
    working: u32,
    /// Whether the frames are job results (`StreamKind::JobDriven`), as the answer gave it.
    job_driven: bool,
    platform: Arc<dyn StreamPlatform>,
    /// The session's record that this stream is open. Dropping the stream without closing it
    /// queues the close.
    hold: Option<Hold>,
}

/// The most descriptors one answer brings: a stream with a descriptor per buffer, at its limit.
pub(crate) const MAX_DESCRIPTORS: usize =
    (lemnos_wire::HANDLE_DESCRIPTORS_BEFORE + lemnos_wire::MAX_HANDLE_BUFFERS) as usize;

/// What each pool index's descriptor allows, and how big its memory is, before the stream is
/// attached. A shared pool is one descriptor, probed once; a read-only descriptor is one a
/// writable mapping of it fails on, which is the answer a writer needs.
fn probe(
    fds: &[OwnedFd],
    layout: Layout,
    lengths: &[u32],
    shared: bool,
    writable_of: &dyn Fn(BorrowedFd<'_>) -> bool,
) -> Result<Box<[Slot]>, Error> {
    let descs = layout.pool_descs(shared, lengths);
    let mut seen: [Option<Slot>; 256] = [None; 256];
    let mut out = Vec::with_capacity(descs.len());
    for desc in &descs {
        let known = match seen.get(usize::from(desc.descriptor)).copied().flatten() {
            Some(slot) => slot,
            None => {
                let fd = fds
                    .get(lemnos_wire::ring::fds::BUFFERS + usize::from(desc.descriptor))
                    .ok_or(Error::Stream(RingError::BadLayout))?;
                let slot = Slot {
                    writable: writable_of(fd.as_fd()),
                    // A shared pool's buffers sit a stride apart; a dma-buf's memory is its size.
                    allocated: if shared {
                        layout.stride()
                    } else {
                        desc.size.max(size_of(fd.as_fd()))
                    },
                };
                seen[usize::from(desc.descriptor)] = Some(slot);
                slot
            }
        };
        out.push(known);
    }
    Ok(out.into_boxed_slice())
}

/// Whether `fd` allows a writable shared mapping, by mapping it writable and unmapping it again:
/// the probe a mapped stream makes, so nothing is kept.
fn writable_fd(fd: BorrowedFd<'_>) -> bool {
    fd.try_clone_to_owned()
        .ok()
        .and_then(|owned| WriteMap::new(owned).ok())
        .is_some()
}

/// The size of `fd` as the kernel reports it, or 0 when it can't say.
fn size_of(fd: BorrowedFd<'_>) -> u64 {
    fd.try_clone_to_owned()
        .ok()
        .and_then(|owned| std::fs::File::from(owned).metadata().ok())
        .map_or(0, |meta| meta.len())
}

impl FrameStream {
    /// Builds the stream from what the answer carried: the descriptors in the order of
    /// [`lemnos_wire::ring::fds`] and then the pool's buffers, the layout, and each buffer's
    /// planes. `shared` says the pool is one descriptor; otherwise each plane of each buffer is its
    /// own dma-buf. Any failure closes every descriptor.
    ///
    /// On an unmapped stream nothing of the buffers is mapped, so `writable` is read from each
    /// descriptor by the platform, and the consumer is attached with [`Mapping::Unmapped`].
    pub(crate) fn new(
        device: u32,
        name: &str,
        (layout, planes, working, job_driven): (Layout, Planes, u32, bool),
        shared: bool,
        fds: Vec<OwnedFd>,
        mapping: Mapping,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<Self, Error> {
        let lengths: &[u32] = if planes.count() == 1 {
            &[]
        } else {
            planes.lengths()
        };
        let layout = layout.checked().map_err(Error::Stream)?;
        let slots = match mapping {
            Mapping::Unmapped => {
                let by_platform = |fd: BorrowedFd<'_>| platform.writable_descriptor(fd);
                probe(&fds, layout, lengths, shared, &by_platform)?
            }
            Mapping::ReadOnly | Mapping::Writable => {
                probe(&fds, layout, lengths, shared, &writable_fd)?
            }
        };
        let writable = mapping == Mapping::Writable;
        if writable && slots.iter().any(|slot| !slot.writable) {
            return Err(Error::StreamBuffersReadOnly);
        }
        let consumer =
            Consumer::attach_with(fds, layout, lengths, shared, mapping).map_err(Error::Stream)?;
        Ok(Self {
            device,
            name: name.to_string(),
            consumer,
            working,
            job_driven,
            dma: !shared,
            writable,
            planes,
            slots,
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

    /// Whether the buffers' memory is mapped in this process. A stream taken with
    /// [`Session::open_stream_unmapped`](crate::Session::open_stream_unmapped) isn't: its frames
    /// have no bytes here ([`bytes`](Self::bytes) and [`plane`](Self::plane) are empty), and
    /// [`read`](Self::read) and [`write`](Self::write) are refused. Its frames, lengths and
    /// [`plane_len`](Self::plane_len) are all there, and so are its regions.
    #[must_use]
    pub const fn is_mapped(&self) -> bool {
        self.consumer.mapped()
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

    /// The producer's working set: how many buffers it holds in its own hands when it asks for
    /// its next one, as the board file declares it (`docs/streams.md`, "The margin").
    #[must_use]
    pub const fn working(&self) -> u32 {
        self.working
    }

    /// Whether the stream's frames are the results of jobs this consumer posted. A job-driven
    /// stream never replaces an unread frame: the margin does not apply (`docs/streams.md`,
    /// "The guarantee").
    #[must_use]
    pub const fn job_driven(&self) -> bool {
        self.job_driven
    }

    /// How many frames this reader may fall behind before it loses one: `buffers - 1 - working`
    /// (`docs/streams.md`, "The margin"). 0 when the stream has no such room.
    #[must_use]
    pub const fn margin(&self) -> u32 {
        self.buffer_count()
            .saturating_sub(1)
            .saturating_sub(self.working)
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
    /// dma-bufs. A failure changes nothing about where the bytes are, so it isn't one here. An
    /// unmapped stream makes no CPU access, so it is never bracketed.
    fn sync_plane(&self, index: u32, access: Access, begin: bool) {
        if !self.dma || !self.consumer.mapped() {
            return;
        }
        if let Some((fd, _)) = self.consumer.descriptor(BufferIndex(index)) {
            let _ = self.platform.dma_sync(fd, access, begin);
        }
    }

    /// The next frame, if one has arrived. Never waits and allocates nothing.
    ///
    /// A frame lost before the reader got to it is passed over, not reported as nothing: a frame
    /// pending behind the losses comes back in this same call. So `None` means that no frame is
    /// pending, and a caller that sleeps on the stream's doorbell afterwards sleeps only with
    /// nothing to read (`docs/streams.md`, "Waiting on the doorbell"). The losses are counted in
    /// [`skipped`](Self::skipped).
    pub fn try_next(&mut self) -> Result<Option<HeldFrame>, Error> {
        let planes = self.planes.count();
        let buffer_bytes = self.layout().buffer_bytes as usize;
        let taken = loop {
            match self.consumer.frames_mut().next() {
                NextBatch::Batch(batch) => {
                    let info =
                        FrameInfo::decode(batch.meta).ok_or(Error::Stream(RingError::Broken))?;
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
                    break Some(HeldFrame {
                        buffer,
                        bytes: info.bytes,
                        sequence: info.sequence,
                        identity: info.identity,
                        timestamp_ns,
                        _held: held,
                    });
                }
                NextBatch::Empty => break None,
                // Lost before this reader read them, and counted by the reader: look again for the
                // frame parked behind them, or for nothing.
                NextBatch::Skipped(_) | NextBatch::Limited => {}
                NextBatch::PoolChanged { .. } | NextBatch::CutOff => {
                    return Err(Error::Stream(RingError::Broken));
                }
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

    /// How many frames this stream's reader was too late for, in total: frames overwritten in the
    /// ring, or whose buffers the producer took back, before a read got to them. A reader that
    /// keeps up never counts one. [`try_next`](Self::try_next) and [`next_frame`](Self::next_frame)
    /// pass them over and count them here.
    #[must_use]
    pub fn skipped(&self) -> u64 {
        self.consumer.frames().missed()
    }

    /// The next frame, or `None` after `timeout` passes or the host ends the stream.
    ///
    /// The wait is on the stream's doorbell and lasts at most `timeout` in all: a signal that
    /// interrupts a sleep ends it early, and the ring is looked at again with what is left of the
    /// time. An error from the wait itself is returned as an error, never as no frame.
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
            self.sleep_for_frame(left)?;
        }
    }

    /// Sleeps on the doorbell for at most `timeout`, unless a frame is already there. The doorbell
    /// is armed first and the ring is looked at once more, so a frame published before the sleep
    /// is not slept through. The doorbell is disarmed on every way out.
    fn sleep_for_frame(&mut self, timeout: Duration) -> Result<(), Error> {
        self.consumer.doorbell().arm();
        if self.frame_waiting() {
            self.consumer.doorbell().disarm();
            return Ok(());
        }
        let woke = self
            .platform
            .wait_readable(self.consumer.doorbell().as_fd(), timeout);
        self.consumer.doorbell().disarm();
        woke.map(|_| ())
            .map_err(|error| Error::StreamWait(error.kind()))
    }

    /// Whether the ring has an entry the reader has not looked at yet, or the stream has ended.
    fn frame_waiting(&self) -> bool {
        let frames = self.consumer.frames();
        frames.pending() > 0 || frames.end() != 0
    }

    /// The bytes of a frame this stream handed out. On a stream whose buffers are several
    /// planes this is the first plane's part of the frame: read each with
    /// [`plane`](Self::plane). A frame from another stream reads as whatever its numbers fall
    /// on here, and within this stream's memory. Empty on an unmapped stream.
    #[must_use]
    pub fn bytes(&self, frame: &HeldFrame) -> &[u8] {
        self.plane(frame, 0)
    }

    /// The bytes of the held frame that are in plane `plane` of its buffer. The planes hold the
    /// frame's bytes end to end, so a plane past the frame's length is empty, and one it ends in
    /// is cut there. Empty for a plane the buffers don't have, and on an unmapped stream
    /// ([`is_mapped`](Self::is_mapped)). On a stream of one plane this is
    /// [`bytes`](Self::bytes).
    #[must_use]
    pub fn plane(&self, frame: &HeldFrame, plane: u32) -> &[u8] {
        if !self.consumer.mapped() {
            return &[];
        }
        let Some(index) = self.index(frame.buffer, plane) else {
            return &[];
        };
        let held = self.plane_len(frame, plane);
        self.consumer
            .frames()
            .pool()
            .bytes(BufferIndex(index))
            .and_then(|bytes| bytes.get(..held))
            .unwrap_or(&[])
    }

    /// The bytes of the held frame that are in plane `plane` of its buffer: the length
    /// [`plane`](Self::plane) returns, without touching any mapping, so it works on a stream
    /// taken with [`Session::open_stream_unmapped`](crate::Session::open_stream_unmapped). 0 for
    /// a plane past the frame's end, and for a plane the stream doesn't have.
    #[must_use]
    pub fn plane_len(&self, frame: &HeldFrame, plane: u32) -> usize {
        if plane >= self.planes.count() {
            return 0;
        }
        let (Some(start), Some(length)) = (
            self.planes.offset(plane),
            self.planes.lengths().get(plane as usize),
        ) else {
            return 0;
        };
        let held = (frame.bytes as usize).saturating_sub(start);
        if held < *length as usize {
            held
        } else {
            *length as usize
        }
    }

    /// The memory of each buffer and plane: the descriptor, where it starts, and what it allows.
    /// `None` for a buffer or plane the stream doesn't have. Lives as long as the stream; see
    /// [`Region`] for the rules that apply to a copy of the descriptor.
    #[must_use]
    pub fn region(&self, buffer: u32, plane: u32) -> Option<Region<'_>> {
        let index = self.index(buffer, plane)?;
        let (fd, desc) = self.consumer.descriptor(BufferIndex(index))?;
        let slot = self.slots.get(index as usize)?;
        let payload = *self.planes.lengths().get(plane as usize)?;
        Some(Region {
            fd,
            kind: if self.dma {
                MemoryKind::DmaBuf
            } else {
                MemoryKind::SharedMemory
            },
            index,
            offset: desc.offset,
            payload_bytes: u64::from(payload),
            allocated_bytes: slot.allocated,
            tail_bytes: 0,
            writable: slot.writable,
        })
    }

    /// The pool page's descriptor, read-only, dup'd: with the descriptors of [`region`](Self::region)
    /// it is everything another reader of this pool needs (see `shm_ring_buffers::PoolView::attach`).
    /// That reader holds no reader slot, so it can read frames it is given but must not mark them;
    /// its protection against reuse is only the hold this stream's frames have. Once this stream
    /// is closed, a copy keeps the memory alive but nothing protects it from reuse.
    pub fn pool_page(&self) -> io::Result<OwnedFd> {
        self.consumer.pool_page().try_clone()
    }

    /// The pool's generation, as the host stamped its page: it changes whenever the host makes a
    /// new pool for this stream (a reopen, a mode change that reallocates). A lease with an older
    /// generation is on a pool that is gone.
    #[must_use]
    pub fn generation(&self) -> u32 {
        self.consumer.frames().pool().generation()
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
        self.region(buffer, plane).map(|region| region.fd)
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
        if !self.consumer.mapped() {
            return Err(Error::StreamUnmapped);
        }
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
        if !self.consumer.mapped() {
            return Err(Error::StreamUnmapped);
        }
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
