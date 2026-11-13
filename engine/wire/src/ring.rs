//! The shared memory of a frame stream, as the shm family lays it out (`docs/ring.md`). A stream is
//! four shm objects and two small pages, and this module says which is which and how each is sized:
//!
//! | Object | Shm type | Written by | Read by |
//! | --- | --- | --- | --- |
//! | frames | `shm_ring::Ring`, one field, a frame's metadata as its payload | host | consumer |
//! | pool | `shm_ring_buffers::Pool`, the buffers, one descriptor per plane of each buffer | host (and consumer, for a stream it fills) | consumer |
//! | holds | a page of `shm_ring_holds::Marks`, one per consumer | consumer | host |
//! | doorbell | `shm_ring_doorbell::Doorbell`: an eventfd and a page | consumer (page), host (eventfd) | consumer |
//! | jobs | a second `shm_ring::Ring`, one field, a job's bytes as payload | consumer | host |
//! | job acks | one word: how many jobs the host has collected | host | consumer |
//!
//! The host makes every object and hands the consumer the descriptors in the order of [`fds`]. A
//! consumer that only has the shm crates and these descriptors can read the frames: see
//! `docs/streams.md#reading-with-the-shm-crates-alone`.
//!
//! A frame's buffers are named by pool index: buffer `b`'s plane `p` is pool buffer `b × planes + p`,
//! so a frame of several planes is an entry naming several pool buffers. Nothing here reads what a
//! frame or job carries: the contract says what the bytes mean.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use shm_ring::{
    FieldId, Flags, Mem, MemMut, ReadMap, RingError as ShmError, SampleTime, Spec, WriteMap,
};
use shm_ring_buffers::{BufferDesc, BufferIndex};

constants! {
/// The most buffers one stream has: what one pool and one set of holds can name.
pub const MAX_BUFFERS: u32 = shm_ring_buffers::MAX_BUFFERS;
/// The largest buffer.
pub const MAX_BUFFER_BYTES: u32 = 1 << 28;
/// The most buffer memory one stream maps.
pub const MAX_POOL_BYTES: u64 = 1 << 34;
/// The most jobs waiting at once.
pub const MAX_JOB_SLOTS: u32 = 64;
/// The largest job.
pub const MAX_JOB_BYTES: u32 = 4096;
/// Buffers start on multiples of this inside a shared pool.
pub const BUFFER_ALIGN: u64 = 4096;
/// Bytes of a frame's metadata: sequence, identity and length.
pub const FRAME_BYTES: usize = 20;
/// Pool buffers one frame may name: one per plane, at most `VIDEO_MAX_PLANES` (8).
pub const MAX_PLANES: usize = shm_ring_buffers::MAX_PER_FRAME;

/// The stream's end codes, stored in the frame ring's end word. Zero while the stream runs.
/// The host ends a stream the consumer may still be reading.
pub const STATE_OPEN: u32 = 0;
/// The stream ended normally.
pub const STATE_CLOSED: u32 = 1;
/// The stream ended because the consumer broke the rules.
pub const STATE_FAULT: u32 = 2;
}

/// Where each descriptor of a stream is in the order the host passes them: the frame ring, the
/// pool page, the job ring, the job acks, the doorbell's eventfd, its page, the holds page, and then
/// the pool's buffers, one descriptor per plane of each buffer (shared memory is one descriptor).
pub mod fds {
    constants! {
    pub const RING: usize = 0;
    pub const POOL_PAGE: usize = 1;
    pub const JOB_RING: usize = 2;
    pub const JOB_ACKS: usize = 3;
    pub const BELL: usize = 4;
    pub const BELL_PAGE: usize = 5;
    pub const MARKS: usize = 6;
    /// The first buffer descriptor.
    pub const BUFFERS: usize = 7;
    /// How many descriptors come before the buffers'.
    pub const BEFORE: usize = BUFFERS;
    }
}

/// The error for a ring that doesn't hold what the layout says, a job that doesn't fit, or a
/// shm operation that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RingError {
    /// The layout is outside the limits.
    BadLayout,
    /// A job is larger than the layout allows.
    JobTooLarge,
    /// The peer broke the rules: it skipped jobs it had no right to lose, or sent a frame or a
    /// buffer that can't be right.
    Broken,
    /// A buffer number outside the stream.
    BadBuffer,
    /// A length or range outside a buffer.
    BadLength,
    /// The shared memory isn't the one the layout describes; the shm crate's message.
    Shm(String),
}

impl core::fmt::Display for RingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadLayout => f.write_str("the stream layout is out of range"),
            Self::JobTooLarge => f.write_str("a job too large for its destination"),
            Self::Broken => f.write_str("the stream's peer is malformed"),
            Self::BadBuffer => f.write_str("a buffer number outside the stream"),
            Self::BadLength => f.write_str("a length out of range"),
            Self::Shm(error) => write!(f, "shared memory: {error}"),
        }
    }
}

impl std::error::Error for RingError {}

impl From<std::io::Error> for RingError {
    fn from(error: std::io::Error) -> Self {
        Self::Shm(error.to_string())
    }
}

impl From<ShmError> for RingError {
    fn from(error: ShmError) -> Self {
        Self::Shm(error.to_string())
    }
}

/// What a stream's rings and buffers look like. Both sides hold their own copy: the host made it,
/// and the consumer was given it with the descriptors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub buffers: u32,
    pub buffer_bytes: u32,
    /// The frame ring's history: at least `buffers`, a power of two.
    pub frame_slots: u32,
    pub job_slots: u32,
    pub job_bytes: u32,
}

impl Layout {
    /// A layout with enough frame slots that a frame can only be lost when its reader stops.
    pub fn new(
        buffers: u32,
        buffer_bytes: u32,
        job_slots: u32,
        job_bytes: u32,
    ) -> Result<Self, RingError> {
        Self {
            buffers,
            buffer_bytes,
            frame_slots: buffers.checked_next_power_of_two().unwrap_or(0).max(2),
            job_slots: job_slots.checked_next_power_of_two().unwrap_or(0).max(2),
            job_bytes,
        }
        .checked()
    }

    /// Checks limits, for a layout that came from a peer or a file.
    pub fn checked(self) -> Result<Self, RingError> {
        let bad = self.buffers == 0
            || self.buffers > MAX_BUFFERS
            || self.buffer_bytes == 0
            || self.buffer_bytes > MAX_BUFFER_BYTES
            || !self.frame_slots.is_power_of_two()
            || self.frame_slots < self.buffers
            || self.frame_slots < 2
            || !self.job_slots.is_power_of_two()
            || self.job_slots < 2
            || self.job_slots > MAX_JOB_SLOTS
            || self.job_bytes == 0
            || self.job_bytes > MAX_JOB_BYTES
            || self.pool_bytes() > MAX_POOL_BYTES;
        if bad {
            Err(RingError::BadLayout)
        } else {
            Ok(self)
        }
    }

    /// Distance between buffers in a shared pool.
    #[must_use]
    pub const fn stride(&self) -> u64 {
        (self.buffer_bytes as u64).next_multiple_of(BUFFER_ALIGN)
    }

    /// Bytes a shared pool holds.
    #[must_use]
    pub const fn pool_bytes(&self) -> u64 {
        self.buffers as u64 * self.stride()
    }

    /// The shape of the frame ring: one field, whose payloads are buffer entries naming up to
    /// [`MAX_PLANES`] pool buffers and carrying a frame's metadata.
    #[must_use]
    pub const fn frame_spec(&self) -> Spec {
        Spec {
            fields: 1,
            history: self.frame_slots,
            max_payload: shm_ring_buffers::entry::len(MAX_PLANES, FRAME_BYTES) as u32,
            expiry_ms: 0,
        }
    }

    /// The shape of the job ring: one field, a job as its payload.
    #[must_use]
    pub const fn job_spec(&self) -> Spec {
        Spec {
            fields: 1,
            history: self.job_slots,
            max_payload: self.job_bytes,
            expiry_ms: 0,
        }
    }

    /// The pool buffers' descriptions, in pool order, for the consumer to attach with. `planes` is
    /// each plane's length, empty for one plane of a whole stride. A shared pool (`shared`) is one
    /// descriptor with every buffer at its stride; otherwise each plane of each buffer is its own
    /// descriptor, and pool buffer `b × planes + p` is buffer `b`'s plane `p` (see [`pool_index`]).
    #[must_use]
    pub fn pool_descs(&self, shared: bool, planes: &[u32]) -> Vec<BufferDesc> {
        let count = planes.len().max(1);
        let mut out = Vec::with_capacity(self.buffers as usize * count);
        for buffer in 0..self.buffers {
            for plane in 0..count {
                out.push(if shared {
                    BufferDesc {
                        descriptor: 0,
                        offset: u64::from(buffer) * self.stride(),
                        size: u64::from(self.buffer_bytes),
                    }
                } else {
                    BufferDesc {
                        descriptor: pool_index(buffer, plane as u32, count as u32) as u8,
                        offset: 0,
                        size: match planes.get(plane) {
                            Some(length) => u64::from(*length),
                            None => self.stride(),
                        },
                    }
                });
            }
        }
        out
    }
}

/// The index in the pool of plane `plane` of buffer `buffer`, for a stream of `planes` planes.
#[must_use]
pub const fn pool_index(buffer: u32, plane: u32, planes: u32) -> u32 {
    buffer * planes + plane
}

/// One frame a consumer took with [`Consumer::pop_frame`]: its buffer, and its metadata. The
/// buffer is held until [`Consumer::release`] gives it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSlot {
    /// The buffer, as the producer numbers it (a plane's pool buffers are its buffer's).
    pub buffer: u32,
    /// The frame's length, across its planes.
    pub bytes: u32,
    pub sequence: u64,
    /// Means something only to the producer and the consumer.
    pub identity: u64,
    /// `CLOCK_MONOTONIC` nanoseconds.
    pub timestamp_ns: u64,
}

/// One frame's metadata, as the frame ring's payload carries it. The frame's buffers are the
/// entry's, not this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    /// Counts from the producer's start, one per frame.
    pub sequence: u64,
    /// Means something only to the producer and the consumer.
    pub identity: u64,
    /// The frame's length, across its planes.
    pub bytes: u32,
}

impl FrameInfo {
    #[must_use]
    pub fn encode(self) -> [u8; FRAME_BYTES] {
        let mut out = [0_u8; FRAME_BYTES];
        out[0..8].copy_from_slice(&self.sequence.to_le_bytes());
        out[8..16].copy_from_slice(&self.identity.to_le_bytes());
        out[16..20].copy_from_slice(&self.bytes.to_le_bytes());
        out
    }

    /// The metadata in `bytes`, or `None` if it is not exactly one frame's.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; FRAME_BYTES] = bytes.try_into().ok()?;
        Some(Self {
            sequence: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            identity: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
            bytes: u32::from_le_bytes(bytes[16..20].try_into().ok()?),
        })
    }
}

/// The page that says how many jobs the host has collected: one word, written by the host.
pub const JOB_ACK_BYTES: usize = 8;

/// The consumer's end of the job ring: posts a job, or says the ring is full. Never waits.
pub struct JobPoster {
    ring: shm_ring::Writer<WriteMap>,
    acks: ReadMap,
    job_bytes: u32,
    slots: u32,
}

impl core::fmt::Debug for JobPoster {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("JobPoster")
            .field("slots", &self.slots)
            .field("job_bytes", &self.job_bytes)
            .finish_non_exhaustive()
    }
}

impl JobPoster {
    /// Takes the write end of the job ring and the job-ack page, read-only.
    pub fn new(ring: OwnedFd, acks: OwnedFd, layout: &Layout) -> Result<Self, RingError> {
        let ring = shm_ring::Writer::attach(ring)?;
        let acks = ReadMap::new(acks)?;
        if ring.geometry().history() != layout.job_slots || acks.words() < 1 {
            return Err(RingError::BadLayout);
        }
        Ok(Self {
            ring,
            acks,
            job_bytes: layout.job_bytes,
            slots: layout.job_slots,
        })
    }

    /// Posts `job`. `false`, and nothing posted, when `job_slots` jobs are waiting, because the
    /// host has not collected them yet. A job longer than the layout allows is an error.
    pub fn post(&mut self, job: &[u8]) -> Result<bool, RingError> {
        if job.len() > self.job_bytes as usize {
            return Err(RingError::JobTooLarge);
        }
        let collected = self.acks.load(0, std::sync::atomic::Ordering::Acquire);
        let waiting = self.ring.published().saturating_sub(collected);
        if waiting >= u64::from(self.slots) {
            return Ok(false);
        }
        self.ring
            .publish(FieldId(0), SampleTime::ZERO, Flags::NONE, job)
            .map_err(|_| RingError::BadLayout)?;
        Ok(true)
    }
}

/// The host's end of the job ring: takes each job in order and tells the consumer how many it
/// has collected.
pub struct JobCollector {
    reader: shm_ring::Reader<ReadMap>,
    acks: WriteMap,
    collected: u64,
    job_bytes: u32,
}

impl core::fmt::Debug for JobCollector {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("JobCollector")
            .field("collected", &self.collected)
            .finish_non_exhaustive()
    }
}

impl JobCollector {
    /// Takes the read end of the job ring and the job-ack page, writable.
    pub fn new(ring: OwnedFd, acks: OwnedFd, layout: &Layout) -> Result<Self, RingError> {
        let reader = shm_ring::Reader::attach(ring)?;
        let acks = WriteMap::new(acks)?;
        if reader.geometry().history() != layout.job_slots || acks.words() < 1 {
            return Err(RingError::BadLayout);
        }
        Ok(Self {
            reader,
            acks,
            collected: 0,
            job_bytes: layout.job_bytes,
        })
    }

    /// The next job in `out`, its length; `None` if there is none. A job the reader lost (the
    /// consumer posted more than the ring holds) is [`RingError::Broken`]: the consumer broke the
    /// rules, and nothing it posted after the lost job can be trusted in order.
    pub fn next(&mut self, out: &mut [u8]) -> Result<Option<usize>, RingError> {
        match self.reader.next() {
            shm_ring::Next::Empty => Ok(None),
            shm_ring::Next::Skipped(_) => Err(RingError::Broken),
            shm_ring::Next::Sample(sample) => {
                let len = sample.payload.len();
                if len > self.job_bytes as usize || len > out.len() {
                    return Err(RingError::JobTooLarge);
                }
                out[..len].copy_from_slice(sample.payload);
                self.collected += 1;
                use std::sync::atomic::Ordering;
                self.acks.store(0, self.collected, Ordering::Release);
                Ok(Some(len))
            }
        }
    }
}

/// The consumer's end of a stream, assembled from the descriptors the host passed in the order of
/// [`fds`]. Every mapping is checked by the shm crates when it is made; nothing here reads a value
/// from the shared memory that the layout doesn't already give.
///
/// A consumer reads frames with [`frames`](Self::frames), returns buffers by dropping their held
/// frames, posts jobs with [`jobs`](Self::jobs), and waits with [`doorbell`](Self::doorbell).
pub struct Consumer {
    layout: Layout,
    /// The pool page as the host passed it (read-only), kept so it can be handed on to another
    /// reader. The pool's own view has a mapping of it, not the descriptor.
    pool_page: OwnedFd,
    /// Whether the buffers' memory is mapped here. An unmapped consumer's pool view maps stand-in
    /// memory of the same shape instead (see [`Mapping::Unmapped`]), so its bytes are never read.
    mapped: bool,
    /// The buffers' descriptors, as the host passed them, kept by an unmapped consumer so that it
    /// can hand them on. A mapped consumer's view owns them, and this is empty.
    pool_fds: Vec<OwnedFd>,
    frames: shm_ring_buffers::BufferReader<ReadMap, shm_ring_holds::Marks<WriteMap>>,
    /// The frames [`pop_frame`](Self::pop_frame) took and nobody has released, by buffer.
    held: Vec<Option<shm_ring_buffers::Held<shm_ring_holds::Marks<WriteMap>>>>,
    /// How many planes each buffer has: one, or each plane's pool buffer.
    planes: u32,
    /// The buffers a consumer that fills them maps writable, apart from the reader's own view.
    fills: Option<shm_ring_buffers::PoolView>,
    jobs: JobPoster,
    bell: shm_ring_doorbell::Doorbell,
}

impl core::fmt::Debug for Consumer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Consumer")
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}

/// How a consumer maps a stream's buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mapping {
    /// Read-only: frames are read in place.
    ReadOnly,
    /// Writable as well, for a stream the consumer fills.
    Writable,
    /// Not at all. The buffers' memory is never mapped by this consumer: its descriptors are kept
    /// and handed on as they are. The rings, the holds, the pool page and the doorbell are mapped
    /// as the protocol needs them. The pool view the frame reader holds maps stand-in memory of
    /// the same shape, a sealed memfd as large as each descriptor's buffers, which nothing writes
    /// or reads; [`Consumer::read`] and [`Consumer::mapped`] keep it from being read.
    Unmapped,
}

impl Consumer {
    /// Attaches to a stream from its descriptors, in the order of [`fds`] followed by the pool's
    /// buffers. `planes` are each plane's length (empty for one plane of a whole stride), and
    /// `shared` says the pool is one descriptor. `writable` maps the buffers writable too, for a
    /// stream the consumer fills.
    ///
    /// # Errors
    ///
    /// [`RingError`] if the descriptors are the wrong number or can't be mapped as the layout
    /// says, or the rings aren't the ones the layout describes.
    pub fn attach(
        descriptors: Vec<OwnedFd>,
        layout: Layout,
        planes: &[u32],
        shared: bool,
        writable: bool,
    ) -> Result<Self, RingError> {
        let mapping = if writable {
            Mapping::Writable
        } else {
            Mapping::ReadOnly
        };
        Self::attach_with(descriptors, layout, planes, shared, mapping)
    }

    /// As [`attach`](Self::attach), with how the buffers are mapped: see [`Mapping`].
    ///
    /// # Errors
    ///
    /// As [`attach`](Self::attach).
    pub fn attach_with(
        descriptors: Vec<OwnedFd>,
        layout: Layout,
        planes: &[u32],
        shared: bool,
        mapping: Mapping,
    ) -> Result<Self, RingError> {
        let layout = layout.checked()?;
        let descs = layout.pool_descs(shared, planes);
        let pool_fds = if shared { 1 } else { descs.len() };
        if descriptors.len() != fds::BUFFERS + pool_fds {
            return Err(RingError::BadLayout);
        }
        let mut descriptors = descriptors.into_iter();
        let mut next = || descriptors.next().ok_or(RingError::BadLayout);
        let ring = next()?;
        let pool_page = next()?;
        let job_ring = next()?;
        let job_acks = next()?;
        let bell_fd = next()?;
        let bell_page = next()?;
        let marks = next()?;
        let buffer_fds: Vec<OwnedFd> = descriptors.collect();

        // The ring is new for this attachment, so the consumer reads from its first sample: a
        // frame the host published on standby before this consumer could map the ring is still
        // there.
        let reader = shm_ring::Reader::attach_from_start(ring)?;
        if reader.geometry().history() != layout.frame_slots {
            return Err(RingError::BadLayout);
        }
        let kept = clone_owned(&pool_page)?;
        let (fills, view, kept_fds) = match mapping {
            Mapping::Writable => {
                let fills = shm_ring_buffers::PoolView::attach_writable(
                    clone_owned(&pool_page)?,
                    clone_all(&buffer_fds)?,
                    &descs,
                )?;
                let view =
                    shm_ring_buffers::PoolView::attach_writable(pool_page, buffer_fds, &descs)?;
                (Some(fills), view, Vec::new())
            }
            Mapping::ReadOnly => (
                None,
                shm_ring_buffers::PoolView::attach(pool_page, buffer_fds, &descs)?,
                Vec::new(),
            ),
            Mapping::Unmapped => {
                // The view needs descriptors it can map, and each must be as large as the buffers
                // reach into it. One sealed memfd of the largest reach serves for every one.
                let reach = descs
                    .iter()
                    .map(|desc| desc.offset.checked_add(desc.size))
                    .try_fold(0_u64, |most, end| end.map(|end| most.max(end)))
                    .and_then(|most| usize::try_from(most).ok())
                    .ok_or(RingError::BadLayout)?;
                let stand_in = shm_ring::sealed_memfd(reach)?;
                let stand_ins = (0..pool_fds)
                    .map(|_| clone_owned(&stand_in))
                    .collect::<Result<Vec<_>, _>>()?;
                let view = shm_ring_buffers::PoolView::attach(pool_page, stand_ins, &descs)?;
                (None, view, buffer_fds)
            }
        };
        let mapped = mapping != Mapping::Unmapped;
        let marks = shm_ring_holds::Marks::over(
            WriteMap::new(marks)?,
            layout.buffers * planes.len().max(1) as u32,
            0,
            0,
        );
        let frames = shm_ring_buffers::BufferReader::new(reader, FieldId(0), view, marks);
        let jobs = JobPoster::new(job_ring, job_acks, &layout)?;
        let bell = shm_ring_doorbell::Doorbell::attach(bell_fd, bell_page)?;
        Ok(Self {
            pool_page: kept,
            mapped,
            pool_fds: kept_fds,
            held: (0..layout.buffers).map(|_| None).collect(),
            planes: planes.len().max(1) as u32,
            layout,
            frames,
            fills,
            jobs,
            bell,
        })
    }

    /// The layout the stream was attached with.
    #[must_use]
    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Takes the next frame and holds its buffer until [`release`](Self::release) gives it back.
    /// `Ok(None)` when no frame is waiting. Frames the ring lost before the consumer got to them
    /// are skipped, as [`shm_ring_buffers::BufferReader::next`] does.
    ///
    /// # Errors
    ///
    /// [`RingError::Broken`] if the frame's metadata or buffers can't be right, or the topic
    /// ended.
    pub fn pop_frame(&mut self) -> Result<Option<FrameSlot>, RingError> {
        let planes = self.planes;
        let buffer_bytes = self.layout.buffer_bytes;
        let taken = match self.frames.next() {
            shm_ring_buffers::NextBatch::Batch(batch) => {
                let info = FrameInfo::decode(batch.meta).ok_or(RingError::Broken)?;
                let first = batch.held.buffer(0).ok_or(RingError::Broken)?;
                let buffer = first.0 / planes;
                if info.bytes > buffer_bytes * planes {
                    return Err(RingError::Broken);
                }
                let slot = FrameSlot {
                    buffer,
                    bytes: info.bytes,
                    sequence: info.sequence,
                    identity: info.identity,
                    timestamp_ns: batch.time.as_nanos(),
                };
                let held = batch.held;
                if let Some(place) = self.held.get_mut(buffer as usize) {
                    *place = Some(held);
                }
                Some(slot)
            }
            shm_ring_buffers::NextBatch::Empty
            | shm_ring_buffers::NextBatch::Skipped(_)
            | shm_ring_buffers::NextBatch::Limited => None,
            shm_ring_buffers::NextBatch::PoolChanged { .. }
            | shm_ring_buffers::NextBatch::CutOff => return Err(RingError::Broken),
        };
        Ok(taken)
    }

    /// Copies `data` into whole buffer `buffer` at `offset`, on a stream attached writable, for a
    /// buffer the consumer fills. `false` if the stream isn't writable, or the range isn't in the
    /// buffer. A stream of one plane per buffer; see [`crate::ring`] for planes.
    pub fn write(&mut self, buffer: u32, offset: usize, data: &[u8]) -> bool {
        let Some(fills) = self.fills.as_mut() else {
            return false;
        };
        fills
            .bytes_mut(BufferIndex(buffer))
            .and_then(|bytes| bytes.get_mut(offset..offset.checked_add(data.len())?))
            .map(|dst| dst.copy_from_slice(data))
            .is_some()
    }

    /// Whether this consumer maps the buffers' memory. An unmapped one ([`Mapping::Unmapped`])
    /// reads and writes nothing through [`read`](Self::read) and [`write`](Self::write), and its
    /// frames' bytes are not its to read.
    #[must_use]
    pub const fn mapped(&self) -> bool {
        self.mapped
    }

    /// The descriptor of the pool's buffer `buffer`, and where the buffer is in it. For an
    /// unmapped consumer this is the descriptor the host passed, not the stand-in its view maps.
    #[must_use]
    pub fn descriptor(&self, buffer: BufferIndex) -> Option<(BorrowedFd<'_>, BufferDesc)> {
        if self.mapped {
            return self.frames.pool().descriptor(buffer);
        }
        let (_, desc) = self.frames.pool().descriptor(buffer)?;
        Some((
            self.pool_fds.get(usize::from(desc.descriptor))?.as_fd(),
            desc,
        ))
    }

    /// Copies from whole buffer `buffer` at `offset` into `out`, for a buffer that isn't a frame.
    /// `false` if the range isn't in the buffer, or this consumer doesn't map the buffers. A
    /// stream of one plane per buffer.
    #[must_use]
    pub fn read(&self, buffer: u32, offset: usize, out: &mut [u8]) -> bool {
        if !self.mapped {
            return false;
        }
        self.frames
            .pool()
            .bytes(BufferIndex(buffer))
            .and_then(|bytes| bytes.get(offset..offset.checked_add(out.len())?))
            .map(|src| out.copy_from_slice(src))
            .is_some()
    }

    /// Gives back a buffer whose frame [`pop_frame`](Self::pop_frame) took. `false` if this
    /// consumer doesn't hold it.
    pub fn release(&mut self, buffer: u32) -> bool {
        self.held
            .get_mut(buffer as usize)
            .and_then(Option::take)
            .is_some()
    }

    /// The buffer reader of the frame ring. Its held frames give their buffers back when dropped.
    #[must_use]
    pub const fn frames(
        &self,
    ) -> &shm_ring_buffers::BufferReader<ReadMap, shm_ring_holds::Marks<WriteMap>> {
        &self.frames
    }

    /// The buffer reader of the frame ring, to read frames from.
    pub fn frames_mut(
        &mut self,
    ) -> &mut shm_ring_buffers::BufferReader<ReadMap, shm_ring_holds::Marks<WriteMap>> {
        &mut self.frames
    }

    /// The pool page descriptor, read-only, as the host passed it: a second reader attaches with it
    /// and the pool's descriptors (see `PoolView::attach`).
    #[must_use]
    pub const fn pool_page(&self) -> &OwnedFd {
        &self.pool_page
    }

    /// The writable view of the pool, for a stream the consumer fills; `None` otherwise.
    pub fn fills(&mut self) -> Option<&mut shm_ring_buffers::PoolView> {
        self.fills.as_mut()
    }

    /// Sleeps until a frame may be ready, the stream ends, or `timeout` passes. `Ok(false)` only
    /// for the timeout: a frame may still be there after `true`, and a caller reads again.
    ///
    /// # Errors
    ///
    /// If the poll on the doorbell fails.
    pub fn wait(&mut self, timeout: std::time::Duration) -> std::io::Result<bool> {
        let frames = &self.frames;
        let woke = self
            .bell
            .wait(Some(timeout), || frames.pending() > 0 || frames.end() != 0)?;
        Ok(!matches!(woke, shm_ring_doorbell::Wake::TimedOut))
    }

    /// Posts jobs. See [`JobPoster::post`].
    pub fn jobs(&mut self) -> &mut JobPoster {
        &mut self.jobs
    }

    /// The doorbell the host rings after each frame, for a consumer that sleeps until one comes.
    pub fn doorbell(&mut self) -> &mut shm_ring_doorbell::Doorbell {
        &mut self.bell
    }

    /// Why the stream ended, as its end word says: [`STATE_OPEN`] while it runs.
    #[must_use]
    pub fn state(&self) -> u32 {
        self.frames.end()
    }
}

fn clone_owned(fd: &OwnedFd) -> Result<OwnedFd, RingError> {
    Ok(fd.try_clone().map_err(ShmError::from)?)
}

fn clone_all(fds: &[OwnedFd]) -> Result<Vec<OwnedFd>, RingError> {
    fds.iter().map(clone_owned).collect()
}
