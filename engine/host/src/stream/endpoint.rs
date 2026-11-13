//! The producer's end of a stream, and the state both ends share under one lock.

use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex, PoisonError};

use lemnos_core::{CallerId, Status};
use lemnos_hal::blocks::{BufferHandle, BufferKind, StreamFrame};
use lemnos_hal::{ErrorKind, StreamEndpoint};
use lemnos_wire::Planes;
use lemnos_wire::ring::{FrameSlot, Layout, Producer, RingError, STATE_CLOSED, STATE_FAULT};

use super::pool::{BufferPool, BufferState};
use super::{Backing, BufferMemory, Doorbell, Lease, StreamAllocator, StreamStats};
use crate::board::{StreamMemory, StreamSpec};

struct Attached {
    caller: CallerId,
    ring: Producer,
    buffers: Arc<dyn BufferMemory>,
    doorbell: Arc<dyn Doorbell>,
    /// One dma-buf per plane of each buffer, when the buffers are dma-bufs.
    handles: Option<Arc<[OwnedFd]>>,
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
}

pub(crate) struct StreamCore {
    layout: Layout,
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
    waiting: Option<Backing>,
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
    counters: Counters,
}

fn lock(core: &Mutex<StreamCore>) -> std::sync::MutexGuard<'_, StreamCore> {
    core.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The memory in place: its buffers, and their dma-bufs when it has them.
type InPlace<'a> = (&'a Arc<dyn BufferMemory>, Option<&'a Arc<[OwnedFd]>>);

impl StreamCore {
    pub(crate) fn new(spec: &StreamSpec, name: &str, allocator: Arc<dyn StreamAllocator>) -> Self {
        // A spec from a parsed board file has already been checked.
        let layout = spec.layout();
        let planes = Planes::new(&spec.plane_bytes(), layout.buffer_bytes)
            .unwrap_or(Planes::one(layout.buffer_bytes));
        Self {
            pool: BufferPool::new(layout.buffers),
            layout,
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
            counters: Counters::default(),
        }
    }

    pub(crate) const fn memory(&self) -> StreamMemory {
        self.memory
    }

    pub(crate) const fn is_attached(&self) -> bool {
        self.attached.is_some()
    }

    /// Memory for one consumer, as the board file declares it, checked against the layout.
    fn make(&self) -> Result<Backing, Status> {
        let backing = match self.memory {
            StreamMemory::Shared => self.allocator.allocate(&self.name, &self.layout)?,
            StreamMemory::DmaBuf => self.allocator.allocate_planes(
                &self.name,
                &self.layout,
                &self.heap,
                &self.planes,
            )?,
        };
        let expected = self.layout.buffers as usize * self.planes.count() as usize;
        if backing
            .handles
            .as_ref()
            .is_some_and(|handles| handles.len() != expected)
        {
            return Err(Status::IO);
        }
        // What the board file declared is what a driver was written against, and what its end
        // of the stream told it when it bound.
        if self.memory == StreamMemory::DmaBuf && backing.handles.is_none() {
            return Err(Status::IO);
        }
        Ok(Backing {
            handles: backing
                .handles
                .filter(|_| self.memory == StreamMemory::DmaBuf),
            ..backing
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
            Ok(backing) => {
                self.waiting = Some(backing);
                self.epoch += 1;
            }
            Err(_) => self.standby_failed = true,
        }
    }

    /// The buffers and their dma-bufs of the memory in place now: the consumer's, or what
    /// waits on standby for the next.
    fn in_place(&self) -> Option<InPlace<'_>> {
        match (&self.attached, &self.waiting) {
            (Some(attached), _) => Some((&attached.buffers, attached.handles.as_ref())),
            (None, Some(waiting)) => Some((&waiting.buffers, waiting.handles.as_ref())),
            (None, None) => None,
        }
    }

    /// Takes the consumer's end for `caller`: the memory waiting on standby, with the frame
    /// kept on it at once, or memory made now.
    pub(crate) fn attach(&mut self, caller: CallerId) -> Result<Lease, Status> {
        if self.attached.is_some() {
            return Err(Status::BUSY);
        }
        let (backing, adopted) = match self.waiting.take() {
            Some(backing) => (backing, true),
            None => (self.make()?, false),
        };
        let Backing {
            ring,
            buffers,
            doorbell,
            descriptors,
            handles,
        } = backing;
        let producer = match Producer::new(Arc::clone(&ring), self.layout) {
            Ok(producer) => producer,
            Err(_) => {
                // The memory is gone either way; the producer's buffers were on it.
                if adopted {
                    self.drop_kept();
                    self.epoch += 1;
                }
                return Err(Status::IO);
            }
        };
        if !adopted {
            self.epoch += 1;
        }
        self.counters.attachments += 1;
        self.attached = Some(Attached {
            caller,
            ring: producer,
            buffers: Arc::clone(&buffers),
            doorbell: Arc::clone(&doorbell),
            handles: handles.clone(),
        });
        if let Some(frame) = self.kept.take() {
            self.deliver(frame);
        }
        Ok(Lease {
            layout: self.layout,
            descriptors,
            ring,
            buffers,
            doorbell,
            handles,
            planes: self.planes,
        })
    }

    /// Ends the attachment with `state`, taking back everything the consumer had.
    pub(crate) fn detach(&mut self, state: u64) {
        let Some(attached) = self.attached.take() else {
            return;
        };
        attached.ring.close(state);
        attached.doorbell.ring();
        self.epoch += 1;
        self.pool.reclaim_published();
        // Fresh memory for the next consumer, if on standby, when the producer next asks.
        self.standby_failed = false;
    }

    /// The host is going: ends the attachment and lets go of any memory on standby.
    pub(crate) fn close(&mut self, state: u64) {
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

    /// Collects returned buffers. Bounded by the ring's capacity, so a peer that keeps writing
    /// can't hold this up.
    fn collect_returns(&mut self) {
        for _ in 0..self.layout.frame_slots {
            let Some(attached) = self.attached.as_mut() else {
                return;
            };
            match attached.ring.pop_return() {
                Ok(None) => return,
                Ok(Some((buffer, fence))) => {
                    if self.pool.release(buffer, fence).is_err() {
                        return self.fault();
                    }
                    self.counters.returned += 1;
                }
                Err(_) => return self.fault(),
            }
        }
    }

    fn next_buffer(&mut self) -> Option<u32> {
        self.collect_returns();
        self.stand_by();
        self.pool.take(self.epoch)
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

    /// Puts a frame the consumer attached now is due on its ring.
    fn deliver(&mut self, frame: StreamFrame) {
        let fence = self.pool.publish(frame.buffer);
        let Some(attached) = self.attached.as_mut() else {
            self.pool.drop_held(frame.buffer);
            return;
        };
        let pushed = attached.ring.push_frame(FrameSlot {
            buffer: frame.buffer,
            bytes: frame.bytes,
            sequence: frame.sequence,
            identity: frame.identity,
            timestamp_ns: frame.timestamp_ns,
            fence,
        });
        match pushed {
            Ok(true) => {
                attached.doorbell.ring();
                self.counters.delivered += 1;
            }
            Ok(false) => {
                self.pool.drop_held(frame.buffer);
                self.counters.dropped += 1;
            }
            Err(_) => self.fault(),
        }
    }

    fn next_job(&mut self, out: &mut [u8]) -> Result<Option<usize>, ErrorKind> {
        let Some(attached) = self.attached.as_mut() else {
            return Ok(None);
        };
        match attached.ring.pop_job(out) {
            Ok(job) => Ok(job),
            Err(RingError::JobTooLarge) => Err(ErrorKind::InvalidInput),
            Err(_) => {
                self.fault();
                Ok(None)
            }
        }
    }

    fn write(&self, buffer: u32, offset: usize, data: &[u8]) -> Result<bool, ErrorKind> {
        match self.pool.state(buffer) {
            Some(BufferState::Producer { .. }) => {}
            _ => return Err(ErrorKind::InvalidInput),
        }
        let in_buffer = offset
            .checked_add(data.len())
            .is_some_and(|end| end <= self.layout.buffer_bytes as usize);
        if !in_buffer {
            return Err(ErrorKind::InvalidInput);
        }
        let Some((buffers, _)) = self.in_place() else {
            return Ok(false);
        };
        let start = self
            .layout
            .buffer_offset(buffer)
            .and_then(|at| at.checked_add(offset))
            .ok_or(ErrorKind::InvalidInput)?;
        Ok(buffers.write(start, data))
    }

    fn read(&self, buffer: u32, offset: usize, out: &mut [u8]) -> Result<bool, ErrorKind> {
        match self.pool.state(buffer) {
            Some(BufferState::Producer { .. }) => {}
            _ => return Err(ErrorKind::InvalidInput),
        }
        let in_buffer = offset
            .checked_add(out.len())
            .is_some_and(|end| end <= self.layout.buffer_bytes as usize);
        if !in_buffer {
            return Err(ErrorKind::InvalidInput);
        }
        let Some((buffers, _)) = self.in_place() else {
            return Ok(false);
        };
        let start = self
            .layout
            .buffer_offset(buffer)
            .and_then(|at| at.checked_add(offset))
            .ok_or(ErrorKind::InvalidInput)?;
        Ok(buffers.read(start, out))
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
        let Some((_, handles)) = self.in_place() else {
            return Ok(None);
        };
        if handles.is_none() {
            return Err(ErrorKind::Unsupported);
        }
        Ok((epoch == self.epoch).then_some(epoch))
    }

    pub(crate) fn stats(&self) -> StreamStats {
        let (free, producer_held, consumer_held, kept) = self.pool.counts();
        StreamStats {
            attached: self.is_attached(),
            claimed: self.claimed,
            attachments: self.counters.attachments,
            published: self.counters.published,
            delivered: self.counters.delivered,
            dropped: self.counters.dropped,
            returned: self.counters.returned,
            faults: self.counters.faults,
            free,
            producer_held,
            consumer_held,
            kept,
            standby: self.waiting.is_some(),
        }
    }
}

/// The producer's end of a stream, given to a driver through `StreamHandle` and usable as a
/// software producer in tests. Dropping it gives back what the producer held, and a consumer
/// attached carries on.
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
            self.lent = core.in_place().and_then(|(_, handles)| handles.cloned());
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
