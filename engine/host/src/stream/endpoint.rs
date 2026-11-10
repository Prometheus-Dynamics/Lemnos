//! The producer's end of a stream, and the state both ends share under one lock.

use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex, PoisonError};

use lemnos_core::{CallerId, Status};
use lemnos_hal::blocks::{BufferHandle, StreamFrame};
use lemnos_hal::{ErrorKind, StreamEndpoint};
use lemnos_wire::ring::{
    FrameSlot, Layout, Producer, RingError, STATE_CLOSED, STATE_FAULT, WordMemory,
};

use super::pool::{BufferPool, BufferState};
use super::{BufferMemory, Doorbell, StreamStats};
use crate::board::{StreamMemory, StreamSpec};

struct Attached {
    caller: CallerId,
    ring: Producer,
    buffers: Arc<dyn BufferMemory>,
    doorbell: Arc<dyn Doorbell>,
    /// One dma-buf per buffer, when the buffers are dma-bufs.
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
    pool: BufferPool,
    attached: Option<Attached>,
    claimed: bool,
    /// Up by one per attachment and per detachment, so a buffer granted before a consumer came or
    /// went is known to be on memory it no longer has.
    epoch: u64,
    counters: Counters,
}

fn lock(core: &Mutex<StreamCore>) -> std::sync::MutexGuard<'_, StreamCore> {
    core.lock().unwrap_or_else(PoisonError::into_inner)
}

impl StreamCore {
    pub(crate) fn new(spec: &StreamSpec) -> Self {
        // A spec from a parsed board file has already been checked.
        let layout = spec.layout();
        Self {
            pool: BufferPool::new(layout.buffers),
            layout,
            memory: spec.memory,
            heap: spec.heap_name().into(),
            attached: None,
            claimed: false,
            epoch: 0,
            counters: Counters::default(),
        }
    }

    pub(crate) const fn layout(&self) -> Layout {
        self.layout
    }

    pub(crate) const fn memory(&self) -> StreamMemory {
        self.memory
    }

    pub(crate) fn heap(&self) -> &str {
        &self.heap
    }

    pub(crate) const fn is_attached(&self) -> bool {
        self.attached.is_some()
    }

    pub(crate) fn attach(
        &mut self,
        caller: CallerId,
        ring: Arc<dyn WordMemory>,
        buffers: Arc<dyn BufferMemory>,
        doorbell: Arc<dyn Doorbell>,
        handles: Option<Arc<[OwnedFd]>>,
    ) -> Result<(), Status> {
        let producer = Producer::new(ring, self.layout).map_err(|_| Status::IO)?;
        self.epoch += 1;
        self.counters.attachments += 1;
        self.attached = Some(Attached {
            caller,
            ring: producer,
            buffers,
            doorbell,
            handles,
        });
        Ok(())
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
        if !current || self.attached.is_none() {
            self.pool.drop_held(frame.buffer);
            self.counters.dropped += 1;
            return Ok(());
        }
        let fence = self.pool.publish(frame.buffer);
        let Some(attached) = self.attached.as_mut() else {
            return Ok(());
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
        Ok(())
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
        let Some(attached) = self.attached.as_ref() else {
            return Ok(false);
        };
        let start = self
            .layout
            .buffer_offset(buffer)
            .and_then(|at| at.checked_add(offset))
            .ok_or(ErrorKind::InvalidInput)?;
        Ok(attached.buffers.write(start, data))
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
        let Some(attached) = self.attached.as_ref() else {
            return Ok(false);
        };
        let start = self
            .layout
            .buffer_offset(buffer)
            .and_then(|at| at.checked_add(offset))
            .ok_or(ErrorKind::InvalidInput)?;
        Ok(attached.buffers.read(start, out))
    }

    /// Whether `buffer`'s handle can be lent: the epoch it was taken in when that attachment
    /// still stands, `None` when nothing is behind it now. Refused for a buffer the producer
    /// doesn't hold, and `Unsupported` when the attached buffers aren't dma-bufs.
    fn lendable(&self, buffer: u32) -> Result<Option<u64>, ErrorKind> {
        let Some(BufferState::Producer { epoch }) = self.pool.state(buffer) else {
            return Err(ErrorKind::InvalidInput);
        };
        let Some(attached) = self.attached.as_ref() else {
            return Ok(None);
        };
        if attached.handles.is_none() {
            return Err(ErrorKind::Unsupported);
        }
        Ok((epoch == self.epoch).then_some(epoch))
    }

    pub(crate) fn stats(&self) -> StreamStats {
        let (free, producer_held, consumer_held) = self.pool.counts();
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
        }
    }
}

/// The producer's end of a stream, given to a driver through `StreamHandle` and usable as a
/// software producer in tests. Dropping it gives back what the producer held, and a consumer
/// attached carries on.
pub struct ProducerEndpoint {
    core: Arc<Mutex<StreamCore>>,
    buffers: u32,
    /// The dma-bufs of the attachment the last buffer was taken in, kept here so one can be
    /// lent for as long as this end is borrowed. Changed only when a buffer is taken, which
    /// needs this end exclusively, so never while a handle is out.
    lent: Option<Arc<[OwnedFd]>>,
    /// The epoch `lent` belongs to.
    lent_epoch: u64,
}

impl ProducerEndpoint {
    pub(super) fn claim(core: Arc<Mutex<StreamCore>>) -> Result<Self, Status> {
        let buffers = {
            let mut guard = lock(&core);
            if guard.claimed {
                return Err(Status::BUSY);
            }
            guard.claimed = true;
            guard.layout.buffers
        };
        Ok(Self {
            core,
            buffers,
            lent: None,
            lent_epoch: 0,
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

    fn publish(&mut self, frame: StreamFrame) -> Result<(), ErrorKind> {
        lock(&self.core).publish(frame)
    }

    fn next_buffer(&mut self) -> Result<Option<u32>, ErrorKind> {
        let mut core = lock(&self.core);
        let buffer = core.next_buffer();
        if buffer.is_some() && self.lent_epoch != core.epoch {
            // Cloning shares the descriptors, and allocates nothing.
            self.lent = core.attached.as_ref().and_then(|a| a.handles.clone());
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

    fn buffer_handle(&self, buffer: u32) -> Result<Option<BufferHandle<'_>>, ErrorKind> {
        let current = lock(&self.core).lendable(buffer)?;
        // A buffer taken in the attachment that still stands was taken through this end, so
        // `lent` is that attachment's.
        if current != Some(self.lent_epoch) {
            return Ok(None);
        }
        let handles = self.lent.as_ref();
        Ok(handles
            .and_then(|handles| handles.get(buffer as usize))
            .map(|fd| BufferHandle::from(fd.as_fd())))
    }
}

impl Drop for ProducerEndpoint {
    fn drop(&mut self) {
        let mut core = lock(&self.core);
        core.claimed = false;
        core.pool.reclaim_producer();
    }
}
