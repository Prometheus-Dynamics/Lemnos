//! A subscriber of a buffer topic.

use std::os::fd::BorrowedFd;
use std::sync::Arc;

use shm_ring::{FieldId, Mem, Next, ReadMap, Reader, SampleTime};

use crate::{BufferDesc, BufferIndex, MAX_PER_FRAME, Marker, NoMarks, PoolView, entry, page};

/// How many entries one [`BufferReader::next`] call steps over looking for a frame that is still
/// there.
const PROBES: u32 = 64;

/// What a reader shares with the frames it has handed out.
#[derive(Debug)]
struct Shared<K> {
    pool: PoolView,
    marker: K,
}

/// The buffers of one frame, held: the publisher will not reuse them until this is dropped.
/// Reading them is reading the publisher's memory in place. Nothing was copied.
///
/// A `Held` is independent of the reader it came from. Keep several, send them to other
/// threads; each buffer is released when its `Held` is dropped.
///
/// With [`NoMarks`] nothing is actually held, and the publisher may write a buffer while a slice
/// of it is in hand. The slice is a `&[u8]`, which the compiler may assume does not change, so
/// such a reader can see a mix of old and new bytes or keep seeing old ones. That is for tests
/// and tools that only pass descriptors on. Anything that reads frames holds them.
#[derive(Debug)]
pub struct Held<K: Marker = NoMarks> {
    shared: Arc<Shared<K>>,
    buffers: [BufferIndex; MAX_PER_FRAME],
    count: usize,
}

impl<K: Marker> Held<K> {
    /// How many buffers the frame has.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.count
    }

    /// Whether the frame has no buffers. It never does.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Which buffer of the pool the frame's buffer `index` is.
    #[must_use]
    pub fn buffer(&self, index: usize) -> Option<BufferIndex> {
        self.buffers[..self.count].get(index).copied()
    }

    /// The bytes of the frame's buffer `index`, in place.
    #[must_use]
    pub fn bytes(&self, index: usize) -> Option<&[u8]> {
        self.shared.pool.bytes(self.buffer(index)?)
    }

    /// The descriptor behind the frame's buffer `index` and where the buffer is in it, for
    /// handing the buffer to something else instead of reading it.
    #[must_use]
    pub fn descriptor(&self, index: usize) -> Option<(BorrowedFd<'_>, BufferDesc)> {
        self.shared.pool.descriptor(self.buffer(index)?)
    }
}

impl<K: Marker> Drop for Held<K> {
    fn drop(&mut self) {
        for buffer in &self.buffers[..self.count] {
            self.shared.marker.unmark(*buffer);
        }
    }
}

/// One batch: the buffers one entry names, published together (a frame and its pyramid levels,
/// for example).
#[derive(Debug)]
pub struct Batch<'a, K: Marker = NoMarks> {
    /// Its number in the order of publication.
    pub seq: u64,
    /// The time it belongs to.
    pub time: SampleTime,
    /// The publisher's metadata, passed on unread. Valid until the next call on the reader.
    pub meta: &'a [u8],
    /// Its buffers. Move this out to keep the frame past the next call on the reader.
    pub held: Held<K>,
}

/// An entry that names no buffer, as [`NextBatch::NoBuffers`] hands it out.
#[derive(Debug)]
pub struct NoBuffers<'a> {
    /// Its number in the order of publication.
    pub seq: u64,
    /// The time it belongs to.
    pub time: SampleTime,
    /// The publisher's metadata, passed on unread. Valid until the next call on the reader.
    pub meta: &'a [u8],
}

/// What [`BufferReader::next`] found.
#[derive(Debug)]
pub enum NextBatch<'a, K: Marker = NoMarks> {
    /// The next frame: one or more buffers, held. Never an entry with no buffer.
    ///
    /// A reader that matched this arm as `NextBatch::Batch` no longer compiles, which is the
    /// point: an entry that names no buffer is now [`NextBatch::NoBuffers`], and a match on the
    /// old name has to be changed by hand.
    ///
    /// ```compile_fail
    /// use shm_ring_buffers::NextBatch;
    /// fn old_reader(next: NextBatch<'_>) {
    ///     if let NextBatch::Batch(_frame) = next {}
    /// }
    /// ```
    Frame(Batch<'a, K>),
    /// An entry that names no buffer: a job's end, or any other entry of metadata alone. Only
    /// its sequence, time and metadata are here; the metadata's layout is its publisher's.
    NoBuffers(NoBuffers<'a>),
    /// Nothing new.
    Empty,
    /// This many frames were gone before the reader got to them: overwritten in the ring, or
    /// their buffers already taken back.
    Skipped(u64),
    /// The publisher replaced its pool. Frames now name buffers of this generation, which the
    /// reader doesn't have yet: get the new pool and call [`BufferReader::set_pool`].
    PoolChanged {
        /// The new pool's generation.
        generation: u32,
    },
    /// The reader holds as many buffers as it is allowed. The frame was skipped; let go of
    /// one to get more.
    Limited,
    /// The publisher cut this reader off for breaking its limits. It gets no more frames, and
    /// frames it still holds may change.
    CutOff,
}

/// Reads frames of a buffer topic.
#[derive(Debug)]
pub struct BufferReader<M: Mem = ReadMap, K: Marker = NoMarks> {
    reader: Reader<M>,
    field: FieldId,
    shared: Arc<Shared<K>>,
    meta: Box<[u8]>,
    /// An entry found behind lost ones, kept for the next call.
    pending: Option<Pending<K>>,
    missed: u64,
}

/// An entry kept for the next call: a frame, or an entry that names no buffer.
#[derive(Debug)]
enum Pending<K: Marker> {
    Frame {
        seq: u64,
        time: SampleTime,
        meta: usize,
        held: Held<K>,
    },
    NoBuffers {
        seq: u64,
        time: SampleTime,
        meta: usize,
    },
}

/// What one step found, with nothing borrowed.
enum Step<K: Marker> {
    Frame {
        seq: u64,
        time: SampleTime,
        meta: usize,
        held: Held<K>,
    },
    NoBuffers {
        seq: u64,
        time: SampleTime,
        meta: usize,
    },
    Gone,
    Other,
    Empty,
    Skipped(u64),
    PoolChanged(u32),
    Limited,
}

impl<M: Mem, K: Marker> BufferReader<M, K> {
    /// Reads the frames published on `field` of `reader`'s ring, whose buffers are in `pool`,
    /// holding them through `marker`.
    #[must_use]
    pub fn new(reader: Reader<M>, field: FieldId, pool: PoolView, marker: K) -> Self {
        let meta = vec![0; reader.geometry().max_payload() as usize].into_boxed_slice();
        Self {
            reader,
            field,
            shared: Arc::new(Shared { pool, marker }),
            meta,
            pending: None,
            missed: 0,
        }
    }

    /// The end code the owner recorded for the topic, 0 while it runs. See
    /// [`shm_ring::Ring::set_end`].
    #[must_use]
    pub fn end(&self) -> u32 {
        self.reader.end()
    }

    /// How many entries have been published that [`BufferReader::next`] has not looked at
    /// yet. One memory read.
    #[must_use]
    pub fn pending(&self) -> u64 {
        self.reader.pending() + u64::from(self.pending.is_some())
    }

    /// The pool this reader has.
    #[must_use]
    pub fn pool(&self) -> &PoolView {
        &self.shared.pool
    }

    /// How many frames this reader has been told it missed, in total.
    #[must_use]
    pub const fn missed(&self) -> u64 {
        self.missed
    }

    /// Switches to a new pool, with the marker for it. Frames of the old pool that are still
    /// held keep it mapped, and marked, until they are dropped.
    pub fn set_pool(&mut self, pool: PoolView, marker: K) {
        self.pending = None;
        self.shared = Arc::new(Shared { pool, marker });
    }

    /// The next entry: a frame, held, or an entry that names no buffer (see [`NextBatch`]).
    // Not an iterator: a frame's metadata borrows the reader.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> NextBatch<'_, K> {
        let pool = &self.shared.pool;
        if let Some(slot) = self.shared.marker.slot()
            && page::is_cut_off(pool.page(), pool.len(), slot)
        {
            return NextBatch::CutOff;
        }
        if let Some(pending) = self.pending.take() {
            return self.entry(pending);
        }
        let mut gone = 0_u64;
        for _ in 0..PROBES {
            match self.step() {
                Step::Gone => gone += 1,
                Step::Other => {}
                Step::Frame {
                    seq,
                    time,
                    meta,
                    held,
                } => {
                    let pending = Pending::Frame {
                        seq,
                        time,
                        meta,
                        held,
                    };
                    if gone > 0 {
                        // Say what was lost first; this entry is returned by the next call.
                        self.pending = Some(pending);
                        break;
                    }
                    return self.entry(pending);
                }
                Step::NoBuffers { seq, time, meta } => {
                    let pending = Pending::NoBuffers { seq, time, meta };
                    if gone > 0 {
                        self.pending = Some(pending);
                        break;
                    }
                    return self.entry(pending);
                }
                Step::Empty => break,
                Step::Skipped(count) => {
                    gone += count;
                    break;
                }
                Step::PoolChanged(generation) => {
                    self.missed = self.missed.saturating_add(gone + 1);
                    return NextBatch::PoolChanged { generation };
                }
                Step::Limited => {
                    self.missed = self.missed.saturating_add(gone + 1);
                    return NextBatch::Limited;
                }
            }
        }
        if gone == 0 {
            return NextBatch::Empty;
        }
        self.missed = self.missed.saturating_add(gone);
        NextBatch::Skipped(gone)
    }

    /// The arm for one entry. A frame's arm always holds at least one buffer.
    fn entry(&mut self, pending: Pending<K>) -> NextBatch<'_, K> {
        match pending {
            Pending::Frame {
                seq,
                time,
                meta,
                held,
            } => NextBatch::Frame(Batch {
                seq,
                time,
                meta: &self.meta[..meta],
                held,
            }),
            Pending::NoBuffers { seq, time, meta } => NextBatch::NoBuffers(NoBuffers {
                seq,
                time,
                meta: &self.meta[..meta],
            }),
        }
    }

    fn step(&mut self) -> Step<K> {
        let sample = match self.reader.next() {
            Next::Sample(sample) => sample,
            Next::Empty => return Step::Empty,
            Next::Skipped(count) => return Step::Skipped(count),
        };
        if sample.field != self.field {
            return Step::Other;
        }
        let entry = match entry::read(sample.payload) {
            None => return Step::Gone,
            Some(entry::Read::NoBuffers { generation, meta }) => {
                // Only the pool's generation check applies to an entry that names no buffer.
                let pool = &self.shared.pool;
                if generation != pool.generation() {
                    return if generation > pool.generation() {
                        Step::PoolChanged(generation)
                    } else {
                        Step::Gone
                    };
                }
                self.meta[..meta.len()].copy_from_slice(meta);
                return Step::NoBuffers {
                    seq: sample.seq,
                    time: sample.time,
                    meta: meta.len(),
                };
            }
            Some(entry::Read::Buffers(entry)) => entry,
        };
        let pool = &self.shared.pool;
        if entry.generation != pool.generation() {
            // Generations only go up. An older one is a frame of the pool before this one.
            return if entry.generation > pool.generation() {
                Step::PoolChanged(entry.generation)
            } else {
                Step::Gone
            };
        }
        let mut held = Held {
            shared: self.shared.clone(),
            buffers: [BufferIndex(0); MAX_PER_FRAME],
            count: 0,
        };
        for (buffer, ticket) in &entry.buffers[..entry.count] {
            if buffer.0 >= pool.len() {
                return Step::Gone;
            }
            // Mark first, then look at the ticket: see `page::still`. Dropping `held` on any
            // way out takes the marks made so far back off.
            if !self.shared.marker.mark(*buffer) {
                return Step::Limited;
            }
            held.buffers[held.count] = *buffer;
            held.count += 1;
            if !page::still(pool.page(), *buffer, *ticket) {
                return Step::Gone;
            }
        }
        // Read and checked: every buffer of the frame is now taken, as far as the publisher can
        // tell a replacement.
        for (buffer, _) in &entry.buffers[..entry.count] {
            self.shared.marker.taken(*buffer, sample.seq);
        }
        self.meta[..entry.meta.len()].copy_from_slice(entry.meta);
        Step::Frame {
            seq: sample.seq,
            time: sample.time,
            meta: entry.meta.len(),
            held,
        }
    }
}
