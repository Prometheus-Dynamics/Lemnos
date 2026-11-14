//! The producer's end of a stream, and the state both ends share under one lock.
//!
//! The memory of one attachment ([`Memory`]) is the shm family's: a frame ring and a job ring the
//! host owns, and a buffer pool it publishes into. The consumer's end ([`Lease`]) is the descriptors
//! of those, plus a hold page and a doorbell the host makes for it. What the consumer gives back is
//! read from its hold marks, never from the shared memory's words.

use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex, PoisonError};

use lemnos_core::{CallerId, Status};
use lemnos_hal::blocks::{BufferHandle, BufferKind, StreamFrame};
use lemnos_hal::{ErrorKind, StreamEndpoint};
use lemnos_wire::Planes;
use lemnos_wire::ring::{
    FrameInfo, JOB_ACK_BYTES, JobCollector, Layout, ProducerWords, RingError, STATE_CLOSED,
    STATE_FAULT, fds, producer,
};
use shm_ring::{FieldId, ReadMap, Ring, SampleTime, WriteMap, read_only, sealed_memfd};
use shm_ring_buffers::{BufferIndex, BufferWriter as Publisher, Holders, MAX_PER_FRAME, Pool};
use shm_ring_doorbell::{Bell, Doorbell};
use shm_ring_holds::{HoldTable, Limits, Marks};

use super::pool::{BufferPool, BufferState};
use super::{Access, Buffers, DmaSync, Lease, StreamAllocator, StreamStats};
use crate::board::{StreamKind, StreamMemory, StreamSpec};

/// What one attachment's memory is: the publisher over the frame ring and the pool, the job ring
/// and its acks, and the dma-bufs when the buffers are dma-bufs. Made once and, on standby, kept
/// until a consumer attaches.
struct Memory {
    publisher: Publisher<WriteMap>,
    ring: Ring,
    jobs: Ring,
    /// The job-ack page: how many jobs the host has collected, and the producer's state. Writable here.
    acks: OwnedFd,
    /// One dma-buf per plane of each buffer, when the buffers are dma-bufs.
    handles: Option<Arc<[OwnedFd]>>,
    sync: Option<Arc<dyn DmaSync>>,
}

/// A consumer's attachment: the memory it reads from, and what the host reads back from it.
struct Attached {
    caller: CallerId,
    memory: Memory,
    holds: HoldTable<ReadMap>,
    jobs: JobCollector,
    bell: Bell,
}

/// Counts that aren't buffer states.
#[derive(Default)]
struct Counters {
    attachments: u64,
    published: u64,
    delivered: u64,
    dropped: u64,
    returned: u64,
    faults: u64,
    /// Asks for a buffer that found none, on a job-driven stream: the job did not start.
    refused: u64,
}

pub(crate) struct StreamCore {
    layout: Layout,
    /// The producer's working set the board file declares (`StreamSpec::working`).
    working: u32,
    /// Who makes the frames (`StreamSpec::kind`), which decides what the producer may take back.
    kind: StreamKind,
    /// What the board file says the buffers are, and the heap dma-bufs come from.
    memory: StreamMemory,
    heap: Box<str>,
    /// Each buffer's planes.
    planes: Planes,
    /// The stream's name, which the allocator labels memory with.
    name: Box<str>,
    allocator: Arc<dyn StreamAllocator>,
    /// Whether memory is kept in place while no consumer is attached.
    standby: bool,
    pool: BufferPool,
    attached: Option<Attached>,
    /// On standby with no consumer attached: the memory the next one gets.
    waiting: Option<Memory>,
    /// The newest frame published onto `waiting`, for the next consumer.
    kept: Option<StreamFrame>,
    /// Making standby memory failed since a consumer last left: it is made at the next
    /// attachment instead, and tried again after that consumer leaves.
    standby_failed: bool,
    /// The host is going: no more memory is made.
    closed: bool,
    claimed: bool,
    /// Up by one whenever the memory in place changes (a consumer's, made for it or taken over
    /// from standby, or standby memory made), so a buffer granted before is known to be on
    /// memory it no longer has.
    epoch: u64,
    /// The pool generation the next memory made is stamped with (the pool page's word, which a
    /// reader gets with its descriptors): up by one for each memory made, so a pool replaced
    /// by another, on a reopen or a mode change, is recognisably a different pool.
    generation: u32,
    /// Publish order of each buffer's newest frame, so the oldest unheld one is taken back first.
    published_at: Box<[u64]>,
    /// Frames published so far on this stream.
    published_count: u64,
    counters: Counters,
    /// What the stream's producing device is doing, written into each attachment's page.
    producer: ProducerWords,
    /// The device was bound again while retrying, so what it recovers to is a new instance.
    producer_lost: bool,
}

pub(super) fn lock(core: &Mutex<StreamCore>) -> std::sync::MutexGuard<'_, StreamCore> {
    core.lock().unwrap_or_else(PoisonError::into_inner)
}

fn io_status(_error: impl std::fmt::Debug) -> Status {
    Status::IO
}

/// A shared-memory error, as the status a stream's attachment is refused with.
fn ring_status(_error: impl std::fmt::Debug) -> Status {
    Status::IO
}

impl StreamCore {
    pub(crate) fn new(spec: &StreamSpec, name: &str, allocator: Arc<dyn StreamAllocator>) -> Self {
        // A spec from a parsed board file has already been checked.
        let layout = spec.layout();
        let planes = Planes::new(&spec.plane_bytes(), layout.buffer_bytes)
            .unwrap_or(Planes::one(layout.buffer_bytes));
        Self {
            pool: BufferPool::new(layout.buffers),
            layout,
            working: spec.working,
            // Set from the driver that serves the stream (`board::resolve_kinds`); a stream no
            // driver serves has no producer, so its kind does not matter.
            kind: spec.kind.unwrap_or(StreamKind::FreeRunning),
            memory: spec.memory,
            heap: spec.heap_name().into(),
            planes,
            name: name.into(),
            allocator,
            standby: spec.standby,
            attached: None,
            waiting: None,
            kept: None,
            standby_failed: false,
            closed: false,
            claimed: false,
            epoch: 0,
            generation: 0,
            published_at: vec![0; layout.buffers as usize].into_boxed_slice(),
            published_count: 0,
            counters: Counters::default(),
            producer: ProducerWords::default(),
            producer_lost: false,
        }
    }

    /// The producing device failed and is being retried, from `since_ns`. `lost` says the host
    /// dropped the driver instance. A change from running is an edge: the page says so and the
    /// consumer's doorbell rings. Already retrying, only learning that the instance was dropped
    /// changes anything, and that is no edge. Allocates nothing.
    pub(crate) fn producer_failed(&mut self, since_ns: u64, lost: bool) {
        if self.producer.state == producer::RETRYING {
            if lost && !self.producer_lost {
                self.producer_lost = true;
            }
            return;
        }
        self.producer_lost = lost;
        self.producer = ProducerWords {
            edges: self.producer.edges + 1,
            state: producer::RETRYING,
            since_ns,
            recovery: self.producer.recovery,
        };
        self.publish_producer();
    }

    /// The producing device works again. An edge only if it was retrying.
    pub(crate) fn producer_back(&mut self) {
        if self.producer.state != producer::RETRYING {
            return;
        }
        self.producer = ProducerWords {
            edges: self.producer.edges + 1,
            state: producer::RUNNING,
            since_ns: 0,
            recovery: if self.producer_lost {
                producer::LOST
            } else {
                producer::KEPT
            },
        };
        self.producer_lost = false;
        self.publish_producer();
    }

    /// Writes the producer's words into the consumer's page and rings its doorbell.
    fn publish_producer(&mut self) {
        let words = self.producer;
        if let Some(attached) = self.attached.as_mut() {
            attached.jobs.set_producer(words);
            attached.bell.ring();
        }
    }

    pub(crate) const fn memory(&self) -> StreamMemory {
        self.memory
    }

    pub(crate) const fn is_attached(&self) -> bool {
        self.attached.is_some()
    }

    /// The pool buffers of this stream: one per plane of each buffer.
    fn pool_buffers(&self) -> u32 {
        self.layout.buffers * self.planes.count()
    }

    /// Memory for one consumer, as the board file declares it: the buffers from the allocator and
    /// the rings the host makes over them.
    fn make(&mut self) -> Result<Memory, Status> {
        let shared = self.memory == StreamMemory::Shared;
        let planes = self.planes.count();
        let Buffers { fds, dma, sync } = match self.memory {
            StreamMemory::Shared => self.allocator.allocate(&self.name, &self.layout)?,
            StreamMemory::DmaBuf => self.allocator.allocate_planes(
                &self.name,
                &self.layout,
                &self.heap,
                &self.planes,
            )?,
        };
        // What the board file declared is what a driver was written against, and what its end of
        // the stream told it when it bound.
        let expected = if shared {
            1
        } else {
            self.pool_buffers() as usize
        };
        if fds.len() != expected || dma == shared {
            return Err(Status::IO);
        }
        let lengths: Vec<u32> = if shared || planes == 1 {
            Vec::new()
        } else {
            self.planes.lengths().to_vec()
        };
        let descs = self.layout.pool_descs(shared, &lengths);
        let handles = if dma {
            let copies = fds
                .iter()
                .map(|fd| fd.try_clone().map_err(io_status))
                .collect::<Result<Vec<_>, _>>()?;
            Some(Arc::from(copies))
        } else {
            None
        };
        // The pool generation: one more than the last memory made, never 0 (a page of 0 reads as no pool).
        self.generation = self.generation.wrapping_add(1).max(1);
        let pool = Pool::mapped(self.generation, fds, descs).map_err(|_| Status::IO)?;
        let ring = Ring::create(&self.layout.frame_spec()).map_err(ring_status)?;
        let jobs = Ring::create(&self.layout.job_spec()).map_err(ring_status)?;
        let acks = sealed_memfd(JOB_ACK_BYTES).map_err(io_status)?;
        let writer = ring.writer().map_err(ring_status)?;
        Ok(Memory {
            publisher: Publisher::new(writer, FieldId(0), pool),
            ring,
            jobs,
            acks,
            handles,
            sync: if dma { sync } else { None },
        })
    }

    /// On standby with nobody attached, makes the memory the next consumer gets, once.
    fn stand_by(&mut self) {
        if !self.standby
            || self.closed
            || self.standby_failed
            || self.attached.is_some()
            || self.waiting.is_some()
        {
            return;
        }
        match self.make() {
            Ok(memory) => {
                self.waiting = Some(memory);
                self.epoch += 1;
            }
            Err(_) => self.standby_failed = true,
        }
    }

    /// The memory in place now: the consumer's, or what waits on standby for the next.
    fn in_place(&self) -> Option<&Memory> {
        match (&self.attached, &self.waiting) {
            (Some(attached), _) => Some(&attached.memory),
            (None, Some(waiting)) => Some(waiting),
            (None, None) => None,
        }
    }

    fn in_place_mut(&mut self) -> Option<&mut Memory> {
        match (&mut self.attached, &mut self.waiting) {
            (Some(attached), _) => Some(&mut attached.memory),
            (None, Some(waiting)) => Some(waiting),
            (None, None) => None,
        }
    }

    /// Takes the consumer's end for `caller`: the memory waiting on standby, with the frame kept
    /// on it at once, or memory made now.
    pub(crate) fn attach(&mut self, caller: CallerId) -> Result<Lease, Status> {
        if self.attached.is_some() {
            return Err(Status::BUSY);
        }
        let (memory, adopted) = match self.waiting.take() {
            Some(memory) => (memory, true),
            None => (self.make()?, false),
        };
        let parts = match self.consumer_parts(&memory) {
            Ok(parts) => parts,
            Err(status) => {
                // The memory is gone either way; the producer's buffers were on it.
                if adopted {
                    self.drop_kept();
                    self.epoch += 1;
                }
                return Err(status);
            }
        };
        let (descriptors, holds, jobs, bell) = parts;
        if !adopted {
            self.epoch += 1;
        }
        let handles = memory.handles.clone();
        self.counters.attachments += 1;
        let mut jobs = jobs;
        // A consumer that attaches during a stall reads it before any wait.
        jobs.set_producer(self.producer);
        self.attached = Some(Attached {
            caller,
            memory,
            holds,
            jobs,
            bell,
        });
        if let Some(frame) = self.kept.take() {
            self.deliver(frame);
        }
        Ok(Lease {
            layout: self.layout,
            working: self.working,
            kind: self.kind,
            descriptors,
            handles,
            planes: self.planes,
            shared: self.memory == StreamMemory::Shared,
        })
    }

    /// The consumer's descriptors and the host's ends that go with them: the hold marks, the job
    /// collector and the doorbell.
    fn consumer_parts(
        &self,
        memory: &Memory,
    ) -> Result<(Vec<OwnedFd>, HoldTable<ReadMap>, JobCollector, Bell), Status> {
        let buffers = self.pool_buffers();
        // The consumer's marks page: made by `Marks`, so it carries this build's layout.
        let marks = Marks::<WriteMap>::create(buffers, 0, 0)
            .and_then(|marks| marks.writable_fd())
            .map_err(ring_status)?;
        let mut holds = HoldTable::new(buffers);
        holds
            .add(
                0,
                read_only(marks.as_fd()).map_err(io_status)?,
                Limits::default(),
            )
            .map_err(ring_status)?;
        let jobs = JobCollector::new(
            memory.jobs.read_fd().map_err(ring_status)?,
            memory.acks.try_clone().map_err(io_status)?,
            &self.layout,
        )
        .map_err(ring_status)?;
        let doorbell = Doorbell::create().map_err(|_| Status::IO)?;
        let (bell_eventfd, bell_page) = doorbell.subscriber_fds().map_err(|_| Status::IO)?;
        let (host_eventfd, host_page) = doorbell.bell_fds().map_err(|_| Status::IO)?;
        let bell = Bell::attach(host_eventfd, host_page).map_err(|_| Status::IO)?;

        let pool_fds: Vec<OwnedFd> = memory
            .publisher
            .pool()
            .descriptors()
            .map(|fd| fd.try_clone_to_owned().map_err(io_status))
            .collect::<Result<_, _>>()?;
        let mut descriptors = Vec::with_capacity(fds::BUFFERS + pool_fds.len());
        descriptors.push(memory.ring.read_fd().map_err(ring_status)?);
        descriptors.push(memory.publisher.pool().page_fd().map_err(ring_status)?);
        descriptors.push(memory.jobs.write_fd().map_err(ring_status)?);
        descriptors.push(read_only(memory.acks.as_fd()).map_err(io_status)?);
        descriptors.push(bell_eventfd);
        descriptors.push(bell_page);
        descriptors.push(marks);
        descriptors.extend(pool_fds);
        Ok((descriptors, holds, jobs, bell))
    }

    /// Ends the attachment with `state`, taking back everything the consumer had. Its memory goes
    /// with it: the ring records the end, which a consumer still reading it sees.
    pub(crate) fn detach(&mut self, state: u32) {
        let Some(attached) = self.attached.take() else {
            return;
        };
        attached.memory.ring.set_end(state);
        let mut bell = attached.bell;
        bell.ring();
        self.epoch += 1;
        self.pool.reclaim_published();
        // Fresh memory for the next consumer, if on standby, when the producer next asks.
        self.standby_failed = false;
    }

    /// The host is going: ends the attachment and lets go of any memory on standby.
    pub(crate) fn close(&mut self, state: u32) {
        self.closed = true;
        self.detach(state);
        if self.waiting.take().is_some() {
            self.drop_kept();
            self.epoch += 1;
        }
    }

    fn drop_kept(&mut self) {
        if let Some(frame) = self.kept.take() {
            self.pool.drop_kept(frame.buffer);
            self.counters.dropped += 1;
        }
    }

    pub(crate) fn detach_if(&mut self, caller: CallerId) -> Result<(), Status> {
        if self.attached.as_ref().is_some_and(|a| a.caller == caller) {
            self.detach(STATE_CLOSED);
            Ok(())
        } else {
            Err(Status::NOT_FOUND)
        }
    }

    /// A consumer broke the rules: it loses the stream. Nothing else does.
    fn fault(&mut self) {
        if self.attached.is_none() {
            return;
        }
        self.counters.faults += 1;
        self.detach(STATE_FAULT);
    }

    /// The next buffer for the producer: a free one, or, when there is none, the frame the
    /// publisher takes back. That is the oldest frame no consumer holds, never the newest, so a
    /// consumer that is behind loses old frames and nothing it holds. A consumer whose marks are
    /// stuck, or that the publisher cut off, is ended as a fault.
    fn next_buffer(&mut self) -> Option<u32> {
        self.stand_by();
        if let Some(buffer) = self.pool.take(self.epoch) {
            return Some(buffer);
        }
        self.take_back()
    }

    /// A buffer for a result about to be published. On a job-driven stream, finding none is a job
    /// that does not start (the consumer holds every buffer, or has not read the result), and is
    /// counted once in `refused`. Collecting what was returned is `next_buffer`, and counts nothing.
    fn ask_buffer(&mut self) -> Option<u32> {
        let buffer = self.next_buffer();
        if buffer.is_none() && self.kind == StreamKind::JobDriven {
            self.counters.refused += 1;
        }
        buffer
    }

    fn take_back(&mut self) -> Option<u32> {
        let now = SampleTime::now().as_nanos();
        let planes = self.planes.count();
        let buffers = self.layout.buffers;
        let epoch = self.epoch;
        let attached = self.attached.as_mut()?;
        attached
            .holds
            .enforce(now, attached.memory.publisher.pool().page());
        if attached.holds.is_cut_off(0) {
            self.fault();
            return None;
        }
        let attached = self.attached.as_mut()?;
        // Only published frames can be taken back, and never a buffer the producer already holds.
        // A frame the consumer has read goes back at any time: nothing is lost. A frame it has not
        // read is replaced only when the producer has no free buffer (this is only reached then),
        // and only on a free-running stream, whose frames arrive whether or not anyone reads them:
        // there the oldest unread frame goes, never the newest, and the replacement is counted in
        // `dropped`. On a job-driven stream an unread frame is a result the consumer posted a job
        // for, so it is never replaced; the job that needs its buffer does not start. Oldest first;
        // a frame whose planes don't all come back is skipped. The producer's own buffers do not
        // matter: a producer that keeps a queue in its device must be able to refill it.
        let newest = (0..buffers)
            .filter(|b| matches!(self.pool.state(*b), Some(BufferState::Consumer { .. })))
            .map(|b| self.published_at[b as usize])
            .max()
            .unwrap_or(0);
        let mut after = 0_u64;
        loop {
            let mut oldest: Option<(u64, u32)> = None;
            for buffer in 0..buffers {
                if !matches!(self.pool.state(buffer), Some(BufferState::Consumer { .. })) {
                    continue;
                }
                let at = self.published_at[buffer as usize];
                if at <= after {
                    continue;
                }
                let taken = attached
                    .memory
                    .publisher
                    .taken(BufferIndex(buffer * planes), &attached.holds);
                if !taken && (at == newest || self.kind == StreamKind::JobDriven) {
                    continue;
                }
                if oldest.is_none_or(|(seen, _)| at < seen) {
                    oldest = Some((at, buffer));
                }
            }
            let (at, buffer) = oldest?;
            // Whether the consumer took the frame before it is replaced: only a frame it never
            // took is lost. Asked before the planes are reclaimed, while the frame is still there.
            let taken = attached
                .memory
                .publisher
                .taken(BufferIndex(buffer * planes), &attached.holds);
            let mut free = true;
            for plane in 0..planes {
                let index = BufferIndex(buffer * planes + plane);
                free &= attached.memory.publisher.reclaim(index, &attached.holds);
            }
            if free {
                self.pool.grant(buffer, epoch);
                self.counters.returned += 1;
                if !taken {
                    // The consumer never read this frame: it is dropped.
                    self.counters.dropped += 1;
                }
                return Some(buffer);
            }
            after = at;
        }
    }

    fn publish(&mut self, frame: StreamFrame) -> Result<(), ErrorKind> {
        if frame.bytes > self.layout.buffer_bytes {
            return Err(ErrorKind::InvalidInput);
        }
        let current = match self.pool.state(frame.buffer) {
            Some(BufferState::Producer { epoch }) => epoch == self.epoch,
            _ => return Err(ErrorKind::InvalidInput),
        };
        self.counters.published += 1;
        // Filled on the memory waiting on standby: the newest is kept for the next consumer.
        // An empty frame is a buffer given up, and is not one to keep.
        if current && self.attached.is_none() && self.waiting.is_some() && frame.bytes > 0 {
            self.drop_kept();
            self.pool.keep(frame.buffer);
            self.kept = Some(frame);
            return Ok(());
        }
        if !current || self.attached.is_none() {
            self.pool.drop_held(frame.buffer);
            self.counters.dropped += 1;
            return Ok(());
        }
        self.deliver(frame);
        Ok(())
    }

    /// Puts a frame the consumer attached now is due on its ring: the frame's buffer's planes, and
    /// its metadata.
    fn deliver(&mut self, frame: StreamFrame) {
        let fence = self.pool.publish(frame.buffer);
        self.published_count += 1;
        self.published_at[frame.buffer as usize] = self.published_count;
        let planes = self.planes.count();
        let mut names = [BufferIndex(0); MAX_PER_FRAME];
        for plane in 0..planes {
            names[plane as usize] = BufferIndex(frame.buffer * planes + plane);
        }
        let meta = FrameInfo {
            sequence: frame.sequence,
            identity: frame.identity,
            bytes: frame.bytes,
        }
        .encode();
        let time = SampleTime::from_nanos(frame.timestamp_ns);
        let Some(attached) = self.attached.as_mut() else {
            self.pool.drop_held(frame.buffer);
            return;
        };
        let published = attached
            .memory
            .publisher
            .publish(&names[..planes as usize], time, &meta);
        match published {
            Ok(_) => {
                attached.bell.ring();
                self.counters.delivered += 1;
            }
            Err(_) => {
                // Not free on the ring: the frame never reached anyone, so its buffer is the
                // producer's to drop.
                let _ = self.pool.release(frame.buffer, fence);
                self.pool.drop_held(frame.buffer);
                self.counters.dropped += 1;
            }
        }
    }

    fn next_job(&mut self, out: &mut [u8]) -> Result<Option<usize>, ErrorKind> {
        let Some(attached) = self.attached.as_mut() else {
            return Ok(None);
        };
        let taken = attached.jobs.next(out);
        match taken {
            Ok(job) => Ok(job),
            Err(RingError::JobTooLarge) => Err(ErrorKind::InvalidInput),
            Err(_) => {
                self.fault();
                Ok(None)
            }
        }
    }

    /// Whether `buffer` is one the producer holds (not published, not kept).
    fn holds_buffer(&self, buffer: u32) -> Result<(), ErrorKind> {
        match self.pool.state(buffer) {
            Some(BufferState::Producer { .. }) => Ok(()),
            _ => Err(ErrorKind::InvalidInput),
        }
    }

    fn write(&mut self, buffer: u32, offset: usize, data: &[u8]) -> Result<bool, ErrorKind> {
        self.holds_buffer(buffer)?;
        let in_buffer = offset
            .checked_add(data.len())
            .is_some_and(|end| end <= self.layout.buffer_bytes as usize);
        if !in_buffer {
            return Err(ErrorKind::InvalidInput);
        }
        let planes = self.planes;
        let Some(memory) = self.in_place_mut() else {
            return Ok(false);
        };
        Ok(span(
            &planes,
            buffer,
            offset,
            data.len(),
            |index, place, from, take| {
                write_plane(
                    memory_mut_parts(memory),
                    index,
                    place,
                    &data[from..from + take],
                )
            },
        ))
    }

    fn read(&mut self, buffer: u32, offset: usize, out: &mut [u8]) -> Result<bool, ErrorKind> {
        self.holds_buffer(buffer)?;
        let in_buffer = offset
            .checked_add(out.len())
            .is_some_and(|end| end <= self.layout.buffer_bytes as usize);
        if !in_buffer {
            return Err(ErrorKind::InvalidInput);
        }
        let planes = self.planes;
        let Some(memory) = self.in_place_mut() else {
            return Ok(false);
        };
        let len = out.len();
        Ok(span(
            &planes,
            buffer,
            offset,
            len,
            |index, place, from, take| {
                read_plane(
                    memory_mut_parts(memory),
                    index,
                    place,
                    &mut out[from..from + take],
                )
            },
        ))
    }

    /// Whether `buffer`, which the producer holds, was taken on the memory in place now.
    fn current(&self, buffer: u32) -> Result<bool, ErrorKind> {
        let Some(BufferState::Producer { epoch }) = self.pool.state(buffer) else {
            return Err(ErrorKind::InvalidInput);
        };
        Ok(self.in_place().is_some() && epoch == self.epoch)
    }

    /// Whether `buffer`'s handles can be lent: the epoch it was taken in when that memory is
    /// still in place, `None` when nothing is behind it now. Refused for a buffer the producer
    /// doesn't hold, and `Unsupported` when the buffers in place aren't dma-bufs.
    fn lendable(&self, buffer: u32) -> Result<Option<u64>, ErrorKind> {
        let Some(BufferState::Producer { epoch }) = self.pool.state(buffer) else {
            return Err(ErrorKind::InvalidInput);
        };
        let Some(memory) = self.in_place() else {
            return Ok(None);
        };
        if memory.handles.is_none() {
            return Err(ErrorKind::Unsupported);
        }
        Ok((epoch == self.epoch).then_some(epoch))
    }

    pub(crate) fn stats(&self) -> StreamStats {
        let (mut free, producer_held, mut consumer_held, kept) = self.pool.counts();
        // A published buffer the consumer has let go of, which nothing has reclaimed yet, is free
        // for the producer to take: only a buffer a hold mark names is the consumer's.
        if let Some(attached) = self.attached.as_ref() {
            let planes = self.planes.count();
            for buffer in 0..self.layout.buffers {
                if matches!(self.pool.state(buffer), Some(BufferState::Consumer { .. }))
                    && !attached.holds.held(BufferIndex(buffer * planes))
                {
                    consumer_held = consumer_held.saturating_sub(1);
                    free += 1;
                }
            }
        }
        StreamStats {
            attached: self.is_attached(),
            claimed: self.claimed,
            attachments: self.counters.attachments,
            published: self.counters.published,
            delivered: self.counters.delivered,
            dropped: self.counters.dropped,
            returned: self.counters.returned,
            faults: self.counters.faults,
            refused: self.counters.refused,
            free,
            producer_held,
            consumer_held,
            kept,
            standby: self.waiting.is_some(),
        }
    }
}

/// The parts of a memory a plane access needs, split so the publisher and the dma-buf brackets
/// are borrowed at once.
struct Parts<'a> {
    publisher: &'a mut Publisher<WriteMap>,
    sync: Option<&'a Arc<dyn DmaSync>>,
}

fn memory_mut_parts(memory: &mut Memory) -> Parts<'_> {
    Parts {
        publisher: &mut memory.publisher,
        sync: memory.sync.as_ref(),
    }
}

/// Calls `each` for every plane `take` bytes at buffer offset `offset` lands in: the plane's pool
/// index, the place in that plane, where in `data` the bytes start, and how many. `false` if the
/// range doesn't lie in the buffer, or a plane refuses.
fn span(
    planes: &Planes,
    buffer: u32,
    offset: usize,
    len: usize,
    mut each: impl FnMut(usize, usize, usize, usize) -> bool,
) -> bool {
    let count = planes.count();
    let (mut at, mut left, mut from) = (offset, len, 0);
    let mut ok = true;
    while left > 0 {
        let Some((plane, place, room)) = planes.locate(at) else {
            return false;
        };
        let take = left.min(room);
        let index = buffer as usize * count as usize + plane as usize;
        ok &= each(index, place, from, take);
        at += take;
        left -= take;
        from += take;
    }
    ok
}

fn write_plane(parts: Parts<'_>, index: usize, place: usize, data: &[u8]) -> bool {
    if let Some(sync) = parts.sync {
        sync.begin(index, Access::Write);
    }
    let written = parts
        .publisher
        .buffer_mut(BufferIndex(index as u32))
        .and_then(|bytes| bytes.get_mut(place..place.checked_add(data.len())?))
        .map(|dst| dst.copy_from_slice(data))
        .is_some();
    if let Some(sync) = parts.sync {
        sync.end(index, Access::Write);
    }
    written
}

fn read_plane(parts: Parts<'_>, index: usize, place: usize, out: &mut [u8]) -> bool {
    if let Some(sync) = parts.sync {
        sync.begin(index, Access::Read);
    }
    let read = parts
        .publisher
        .buffer_mut(BufferIndex(index as u32))
        .and_then(|bytes| bytes.get(place..place.checked_add(out.len())?))
        .map(|src| out.copy_from_slice(src))
        .is_some();
    if let Some(sync) = parts.sync {
        sync.end(index, Access::Read);
    }
    read
}

/// The producer's end of a stream, given to a driver through `StreamHandle` and usable as a
/// software producer in tests. Dropping it gives back what the producer held, and a consumer
/// attached carries on, with the producer's state in its job-ack page while the device is retried
/// (`StreamCore::producer_failed`), or ended with `STATE_DEVICE_FAILED` once the device is off for
/// good (`Streams::end_device`).
pub struct ProducerEndpoint {
    core: Arc<Mutex<StreamCore>>,
    buffers: u32,
    /// What the board file says the buffers are. An attachment is never made of anything else.
    kind: BufferKind,
    /// The dma-bufs of the attachment the last buffer was taken in, kept here so one can be
    /// lent for as long as this end is borrowed. Changed only when a buffer is taken, which
    /// needs this end exclusively, so never while a handle is out.
    lent: Option<Arc<[OwnedFd]>>,
    /// The epoch `lent` belongs to.
    lent_epoch: u64,
    /// Each buffer's planes.
    planes: Planes,
}

impl ProducerEndpoint {
    pub(super) fn claim(core: Arc<Mutex<StreamCore>>) -> Result<Self, Status> {
        let (buffers, kind, planes) = {
            let mut guard = lock(&core);
            if guard.claimed {
                return Err(Status::BUSY);
            }
            guard.claimed = true;
            let kind = match guard.memory {
                StreamMemory::Shared => BufferKind::Plain,
                StreamMemory::DmaBuf => BufferKind::Handles,
            };
            (guard.layout.buffers, kind, guard.planes)
        };
        Ok(Self {
            core,
            buffers,
            kind,
            lent: None,
            lent_epoch: 0,
            planes,
        })
    }

    /// A handle for writing the buffers this producer holds, as a device would.
    #[must_use]
    pub fn writer(&self) -> BufferWriter {
        BufferWriter {
            core: Arc::clone(&self.core),
        }
    }

    #[must_use]
    pub fn stats(&self) -> StreamStats {
        lock(&self.core).stats()
    }
}

/// Writes the buffers a stream's producer holds, from software. A device writes them by DMA and
/// has no need of this.
#[derive(Clone)]
pub struct BufferWriter {
    core: Arc<Mutex<StreamCore>>,
}

impl BufferWriter {
    /// Copies `data` into `buffer` at `offset`. `false` when no consumer is attached, so there's
    /// nowhere for it to land. Refused for a buffer the producer doesn't hold or a range outside it.
    pub fn write(&self, buffer: u32, offset: usize, data: &[u8]) -> Result<bool, ErrorKind> {
        lock(&self.core).write(buffer, offset, data)
    }
}

impl StreamEndpoint for ProducerEndpoint {
    type Error = ErrorKind;

    fn buffer_count(&self) -> u32 {
        self.buffers
    }

    fn buffer_kind(&self) -> BufferKind {
        self.kind
    }

    fn publish(&mut self, frame: StreamFrame) -> Result<(), ErrorKind> {
        lock(&self.core).publish(frame)
    }

    fn next_buffer(&mut self) -> Result<Option<u32>, ErrorKind> {
        let mut core = lock(&self.core);
        let buffer = core.next_buffer();
        if buffer.is_some() && self.lent_epoch != core.epoch {
            // Cloning shares the descriptors, and allocates nothing.
            self.lent = core.in_place().and_then(|memory| memory.handles.clone());
            self.lent_epoch = core.epoch;
        }
        Ok(buffer)
    }

    fn ask_buffer(&mut self) -> Result<Option<u32>, ErrorKind> {
        let mut core = lock(&self.core);
        let buffer = core.ask_buffer();
        if buffer.is_some() && self.lent_epoch != core.epoch {
            // Cloning shares the descriptors, and allocates nothing.
            self.lent = core.in_place().and_then(|memory| memory.handles.clone());
            self.lent_epoch = core.epoch;
        }
        Ok(buffer)
    }

    fn next_job(&mut self, out: &mut [u8]) -> Result<Option<usize>, ErrorKind> {
        lock(&self.core).next_job(out)
    }

    fn write(&mut self, buffer: u32, offset: usize, data: &[u8]) -> Result<bool, ErrorKind> {
        lock(&self.core).write(buffer, offset, data)
    }

    fn read(&mut self, buffer: u32, offset: usize, out: &mut [u8]) -> Result<bool, ErrorKind> {
        lock(&self.core).read(buffer, offset, out)
    }

    fn buffer_current(&self, buffer: u32) -> Result<bool, ErrorKind> {
        lock(&self.core).current(buffer)
    }

    fn buffer_handle(&self, buffer: u32) -> Result<Option<BufferHandle<'_>>, ErrorKind> {
        self.plane_handle(buffer, 0)
    }

    fn planes(&self) -> u32 {
        self.planes.count()
    }

    fn plane_offset(&self, plane: u32) -> Option<usize> {
        self.planes.offset(plane)
    }

    fn plane_handle(&self, buffer: u32, plane: u32) -> Result<Option<BufferHandle<'_>>, ErrorKind> {
        if plane >= self.planes.count() {
            return Err(ErrorKind::InvalidInput);
        }
        let current = lock(&self.core).lendable(buffer)?;
        // A buffer taken on the memory that is still in place was taken through this end, so
        // `lent` is that memory's.
        if current != Some(self.lent_epoch) {
            return Ok(None);
        }
        let index = (buffer as usize)
            .checked_mul(self.planes.count() as usize)
            .and_then(|first| first.checked_add(plane as usize));
        let handles = self.lent.as_ref();
        Ok(index
            .and_then(|index| handles.and_then(|handles| handles.get(index)))
            .map(|fd| BufferHandle::from(fd.as_fd())))
    }

    fn fault_consumer(&mut self) -> Result<(), ErrorKind> {
        lock(&self.core).fault();
        Ok(())
    }
}

impl Drop for ProducerEndpoint {
    fn drop(&mut self) {
        let mut core = lock(&self.core);
        core.claimed = false;
        core.pool.reclaim_producer();
    }
}
