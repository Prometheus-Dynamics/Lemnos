//! The ring a frame stream shares between its producer and its one consumer. `docs/wire.md` is its
//! specification.
//!
//! The memory is shared with a peer that isn't trusted, so everything here is atomics over a slice
//! of `u64` words: reading a word the peer is writing is well defined, and a word is read once,
//! then checked. Every index and length read from the shared words is checked against the
//! capacities each side already holds, never against a value stored in the memory. Nothing here
//! panics, indexes out of bounds or loops on what a peer wrote; a malformed peer is an error, and
//! the side that saw it never looks at the ring again.
//!
//! Three queues live in the words, one writer each: frames (producer to consumer), returned
//! buffers and jobs (consumer to producer). Heads and tails count up forever and wrap only
//! at 2^64; the slot is the count modulo a power-of-two capacity.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Shared memory as 64-bit atomics. Implemented outside this crate over a mapping, and over the
/// heap for in-process use.
pub trait WordMemory: Send + Sync {
    fn words(&self) -> &[AtomicU64];
}

/// The frame buffers, as the consumer reads them.
pub trait ByteMemory: Send + Sync {
    fn bytes(&self) -> &[u8];
}

impl WordMemory for Box<[AtomicU64]> {
    fn words(&self) -> &[AtomicU64] {
        self
    }
}

/// `b"LMNSRING"`, stored in the first word.
pub const MAGIC: u64 = u64::from_le_bytes(*b"LMNSRING");
/// The ring's layout version, stored in the second word.
pub const LAYOUT_VERSION: u64 = 1;
/// The most buffers one stream has.
pub const MAX_BUFFERS: u32 = 4096;
/// The largest buffer.
pub const MAX_BUFFER_BYTES: u32 = 1 << 28;
/// The most buffer memory one stream maps.
pub const MAX_POOL_BYTES: u64 = 1 << 34;
/// The most jobs waiting at once.
pub const MAX_JOB_SLOTS: u32 = 64;
/// The largest job.
pub const MAX_JOB_BYTES: u32 = 4096;
/// Buffers start on multiples of this inside the buffer descriptor.
pub const BUFFER_ALIGN: u64 = 4096;
/// Words in one frame slot.
pub const FRAME_WORDS: usize = 5;

/// Where things are, in words from the start of the ring.
pub mod offsets {
    pub const MAGIC: usize = 0;
    pub const VERSION: usize = 1;
    pub const BUFFERS: usize = 2;
    pub const BUFFER_BYTES: usize = 3;
    pub const FRAME_SLOTS: usize = 4;
    pub const JOB_SLOTS: usize = 5;
    pub const JOB_BYTES: usize = 6;
    /// [`super::STATE_OPEN`], [`super::STATE_CLOSED`] or [`super::STATE_FAULT`]. Host writes.
    pub const STATE: usize = 8;
    /// Frames published. Producer writes.
    pub const FRAMES_HEAD: usize = 16;
    /// Frames taken. Consumer writes.
    pub const FRAMES_TAIL: usize = 24;
    /// Buffers returned. Consumer writes.
    pub const RETURNS_HEAD: usize = 32;
    /// Returned buffers collected. Producer writes.
    pub const RETURNS_TAIL: usize = 40;
    /// Jobs posted. Consumer writes.
    pub const JOBS_HEAD: usize = 48;
    /// Jobs collected. Producer writes.
    pub const JOBS_TAIL: usize = 56;
    /// The frame slots start here.
    pub const FRAMES: usize = 64;
}

/// The stream is running.
pub const STATE_OPEN: u64 = 0;
/// The stream ended normally.
pub const STATE_CLOSED: u64 = 1;
/// The stream ended because the consumer broke the rules.
pub const STATE_FAULT: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingError {
    /// The layout is outside the limits.
    BadLayout,
    /// The memory is smaller than the layout needs.
    TooSmall,
    /// The memory wasn't set up by this version of the producer.
    BadHeader,
    /// A shared head or tail is impossible.
    BadIndex,
    /// A buffer number is outside the pool.
    BadBuffer,
    /// A length is too large.
    BadLength,
    /// A buffer is already held by the consumer.
    BufferInUse,
    /// The buffer isn't held by this consumer.
    NotHeld,
    /// The job doesn't fit where it was asked to go. Nothing was consumed.
    JobTooLarge,
    /// This side already saw a malformed peer.
    Broken,
}

impl core::fmt::Display for RingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::BadLayout => "the stream layout is out of range",
            Self::TooSmall => "the shared memory is too small",
            Self::BadHeader => "the ring header doesn't match",
            Self::BadIndex => "an impossible ring index",
            Self::BadBuffer => "a buffer number outside the pool",
            Self::BadLength => "a length out of range",
            Self::BufferInUse => "a buffer already held",
            Self::NotHeld => "a buffer not held",
            Self::JobTooLarge => "a job too large for its destination",
            Self::Broken => "the stream's peer is malformed",
        })
    }
}

impl std::error::Error for RingError {}

/// What a stream's ring and buffers look like. Both sides hold their own copy and never trust the
/// shared words for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub buffers: u32,
    pub buffer_bytes: u32,
    pub frame_slots: u32,
    pub job_slots: u32,
    pub job_bytes: u32,
}

impl Layout {
    /// A layout with enough slots that a queue can only fill if its reader stops.
    pub fn new(
        buffers: u32,
        buffer_bytes: u32,
        job_slots: u32,
        job_bytes: u32,
    ) -> Result<Self, RingError> {
        Self {
            buffers,
            buffer_bytes,
            frame_slots: buffers.checked_next_power_of_two().unwrap_or(0).max(1),
            job_slots: job_slots.checked_next_power_of_two().unwrap_or(0).max(1),
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
            || self.frame_slots > 2 * MAX_BUFFERS
            || !self.job_slots.is_power_of_two()
            || self.job_slots > MAX_JOB_SLOTS
            || self.job_bytes > MAX_JOB_BYTES
            || self.pool_bytes() > MAX_POOL_BYTES;
        if bad {
            Err(RingError::BadLayout)
        } else {
            Ok(self)
        }
    }

    const fn job_words(&self) -> usize {
        1 + (self.job_bytes as usize).div_ceil(8)
    }

    const fn returns_at(&self) -> usize {
        offsets::FRAMES + self.frame_slots as usize * FRAME_WORDS
    }

    const fn jobs_at(&self) -> usize {
        self.returns_at() + self.frame_slots as usize
    }

    /// Words the ring occupies.
    pub const fn ring_words(&self) -> usize {
        self.jobs_at() + self.job_slots as usize * self.job_words()
    }

    /// Bytes the ring occupies.
    pub const fn ring_bytes(&self) -> usize {
        self.ring_words() * 8
    }

    /// Distance between buffers in the buffer descriptor.
    pub const fn stride(&self) -> u64 {
        (self.buffer_bytes as u64).next_multiple_of(BUFFER_ALIGN)
    }

    /// Bytes the buffer descriptor holds.
    pub const fn pool_bytes(&self) -> u64 {
        self.buffers as u64 * self.stride()
    }

    /// Where `buffer` starts, or `None` outside the pool.
    pub fn buffer_offset(&self, buffer: u32) -> Option<usize> {
        if buffer >= self.buffers {
            return None;
        }
        usize::try_from(u64::from(buffer) * self.stride()).ok()
    }
}

/// One published frame, as a ring slot holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSlot {
    pub buffer: u32,
    pub bytes: u32,
    pub sequence: u64,
    /// Means something only to the producer and the consumer.
    pub identity: u64,
    pub timestamp_ns: u64,
    /// What the consumer hands back with the buffer.
    pub fence: u32,
}

fn load(words: &[AtomicU64], at: usize, order: Ordering) -> Result<u64, RingError> {
    words
        .get(at)
        .map(|word| word.load(order))
        .ok_or(RingError::TooSmall)
}

fn store(words: &[AtomicU64], at: usize, value: u64, order: Ordering) -> Result<(), RingError> {
    words
        .get(at)
        .map(|word| word.store(value, order))
        .ok_or(RingError::TooSmall)
}

/// How many entries a reader may take, from the writer's head, or an error if that's impossible.
fn available(head: u64, tail: u64, slots: u32) -> Result<u64, RingError> {
    let count = head.wrapping_sub(tail);
    if count > u64::from(slots) {
        return Err(RingError::BadIndex);
    }
    Ok(count)
}

/// A writer's view of the reader's tail: it must lie between the last one seen and `head`, or the
/// reader is lying.
fn checked_tail(tail: u64, seen: u64, head: u64, slots: u32) -> Result<u64, RingError> {
    if tail.wrapping_sub(seen) > head.wrapping_sub(seen)
        || head.wrapping_sub(tail) > u64::from(slots)
    {
        return Err(RingError::BadIndex);
    }
    Ok(tail)
}

const fn slot_of(count: u64, slots: u32) -> usize {
    (count & (slots as u64 - 1)) as usize
}

/// The producer's end. Holds its own counters and capacities and writes the header once.
pub struct Producer {
    memory: Arc<dyn WordMemory>,
    layout: Layout,
    frames_head: u64,
    frames_seen: u64,
    returns_tail: u64,
    jobs_tail: u64,
    broken: bool,
}

impl core::fmt::Debug for Producer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Producer")
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}

impl Producer {
    /// Sets up fresh, zeroed memory.
    pub fn new(memory: Arc<dyn WordMemory>, layout: Layout) -> Result<Self, RingError> {
        let layout = layout.checked()?;
        let words = memory.words();
        if words.len() < layout.ring_words() {
            return Err(RingError::TooSmall);
        }
        for (at, value) in [
            (offsets::VERSION, LAYOUT_VERSION),
            (offsets::BUFFERS, u64::from(layout.buffers)),
            (offsets::BUFFER_BYTES, u64::from(layout.buffer_bytes)),
            (offsets::FRAME_SLOTS, u64::from(layout.frame_slots)),
            (offsets::JOB_SLOTS, u64::from(layout.job_slots)),
            (offsets::JOB_BYTES, u64::from(layout.job_bytes)),
            (offsets::STATE, STATE_OPEN),
        ] {
            store(words, at, value, Ordering::Relaxed)?;
        }
        store(words, offsets::MAGIC, MAGIC, Ordering::Release)?;
        Ok(Self {
            memory,
            layout,
            frames_head: 0,
            frames_seen: 0,
            returns_tail: 0,
            jobs_tail: 0,
            broken: false,
        })
    }

    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    fn guard(&self) -> Result<(), RingError> {
        if self.broken {
            return Err(RingError::Broken);
        }
        Ok(())
    }

    /// Keeps what the error says, and refuses from then on.
    fn fail<T>(&mut self, error: RingError) -> Result<T, RingError> {
        self.broken = true;
        Err(error)
    }

    /// Publishes a frame. `false` means the ring is full and nothing was written.
    pub fn push_frame(&mut self, frame: FrameSlot) -> Result<bool, RingError> {
        self.guard()?;
        let slots = self.layout.frame_slots;
        let memory = Arc::clone(&self.memory);
        let words = memory.words();
        let tail = load(words, offsets::FRAMES_TAIL, Ordering::Acquire);
        let tail =
            tail.and_then(|tail| checked_tail(tail, self.frames_seen, self.frames_head, slots));
        let tail = match tail {
            Ok(tail) => tail,
            Err(error) => return self.fail(error),
        };
        self.frames_seen = tail;
        if self.frames_head.wrapping_sub(tail) == u64::from(slots) {
            return Ok(false);
        }
        let at = offsets::FRAMES + slot_of(self.frames_head, slots) * FRAME_WORDS;
        let pair = u64::from(frame.buffer) | (u64::from(frame.bytes) << 32);
        for (offset, value) in [
            pair,
            frame.sequence,
            frame.identity,
            frame.timestamp_ns,
            u64::from(frame.fence),
        ]
        .into_iter()
        .enumerate()
        {
            store(words, at + offset, value, Ordering::Relaxed)?;
        }
        self.frames_head = self.frames_head.wrapping_add(1);
        store(
            words,
            offsets::FRAMES_HEAD,
            self.frames_head,
            Ordering::Release,
        )?;
        Ok(true)
    }

    /// The next returned buffer and its fence, or `None`. The caller checks them against the pool.
    pub fn pop_return(&mut self) -> Result<Option<(u32, u32)>, RingError> {
        self.guard()?;
        let slots = self.layout.frame_slots;
        let memory = Arc::clone(&self.memory);
        let words = memory.words();
        let head = load(words, offsets::RETURNS_HEAD, Ordering::Acquire)?;
        match available(head, self.returns_tail, slots) {
            Ok(0) => return Ok(None),
            Ok(_) => {}
            Err(error) => return self.fail(error),
        }
        let at = self.layout.returns_at() + slot_of(self.returns_tail, slots);
        let word = load(words, at, Ordering::Relaxed)?;
        self.returns_tail = self.returns_tail.wrapping_add(1);
        store(
            words,
            offsets::RETURNS_TAIL,
            self.returns_tail,
            Ordering::Release,
        )?;
        Ok(Some((word as u32, (word >> 32) as u32)))
    }

    /// Copies the next job into `out` and returns its length. A job too large for `out` stays and
    /// is [`RingError::JobTooLarge`]: that's the caller's error, not the peer's.
    pub fn pop_job(&mut self, out: &mut [u8]) -> Result<Option<usize>, RingError> {
        self.guard()?;
        let slots = self.layout.job_slots;
        let memory = Arc::clone(&self.memory);
        let words = memory.words();
        let head = load(words, offsets::JOBS_HEAD, Ordering::Acquire)?;
        match available(head, self.jobs_tail, slots) {
            Ok(0) => return Ok(None),
            Ok(_) => {}
            Err(error) => return self.fail(error),
        }
        let at = self.layout.jobs_at() + slot_of(self.jobs_tail, slots) * self.layout.job_words();
        let len = load(words, at, Ordering::Relaxed)?;
        if len > u64::from(self.layout.job_bytes) {
            return self.fail(RingError::BadLength);
        }
        let len = len as usize;
        let into = out.get_mut(..len).ok_or(RingError::JobTooLarge)?;
        for (index, chunk) in into.chunks_mut(8).enumerate() {
            let word = load(words, at + 1 + index, Ordering::Relaxed)?;
            let bytes = word.to_le_bytes();
            let part = bytes.get(..chunk.len()).ok_or(RingError::BadLength)?;
            chunk.copy_from_slice(part);
        }
        self.jobs_tail = self.jobs_tail.wrapping_add(1);
        store(words, offsets::JOBS_TAIL, self.jobs_tail, Ordering::Release)?;
        Ok(Some(len))
    }

    /// Tells the consumer the stream has ended.
    pub fn close(&self, state: u64) {
        let _ = store(
            self.memory.words(),
            offsets::STATE,
            state,
            Ordering::Release,
        );
    }
}

/// The consumer's end.
pub struct Consumer {
    memory: Arc<dyn WordMemory>,
    layout: Layout,
    frames_tail: u64,
    returns_head: u64,
    returns_seen: u64,
    jobs_head: u64,
    jobs_seen: u64,
    held: Vec<bool>,
    broken: bool,
}

impl core::fmt::Debug for Consumer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Consumer")
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}

impl Consumer {
    /// Attaches to a ring the producer has set up, refusing one that doesn't match `layout`.
    pub fn attach(memory: Arc<dyn WordMemory>, layout: Layout) -> Result<Self, RingError> {
        let layout = layout.checked()?;
        let words = memory.words();
        if words.len() < layout.ring_words() {
            return Err(RingError::TooSmall);
        }
        let header = [
            (offsets::MAGIC, MAGIC),
            (offsets::VERSION, LAYOUT_VERSION),
            (offsets::BUFFERS, u64::from(layout.buffers)),
            (offsets::BUFFER_BYTES, u64::from(layout.buffer_bytes)),
            (offsets::FRAME_SLOTS, u64::from(layout.frame_slots)),
            (offsets::JOB_SLOTS, u64::from(layout.job_slots)),
            (offsets::JOB_BYTES, u64::from(layout.job_bytes)),
        ];
        for (at, expected) in header {
            if load(words, at, Ordering::Acquire)? != expected {
                return Err(RingError::BadHeader);
            }
        }
        Ok(Self {
            memory,
            layout,
            frames_tail: 0,
            returns_head: 0,
            returns_seen: 0,
            jobs_head: 0,
            jobs_seen: 0,
            held: vec![false; layout.buffers as usize],
            broken: false,
        })
    }

    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The ring's state: [`STATE_OPEN`] while running. An unknown value reads as a fault.
    pub fn state(&self) -> u64 {
        match load(self.memory.words(), offsets::STATE, Ordering::Acquire) {
            Ok(state @ (STATE_OPEN | STATE_CLOSED)) => state,
            _ => STATE_FAULT,
        }
    }

    fn guard(&self) -> Result<(), RingError> {
        if self.broken {
            return Err(RingError::Broken);
        }
        Ok(())
    }

    fn fail<T>(&mut self, error: RingError) -> Result<T, RingError> {
        self.broken = true;
        Err(error)
    }

    /// The next frame, or `None`. The buffer counts as held from here until [`Self::release`].
    pub fn pop_frame(&mut self) -> Result<Option<FrameSlot>, RingError> {
        self.guard()?;
        let slots = self.layout.frame_slots;
        let memory = Arc::clone(&self.memory);
        let words = memory.words();
        let head = load(words, offsets::FRAMES_HEAD, Ordering::Acquire)?;
        match available(head, self.frames_tail, slots) {
            Ok(0) => return Ok(None),
            Ok(_) => {}
            Err(error) => return self.fail(error),
        }
        let at = offsets::FRAMES + slot_of(self.frames_tail, slots) * FRAME_WORDS;
        let pair = load(words, at, Ordering::Relaxed)?;
        let frame = FrameSlot {
            buffer: pair as u32,
            bytes: (pair >> 32) as u32,
            sequence: load(words, at + 1, Ordering::Relaxed)?,
            identity: load(words, at + 2, Ordering::Relaxed)?,
            timestamp_ns: load(words, at + 3, Ordering::Relaxed)?,
            fence: load(words, at + 4, Ordering::Relaxed)? as u32,
        };
        if frame.bytes > self.layout.buffer_bytes {
            return self.fail(RingError::BadLength);
        }
        let Some(held) = self.held.get_mut(frame.buffer as usize) else {
            return self.fail(RingError::BadBuffer);
        };
        if *held {
            return self.fail(RingError::BufferInUse);
        }
        *held = true;
        self.frames_tail = self.frames_tail.wrapping_add(1);
        store(
            words,
            offsets::FRAMES_TAIL,
            self.frames_tail,
            Ordering::Release,
        )?;
        Ok(Some(frame))
    }

    /// Gives a buffer back. `false` means the return queue is full and nothing changed: that
    /// happens only while the producer has stopped collecting.
    pub fn release(&mut self, buffer: u32, fence: u32) -> Result<bool, RingError> {
        self.guard()?;
        let slots = self.layout.frame_slots;
        let memory = Arc::clone(&self.memory);
        let words = memory.words();
        if !self.held.get(buffer as usize).copied().unwrap_or(false) {
            return Err(RingError::NotHeld);
        }
        let tail = load(words, offsets::RETURNS_TAIL, Ordering::Acquire);
        let tail =
            tail.and_then(|tail| checked_tail(tail, self.returns_seen, self.returns_head, slots));
        let tail = match tail {
            Ok(tail) => tail,
            Err(error) => return self.fail(error),
        };
        self.returns_seen = tail;
        if self.returns_head.wrapping_sub(tail) == u64::from(slots) {
            return Ok(false);
        }
        let at = self.layout.returns_at() + slot_of(self.returns_head, slots);
        let word = u64::from(buffer) | (u64::from(fence) << 32);
        store(words, at, word, Ordering::Relaxed)?;
        self.returns_head = self.returns_head.wrapping_add(1);
        store(
            words,
            offsets::RETURNS_HEAD,
            self.returns_head,
            Ordering::Release,
        )?;
        if let Some(held) = self.held.get_mut(buffer as usize) {
            *held = false;
        }
        Ok(true)
    }

    /// Posts a job for the producer. `false` means the job queue is full.
    pub fn push_job(&mut self, job: &[u8]) -> Result<bool, RingError> {
        self.guard()?;
        let slots = self.layout.job_slots;
        if job.len() > self.layout.job_bytes as usize {
            return Err(RingError::BadLength);
        }
        let memory = Arc::clone(&self.memory);
        let words = memory.words();
        let tail = load(words, offsets::JOBS_TAIL, Ordering::Acquire);
        let tail = tail.and_then(|tail| checked_tail(tail, self.jobs_seen, self.jobs_head, slots));
        let tail = match tail {
            Ok(tail) => tail,
            Err(error) => return self.fail(error),
        };
        self.jobs_seen = tail;
        if self.jobs_head.wrapping_sub(tail) == u64::from(slots) {
            return Ok(false);
        }
        let at = self.layout.jobs_at() + slot_of(self.jobs_head, slots) * self.layout.job_words();
        store(words, at, job.len() as u64, Ordering::Relaxed)?;
        for (index, chunk) in job.chunks(8).enumerate() {
            let mut bytes = [0u8; 8];
            let to = bytes.get_mut(..chunk.len()).ok_or(RingError::BadLength)?;
            to.copy_from_slice(chunk);
            store(
                words,
                at + 1 + index,
                u64::from_le_bytes(bytes),
                Ordering::Relaxed,
            )?;
        }
        self.jobs_head = self.jobs_head.wrapping_add(1);
        store(words, offsets::JOBS_HEAD, self.jobs_head, Ordering::Release)?;
        Ok(true)
    }
}

/// Zeroed words on the heap: a ring within one process.
#[must_use]
pub fn heap_words(words: usize) -> Arc<dyn WordMemory> {
    let memory: Box<[AtomicU64]> = (0..words).map(|_| AtomicU64::new(0)).collect();
    Arc::new(memory)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::new(4, 256, 2, 16).unwrap()
    }

    fn frame(buffer: u32, sequence: u64) -> FrameSlot {
        FrameSlot {
            buffer,
            bytes: 16,
            sequence,
            identity: sequence + 100,
            timestamp_ns: sequence * 10,
            fence: 1,
        }
    }

    fn pair() -> (Arc<dyn WordMemory>, Producer, Consumer) {
        let layout = layout();
        let memory = heap_words(layout.ring_words());
        let producer = Producer::new(Arc::clone(&memory), layout).unwrap();
        let consumer = Consumer::attach(Arc::clone(&memory), layout).unwrap();
        (memory, producer, consumer)
    }

    fn set(memory: &dyn WordMemory, at: usize, value: u64) {
        memory.words()[at].store(value, Ordering::SeqCst);
    }

    #[test]
    fn frames_arrive_in_order_with_their_fields() {
        let (_, mut producer, mut consumer) = pair();
        for i in 0..4 {
            assert!(producer.push_frame(frame(i, u64::from(i))).unwrap());
        }
        for i in 0..4 {
            assert_eq!(
                consumer.pop_frame().unwrap(),
                Some(frame(i, u64::from(i))),
                "frame {i}"
            );
        }
        assert_eq!(consumer.pop_frame().unwrap(), None);
    }

    #[test]
    fn the_rings_wrap_and_stay_correct_over_many_cycles() {
        let (_, mut producer, mut consumer) = pair();
        for sequence in 0..10_000u64 {
            let buffer = (sequence % 4) as u32;
            assert!(producer.push_frame(frame(buffer, sequence)).unwrap());
            let got = consumer.pop_frame().unwrap().unwrap();
            assert_eq!(got, frame(buffer, sequence));
            assert!(consumer.release(buffer, 7).unwrap());
            assert_eq!(producer.pop_return().unwrap(), Some((buffer, 7)));
        }
    }

    #[test]
    fn a_full_frame_ring_reports_full_and_changes_nothing() {
        let (_, mut producer, mut consumer) = pair();
        for i in 0..4 {
            assert!(producer.push_frame(frame(i, u64::from(i))).unwrap());
        }
        assert!(!producer.push_frame(frame(0, 9)).unwrap());
        assert_eq!(consumer.pop_frame().unwrap(), Some(frame(0, 0)));
    }

    #[test]
    fn jobs_round_trip_with_their_bytes_and_a_short_destination_keeps_them() {
        let (_, mut producer, mut consumer) = pair();
        assert!(consumer.push_job(&[1, 2, 3, 4, 5, 6, 7, 8, 9]).unwrap());
        assert!(consumer.push_job(&[]).unwrap());
        assert!(!consumer.push_job(&[1]).unwrap(), "two slots");
        let mut small = [0u8; 2];
        assert_eq!(producer.pop_job(&mut small), Err(RingError::JobTooLarge));
        let mut out = [0u8; 16];
        assert_eq!(producer.pop_job(&mut out).unwrap(), Some(9));
        assert_eq!(&out[..9], [1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(producer.pop_job(&mut out).unwrap(), Some(0));
        assert_eq!(producer.pop_job(&mut out).unwrap(), None);
    }

    #[test]
    fn an_impossible_frame_head_is_an_error_and_sticks() {
        let (memory, _producer, mut consumer) = pair();
        set(memory.as_ref(), offsets::FRAMES_HEAD, u64::MAX);
        assert_eq!(consumer.pop_frame(), Err(RingError::BadIndex));
        set(memory.as_ref(), offsets::FRAMES_HEAD, 0);
        assert_eq!(consumer.pop_frame(), Err(RingError::Broken));
    }

    #[test]
    fn a_frame_head_just_past_the_capacity_is_refused() {
        let (memory, _producer, mut consumer) = pair();
        set(memory.as_ref(), offsets::FRAMES_HEAD, 5);
        assert_eq!(consumer.pop_frame(), Err(RingError::BadIndex));
    }

    #[test]
    fn a_frame_naming_a_buffer_outside_the_pool_is_refused() {
        let (memory, _producer, mut consumer) = pair();
        set(memory.as_ref(), offsets::FRAMES, 4);
        set(memory.as_ref(), offsets::FRAMES_HEAD, 1);
        assert_eq!(consumer.pop_frame(), Err(RingError::BadBuffer));
    }

    #[test]
    fn a_frame_longer_than_a_buffer_is_refused() {
        let (memory, _producer, mut consumer) = pair();
        set(memory.as_ref(), offsets::FRAMES, 257 << 32);
        set(memory.as_ref(), offsets::FRAMES_HEAD, 1);
        assert_eq!(consumer.pop_frame(), Err(RingError::BadLength));
    }

    #[test]
    fn a_frame_on_a_buffer_the_consumer_holds_is_refused() {
        let (_, mut producer, mut consumer) = pair();
        assert!(producer.push_frame(frame(1, 1)).unwrap());
        assert!(producer.push_frame(frame(1, 2)).unwrap());
        assert!(consumer.pop_frame().unwrap().is_some());
        assert_eq!(consumer.pop_frame(), Err(RingError::BufferInUse));
    }

    #[test]
    fn a_buffer_cant_be_released_twice_or_unheld() {
        let (_, mut producer, mut consumer) = pair();
        assert_eq!(consumer.release(0, 1), Err(RingError::NotHeld));
        assert_eq!(consumer.release(99, 1), Err(RingError::NotHeld));
        producer.push_frame(frame(0, 1)).unwrap();
        consumer.pop_frame().unwrap();
        assert!(consumer.release(0, 1).unwrap());
        assert_eq!(consumer.release(0, 1), Err(RingError::NotHeld));
    }

    #[test]
    fn a_producer_refuses_an_impossible_frames_tail() {
        let (memory, mut producer, _consumer) = pair();
        producer.push_frame(frame(0, 0)).unwrap();
        // Beyond the head.
        set(memory.as_ref(), offsets::FRAMES_TAIL, 9);
        assert_eq!(producer.push_frame(frame(0, 1)), Err(RingError::BadIndex));
    }

    #[test]
    fn a_tail_going_backwards_is_refused() {
        let (memory, mut producer, mut consumer) = pair();
        for i in 0..3 {
            producer.push_frame(frame(i, 0)).unwrap();
            consumer.pop_frame().unwrap();
        }
        producer.push_frame(frame(3, 0)).unwrap();
        set(memory.as_ref(), offsets::FRAMES_TAIL, 0);
        assert_eq!(producer.push_frame(frame(0, 1)), Err(RingError::BadIndex));
    }

    #[test]
    fn an_impossible_returns_head_is_refused() {
        let (memory, mut producer, _consumer) = pair();
        set(memory.as_ref(), offsets::RETURNS_HEAD, 1 << 40);
        assert_eq!(producer.pop_return(), Err(RingError::BadIndex));
        assert_eq!(producer.pop_return(), Err(RingError::Broken));
    }

    #[test]
    fn a_job_length_past_the_slot_is_refused_and_a_negative_one_too() {
        let layout = layout();
        let (memory, mut producer, _consumer) = pair();
        set(memory.as_ref(), layout.jobs_at(), 17);
        set(memory.as_ref(), offsets::JOBS_HEAD, 1);
        let mut out = [0u8; 64];
        assert_eq!(producer.pop_job(&mut out), Err(RingError::BadLength));
        let (memory, mut producer, _consumer) = pair();
        set(memory.as_ref(), layout.jobs_at(), u64::MAX);
        set(memory.as_ref(), offsets::JOBS_HEAD, 1);
        assert_eq!(producer.pop_job(&mut out), Err(RingError::BadLength));
    }

    #[test]
    fn a_consumer_refuses_a_header_that_doesnt_match() {
        let layout = layout();
        let memory = heap_words(layout.ring_words());
        let _producer = Producer::new(Arc::clone(&memory), layout).unwrap();
        set(memory.as_ref(), offsets::BUFFERS, 1000);
        assert_eq!(
            Consumer::attach(Arc::clone(&memory), layout).unwrap_err(),
            RingError::BadHeader
        );
        let blank = heap_words(layout.ring_words());
        assert_eq!(
            Consumer::attach(blank, layout).unwrap_err(),
            RingError::BadHeader
        );
    }

    #[test]
    fn memory_smaller_than_the_layout_is_refused_on_both_sides() {
        let layout = layout();
        let short = heap_words(layout.ring_words() - 1);
        assert_eq!(
            Producer::new(Arc::clone(&short), layout).unwrap_err(),
            RingError::TooSmall
        );
        assert_eq!(
            Consumer::attach(short, layout).unwrap_err(),
            RingError::TooSmall
        );
    }

    #[test]
    fn layouts_out_of_range_are_refused() {
        let good = layout();
        assert!(Layout::new(0, 1, 1, 0).is_err());
        assert!(Layout::new(MAX_BUFFERS + 1, 1, 1, 0).is_err());
        assert!(Layout::new(1, 0, 1, 0).is_err());
        assert!(Layout::new(1, MAX_BUFFER_BYTES + 1, 1, 0).is_err());
        assert!(Layout::new(1, 1, 1, MAX_JOB_BYTES + 1).is_err());
        assert!(Layout::new(MAX_BUFFERS, MAX_BUFFER_BYTES, 1, 0).is_err());
        for bad in [
            Layout {
                frame_slots: 3,
                ..good
            },
            Layout {
                frame_slots: 2,
                ..good
            },
            Layout {
                job_slots: 0,
                ..good
            },
            Layout {
                job_slots: 128,
                ..good
            },
            Layout {
                frame_slots: 1 << 30,
                ..good
            },
        ] {
            assert_eq!(bad.checked(), Err(RingError::BadLayout), "{bad:?}");
        }
        assert_eq!(good.checked(), Ok(good));
    }

    #[test]
    fn buffers_sit_at_multiples_of_the_stride() {
        let layout = Layout::new(3, 5000, 1, 0).unwrap();
        assert_eq!(layout.stride(), 8192);
        assert_eq!(layout.buffer_offset(0), Some(0));
        assert_eq!(layout.buffer_offset(2), Some(16384));
        assert_eq!(layout.buffer_offset(3), None);
        assert_eq!(layout.pool_bytes(), 3 * 8192);
    }

    #[test]
    fn the_state_word_reads_as_a_fault_when_unknown() {
        let (memory, producer, consumer) = pair();
        assert_eq!(consumer.state(), STATE_OPEN);
        producer.close(STATE_CLOSED);
        assert_eq!(consumer.state(), STATE_CLOSED);
        set(memory.as_ref(), offsets::STATE, 99);
        assert_eq!(consumer.state(), STATE_FAULT);
    }
}
