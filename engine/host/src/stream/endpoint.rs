//! The producer's end of a stream, and the state both ends share under one lock.
//!
//! The memory of one attachment ([`Memory`]) is the shm family's: a frame ring and a job ring the
//! host owns, and a buffer pool it publishes into. The consumer's end ([`Lease`]) is the descriptors
//! of those, plus a hold page and a doorbell the host makes for it. What the consumer gives back is
//! read from its hold marks, never from the shared memory's words.

use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use lemnos_core::{CallerId, Status};
use lemnos_hal::blocks::{BufferHandle, BufferKind, StreamFrame};
use lemnos_hal::{ErrorKind, StreamEndpoint};
use lemnos_wire::Planes;
use lemnos_wire::ring::{
    FrameInfo, JOB_ACK_BYTES, JobCollector, Layout, ProducerWords, RingError, STATE_CLOSED,
    STATE_FAULT, fds, producer,
};
use shm_ring::{
    FieldId, ReadMap, Ring, SampleTime, WriteMap, liveness_pipe, read_only, sealed_memfd,
};
use shm_ring_buffers::{BufferIndex, BufferWriter as Publisher, Holders, MAX_PER_FRAME, Pool};
use shm_ring_doorbell::{Bell, Doorbell};
use shm_ring_holds::{HoldTable, Limits, Marks};

use super::pool::{BufferPool, BufferState};
use super::{
    Access, Buffers, DmaSync, Lease, ReaderTicket, StreamAllocator, StreamStats, TicketRefused,
    TicketState,
};
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
    /// The bells of the outside readers that hold a slot, by slot: rung with the consumer's.
    reader_bells: Vec<(u32, Bell)>,
    /// What each reader slot was granted: its slot and how many buffers it may hold.
    grants: Vec<(u32, u32)>,
    /// For each reader slot, why and when it was last cut off (`CLOCK_MONOTONIC` ns), kept until the
    /// slot is given to a new reader.
    cut_at: Box<[Option<(u64, u64)>]>,
    /// How many cut-offs of readers this attachment has seen.
    cut_total: u64,
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
    /// What an outside reader may be given: how many slots, the most it may be granted, and the
    /// slack past each dma-buf.
    readers: u32,
    /// Whether the dma-bufs are allocated read-only: a stream the hardware fills, with reader slots.
    read_only_buffers: bool,
    /// Whether readers may import this stream's dma-bufs, under the daemon's signed mode.
    reader_import: bool,
    reader_held_max: u32,
    reader_hold_ms_max: u32,
    tail_bytes: u32,
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
    /// Each buffer's newest frame's identity and length, for a block that reads it lent.
    published_identity: Box<[u64]>,
    published_bytes: Box<[u32]>,
    /// Lent to a block that reads the buffer (`LendSource`): the identity of the frame lent. A lent
    /// buffer is never taken back until it is unlent.
    lent: Box<[Option<u64>]>,
    /// Frames published so far on this stream.
    published_count: u64,
    counters: Counters,
    /// What the stream's producing device is doing, written into each attachment's page.
    producer: ProducerWords,
    /// The device was bound again while retrying, so what it recovers to is a new instance.
    producer_lost: bool,
    /// The descriptor sets of memory this stream has let go of, weakly: while a driver's handle
    /// still holds one, the set's dma-bufs stay open. Counted by `stats` as held by the driver.
    /// Fixed size, so none is allocated; a set past the last slot's reach is not counted.
    released: [Option<Weak<[OwnedFd]>>; RELEASED_SLOTS],
}

/// How many released descriptor sets are tracked at once.
const RELEASED_SLOTS: usize = 8;

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
            readers: spec.readers(),
            read_only_buffers: spec.read_only_buffers(),
            reader_import: spec.reader_import,
            reader_held_max: spec.reader_held_max(),
            reader_hold_ms_max: spec.reader_hold_ms_max,
            tail_bytes: spec.tail_bytes,
            attached: None,
            waiting: None,
            kept: None,
            standby_failed: false,
            closed: false,
            claimed: false,
            epoch: 0,
            generation: 0,
            published_at: vec![0; layout.buffers as usize].into_boxed_slice(),
            published_identity: vec![0; layout.buffers as usize].into_boxed_slice(),
            published_bytes: vec![0; layout.buffers as usize].into_boxed_slice(),
            lent: vec![None; layout.buffers as usize].into_boxed_slice(),
            published_count: 0,
            counters: Counters::default(),
            producer: ProducerWords::default(),
            producer_lost: false,
            released: std::array::from_fn(|_| None),
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
            StreamMemory::DmaBuf => self.allocator.allocate_planes_for(
                &self.name,
                &self.layout,
                &self.heap,
                &self.planes,
                // Read-only only where nothing maps the buffers writable on the CPU: a stream the
                // hardware fills, with reader slots (see `StreamSpec::read_only_buffers`).
                self.read_only_buffers,
                self.tail_bytes,
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
        // Read-only dma-bufs can't be mapped writable, and the host never fills a dma-buf on the CPU: the
        // unmapped pool holds them. Other pools are mapped as they always were.
        let pool = if dma && self.read_only_buffers {
            Pool::new(self.generation, fds, descs).map_err(|_| Status::IO)?
        } else {
            Pool::mapped(self.generation, fds, descs).map_err(|_| Status::IO)?
        };
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
            reader_bells: Vec::new(),
            grants: Vec::new(),
            cut_at: vec![None; shm_ring_buffers::MAX_READERS as usize].into_boxed_slice(),
            cut_total: 0,
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

    /// Gives `caller`, the stream's consumer, a reader slot: a third process's view of the same
    /// buffers, read-only, with its own marks page, bell and liveness pipe.
    ///
    /// `held` and `hold_ms` are asked for; zero asks for 2 buffers and 500 ms. A request is granted
    /// up to the stream's `reader_held_max` and `reader_hold_ms_max`. A grant is refused when the
    /// holds granted to all readers, with this one, would leave the producer fewer than two buffers
    /// after the consumer's one: the refusal says what was asked and what is left.
    pub(crate) fn ticket(
        &mut self,
        caller: CallerId,
        held: u32,
        hold_ms: u32,
        import: bool,
        signed: bool,
    ) -> Result<ReaderTicket, TicketRefused> {
        let stream = self.name.to_string();
        let refuse = |status: Status, text: String| TicketRefused { status, text };
        let buffers = self.layout.buffers;
        let asked_held = if held == 0 { DEFAULT_HELD } else { held };
        let asked_ms = if hold_ms == 0 {
            DEFAULT_HOLD_MS
        } else {
            hold_ms
        };
        let granted_held = asked_held.min(self.reader_held_max);
        let granted_ms = asked_ms.min(self.reader_hold_ms_max);
        let shared = self.memory == StreamMemory::Shared;
        let layout = self.layout;
        let planes = self.planes;
        let readers = self.readers;
        let reader_import = self.reader_import;
        let tail = self.tail_bytes;
        let pool_buffers = self.pool_buffers();
        let attached = self.attached.as_mut().ok_or_else(|| {
            refuse(
                Status::NOT_FOUND,
                format!("stream {stream:?} has no consumer"),
            )
        })?;
        if attached.caller != caller {
            return Err(refuse(
                Status::NOT_PERMITTED,
                format!("the connection is not the consumer of stream {stream:?}"),
            ));
        }
        if readers == 0 {
            return Err(refuse(
                Status::NOT_SUPPORTED,
                format!("stream {stream:?} has no reader slots (readers = 0)"),
            ));
        }
        // The rights. A dma-buf reader can only be handed the buffer's descriptor with the import right:
        // without it the reader would need a copy from the service, which the daemon never makes.
        let dma = self.memory == StreamMemory::DmaBuf;
        if dma && !import {
            return Err(refuse(
                Status::NOT_PERMITTED,
                format!(
                    "stream {stream:?}'s buffers can only be handed out with the import right; a \
                     reader without it gets a copy from the service"
                ),
            ));
        }
        if dma && signed && !reader_import {
            return Err(refuse(
                Status::NOT_PERMITTED,
                format!(
                    "stream {stream:?} does not let readers import its buffers (the daemon is in \
                     signed mode): set `reader_import = true` on the stream"
                ),
            ));
        }
        let rights = if dma {
            lemnos_wire::ring::ticket::rights::READ | lemnos_wire::ring::ticket::rights::IMPORT
        } else {
            lemnos_wire::ring::ticket::rights::READ
        };
        let slot = attached.holds.free_reader_slot(readers).ok_or_else(|| {
            refuse(
                Status::BUSY,
                format!("every reader slot of stream {stream:?} is taken (readers = {readers})"),
            )
        })?;
        // The producer keeps two buffers after the consumer's one; the readers share the rest.
        let sum: u32 = attached.grants.iter().map(|(_, held)| held).sum();
        let left = buffers.saturating_sub(sum).saturating_sub(CONSUMER_HELD);
        if left < granted_held || left - granted_held < MIN_PRODUCER_BUFFERS {
            return Err(refuse(
                Status::BUSY,
                format!(
                    "stream {stream:?}: a reader asked for {granted_held} held buffers, and {left} \
                     of {buffers} are left after the consumer's and the other readers' holds; the \
                     producer needs {MIN_PRODUCER_BUFFERS}"
                ),
            ));
        }
        let limits = Limits {
            max_held: granted_held,
            max_hold: Some(std::time::Duration::from_millis(u64::from(granted_ms))),
        };
        // Read-only first, so that a refusal leaves nothing made. The ring and the pool page are
        // sealed; the buffers are sealed (shared memory) or checked to be read-only files (dma-buf).
        attached
            .memory
            .ring
            .share_read_only()
            .map_err(ring_refused)?;
        if let Err(error) = attached.memory.publisher.pool().share_read_only() {
            return Err(if shared {
                ring_refused(error)
            } else {
                refuse(
                    Status::NOT_SUPPORTED,
                    format!(
                        "stream {stream:?}: its dma-bufs are writable, so readers can't have them \
                         read-only ({error}); declare readers only on a stream the hardware fills"
                    ),
                )
            });
        }

        // The reader's marks page, made by `Marks` so that it carries this build's layout.
        let marks = Marks::<WriteMap>::create(pool_buffers, slot, 0)
            .and_then(|marks| marks.writable_fd())
            .map_err(ring_refused)?;
        let marks_ro = read_only(marks.as_fd()).map_err(io_refused)?;
        let (liveness_read, liveness_write) = liveness_pipe().map_err(ring_refused)?;
        let doorbell = Doorbell::create().map_err(|_| refuse(Status::IO, "a doorbell".into()))?;
        let (bell_eventfd, bell_page) = doorbell
            .subscriber_fds()
            .map_err(|_| refuse(Status::IO, "a doorbell".into()))?;
        let (host_eventfd, host_page) = doorbell
            .bell_fds()
            .map_err(|_| refuse(Status::IO, "a doorbell".into()))?;
        let bell = Bell::attach(host_eventfd, host_page)
            .map_err(|_| refuse(Status::IO, "a doorbell".into()))?;

        let mut descriptors = Vec::with_capacity(lemnos_wire::ring::ticket::fds::BUFFERS + 1);
        descriptors.push(attached.memory.ring.read_fd().map_err(ring_refused)?);
        descriptors.push(
            attached
                .memory
                .publisher
                .pool()
                .page_fd()
                .map_err(ring_refused)?,
        );
        descriptors.push(bell_eventfd);
        descriptors.push(bell_page);
        descriptors.push(marks);
        descriptors.push(liveness_write);
        for fd in attached.memory.publisher.pool().descriptors() {
            descriptors.push(fd.try_clone_to_owned().map_err(io_refused)?);
        }

        attached
            .holds
            .add_reader(slot, marks_ro, limits, liveness_read)
            .map_err(ring_refused)?;
        attached.reader_bells.push((slot, bell));
        attached.grants.push((slot, granted_held));
        if let Some(record) = attached.cut_at.get_mut(slot as usize) {
            *record = None;
        }
        Ok(ReaderTicket {
            slot,
            layout,
            planes,
            tail_bytes: tail,
            descriptors,
            held: granted_held,
            hold_ms: granted_ms,
            rights,
        })
    }

    /// Gives back reader `slot`, a ticket `caller` was granted: it is cut off with reason 5, and its
    /// slot is freed once its liveness pipe hangs up.
    pub(crate) fn unticket(&mut self, caller: CallerId, slot: u32) -> Result<(), Status> {
        let attached = self.attached.as_mut().ok_or(Status::NOT_FOUND)?;
        if attached.caller != caller {
            return Err(Status::NOT_PERMITTED);
        }
        if !attached.holds.is_reader(slot) {
            return Err(Status::NOT_FOUND);
        }
        let pool = attached.memory.publisher.pool();
        let before =
            shm_ring_buffers::page::cut_off_reason(pool.page(), pool.buffers().len() as u32, slot);
        attached
            .holds
            .release(slot, attached.memory.publisher.pool().page());
        // Recorded now, as the tick would: the release is the cut-off, and its time is the answer's.
        // A reader already cut off for another reason keeps that reason and its time.
        if let Some(record) = attached.cut_at.get_mut(slot as usize)
            && record.is_none()
            && before == 0
        {
            *record = Some((
                shm_ring_buffers::page::cut::RELEASED,
                SampleTime::now().as_nanos(),
            ));
            attached.cut_total += 1;
        }
        Ok(())
    }

    /// Why reader `slot` was cut off, if it was, and when, and how many cut-offs this attachment has
    /// seen, for `caller`, the consumer. `live` is whether the reader still holds the slot.
    pub(crate) fn ticket_state(&self, caller: CallerId, slot: u32) -> Result<TicketState, Status> {
        let attached = self.attached.as_ref().ok_or(Status::NOT_FOUND)?;
        if attached.caller != caller {
            return Err(Status::NOT_PERMITTED);
        }
        if slot == 0 || slot as usize >= attached.cut_at.len() {
            return Err(Status::NOT_FOUND);
        }
        let live = attached.holds.is_reader(slot);
        // The pool page is the truth while the reader is live; the record keeps the time it landed.
        let now_reason = shm_ring_buffers::page::cut_off_reason(
            attached.memory.publisher.pool().page(),
            attached.memory.publisher.pool().buffers().len() as u32,
            slot,
        );
        let (reason, cut_at_ns) = match attached.cut_at[slot as usize] {
            Some((reason, at)) => (reason, at),
            None if now_reason != 0 => (now_reason, 0),
            None => (0, 0),
        };
        Ok(TicketState {
            live,
            reason,
            cut_at_ns,
            cuts: attached.cut_total,
        })
    }

    /// The host's pass over this stream's readers, on its tick: applies every reader's limits,
    /// frees the slots of readers that have gone, and records each new cut-off with its time. A
    /// consumer cut off by its own limits ends the stream as a fault. Reads memory and polls each
    /// reader's liveness pipe; allocates nothing.
    pub(crate) fn tick(&mut self) {
        let now = SampleTime::now().as_nanos();
        let Some(attached) = self.attached.as_mut() else {
            return;
        };
        attached
            .holds
            .tick(now, attached.memory.publisher.pool().page());
        attached
            .reader_bells
            .retain(|(slot, _)| attached.holds.is_reader(*slot));
        attached
            .grants
            .retain(|(slot, _)| attached.holds.is_reader(*slot));
        // Each reader slot's cut-off, when it first shows in the pool page.
        for slot in 1..attached.cut_at.len() {
            let reason = shm_ring_buffers::page::cut_off_reason(
                attached.memory.publisher.pool().page(),
                attached.memory.publisher.pool().buffers().len() as u32,
                slot as u32,
            );
            if reason != 0 && attached.cut_at[slot].is_none() {
                attached.cut_at[slot] = Some((reason, now));
                attached.cut_total += 1;
            }
        }
        if attached.holds.is_cut_off(0) {
            self.fault();
        }
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
        if let Some(set) = &attached.memory.handles {
            self.note_released(set);
        }
        // Every outside reader sees the same end, and is woken to see it.
        for (_, mut bell) in attached.reader_bells {
            bell.ring();
        }
        self.epoch += 1;
        self.reclaim_unlent_published();
        // Fresh memory for the next consumer, if on standby, when the producer next asks.
        self.standby_failed = false;
    }

    /// The host is going: ends the attachment and lets go of any memory on standby.
    pub(crate) fn close(&mut self, state: u32) {
        self.closed = true;
        self.detach(state);
        if let Some(memory) = self.waiting.take() {
            if let Some(set) = &memory.handles {
                self.note_released(set);
            }
            self.drop_kept();
            self.epoch += 1;
        }
    }

    /// Records a descriptor set this stream has let go of. A dead slot is reused first; with
    /// every slot still held, the first is overwritten (its bytes go uncounted).
    fn note_released(&mut self, set: &Arc<[OwnedFd]>) {
        let slot = self
            .released
            .iter()
            .position(|slot| slot.as_ref().is_none_or(|weak| weak.strong_count() == 0))
            .unwrap_or(0);
        self.released[slot] = Some(Arc::downgrade(set));
    }

    /// Bytes of released memory a handle still holds open: each set is the whole attachment's
    /// buffers, so one live handle holds all of them.
    fn held_by_driver(&self) -> u64 {
        let set_bytes = u64::from(self.layout.buffers) * u64::from(self.layout.buffer_bytes);
        self.released
            .iter()
            .flatten()
            .filter(|weak| weak.strong_count() > 0)
            .count() as u64
            * set_bytes
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
                // Lent to a block: it still reads this frame.
                if self.lent[buffer as usize].is_some() {
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

    /// Takes back every published buffer no block is reading lent. Lock held.
    fn reclaim_unlent_published(&mut self) {
        let lent = &self.lent;
        self.pool
            .reclaim_published_where(|buffer| lent[buffer as usize].is_some());
    }

    /// Lends `buffer` to a block for the frame `identity`: its memory, shared, and the frame's length.
    /// Only a published frame can be lent, and one not already lent. Lock held.
    pub(crate) fn lend(
        &mut self,
        buffer: u32,
        identity: u64,
    ) -> Result<lemnos_hal::Lend<'static>, ErrorKind> {
        let at = buffer as usize;
        if at >= self.lent.len() {
            return Err(ErrorKind::InvalidInput);
        }
        if !matches!(self.pool.state(buffer), Some(BufferState::Consumer { .. }))
            || self.lent[at].is_some()
        {
            return Ok(lemnos_hal::Lend::Busy);
        }
        if self.published_identity[at] != identity {
            return Ok(lemnos_hal::Lend::IdentityMismatch);
        }
        let Some(set) = self.in_place().and_then(|memory| memory.handles.clone()) else {
            return Ok(lemnos_hal::Lend::Busy);
        };
        let Some(handle) = lemnos_hal::blocks::BufferHandle::shared(set, at) else {
            return Ok(lemnos_hal::Lend::Busy);
        };
        self.lent[at] = Some(identity);
        Ok(lemnos_hal::Lend::Lent(lemnos_hal::LentBuffer {
            handle,
            bytes: self.published_bytes[at],
            generation: self.generation,
        }))
    }

    /// Gives back a buffer lent for `identity`. Refused for a buffer not lent to it. Lock held.
    pub(crate) fn unlend(&mut self, buffer: u32, identity: u64) -> Result<(), ErrorKind> {
        match self.lent.get_mut(buffer as usize) {
            Some(lent) if *lent == Some(identity) => {
                *lent = None;
                Ok(())
            }
            _ => Err(ErrorKind::InvalidInput),
        }
    }

    /// Puts a job's end on the ring: no buffer, only the metadata, so the consumer learns the job
    /// ended and why. Lock held; bounded. Refused for a reason of 0, which is a result.
    fn publish_job_end(
        &mut self,
        identity: u64,
        reason: u32,
        end_ns: u64,
    ) -> Result<(), ErrorKind> {
        if reason == lemnos_wire::ring::reason::NONE {
            return Err(ErrorKind::InvalidInput);
        }
        let meta = FrameInfo {
            sequence: 0,
            identity,
            bytes: 0,
            reason,
            end_ns,
        }
        .encode();
        let published = match self.attached.as_mut() {
            Some(attached) => {
                let time = SampleTime::from_nanos(end_ns);
                let result = attached.memory.publisher.publish_empty(time, &meta);
                if result.is_ok() {
                    attached.bell.ring();
                }
                result.is_ok()
            }
            None => false,
        };
        if !published {
            self.counters.dropped += 1;
        }
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
        // A frame of no bytes is a result the block flagged bad: its reason says so.
        let reason = if frame.bytes == 0 {
            lemnos_wire::ring::reason::FAILED
        } else {
            lemnos_wire::ring::reason::NONE
        };
        self.published_identity[frame.buffer as usize] = frame.identity;
        self.published_bytes[frame.buffer as usize] = frame.bytes;
        let meta = FrameInfo {
            sequence: frame.sequence,
            identity: frame.identity,
            bytes: frame.bytes,
            reason,
            end_ns: frame.end_ns,
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
                for (_, bell) in &mut attached.reader_bells {
                    bell.ring();
                }
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
            held_by_driver: self.held_by_driver(),
            standby_bytes: if self.waiting.is_some() {
                u64::from(self.layout.buffers) * u64::from(self.layout.buffer_bytes)
            } else {
                0
            },
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
    /// Each buffer's planes.
    planes: Planes,
}

impl ProducerEndpoint {
    /// The handle for `plane` of `buffer`, sharing the attachment's descriptors. Nothing is kept
    /// here: the handle is the only thing holding the dma-buf open, besides the attachment itself.
    fn plane_share(
        &self,
        buffer: u32,
        plane: u32,
    ) -> Result<Option<BufferHandle<'static>>, ErrorKind> {
        let core = lock(&self.core);
        if core.lendable(buffer)?.is_none() {
            return Ok(None);
        }
        let Some(set) = core.in_place().and_then(|memory| memory.handles.clone()) else {
            return Ok(None);
        };
        let index = (buffer as usize)
            .checked_mul(self.planes.count() as usize)
            .and_then(|first| first.checked_add(plane as usize));
        Ok(index.and_then(|index| BufferHandle::shared(set, index)))
    }

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

/// A stream's buffers lent to a block, from the stream's own core and without claiming the stream: the
/// producer keeps it. Lends and unlends run under the core's lock, so a buffer is never lent while
/// the stream reuses it.
#[derive(Clone)]
pub struct LendView {
    core: Arc<Mutex<StreamCore>>,
}

impl LendView {
    pub(crate) const fn new(core: Arc<Mutex<StreamCore>>) -> Self {
        Self { core }
    }
}

impl lemnos_hal::LendSource for LendView {
    type Error = ErrorKind;

    fn lend(&mut self, buffer: u32, identity: u64) -> Result<lemnos_hal::Lend<'static>, ErrorKind> {
        lock(&self.core).lend(buffer, identity)
    }

    fn unlend(&mut self, buffer: u32, identity: u64) -> Result<(), ErrorKind> {
        lock(&self.core).unlend(buffer, identity)
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

    fn publish_job_end(
        &mut self,
        identity: u64,
        reason: u32,
        end_ns: u64,
    ) -> Result<(), ErrorKind> {
        lock(&self.core).publish_job_end(identity, reason, end_ns)
    }

    fn next_buffer(&mut self) -> Result<Option<u32>, ErrorKind> {
        Ok(lock(&self.core).next_buffer())
    }

    fn ask_buffer(&mut self) -> Result<Option<u32>, ErrorKind> {
        Ok(lock(&self.core).ask_buffer())
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
        self.plane_share(buffer, plane)
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

/// The buffers a reader is granted when it asks for none.
pub(crate) const DEFAULT_HELD: u32 = 2;
/// The time a reader is granted to hold a buffer when it asks for none.
pub(crate) const DEFAULT_HOLD_MS: u32 = 500;
/// Buffers the consumer may hold: its own, which readers are not granted.
pub(crate) const CONSUMER_HELD: u32 = 1;
/// Buffers the producer always keeps free for itself: the newest frame, and one being filled.
pub const MIN_PRODUCER_BUFFERS: u32 = 2;

fn ring_refused(error: impl std::fmt::Display) -> TicketRefused {
    TicketRefused {
        status: Status::IO,
        text: error.to_string(),
    }
}

fn io_refused(error: impl std::fmt::Display) -> TicketRefused {
    TicketRefused {
        status: Status::IO,
        text: error.to_string(),
    }
}
