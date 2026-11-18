//! A module stream's shared ring, read and written the way `kernel/rs/stream_ring.rs` lays it out
//! (`shm/README.md`, layout 2): the frame ring and the pool page are read with the shm crates, and
//! the job block is written word by word with its head and tail.
//!
//! Layout, in words from the ring's start, for `F` frame slots, `B` buffers and `J` job slots of
//! `W` words each (`W` is `job_bytes / 8 + 1`, the length word first):
//!
//! - the shm ring: its header, the lane head, `F` index words at 17, then `F` slots of 10 words;
//! - the pool page at `P`: magic, layout, shape, `B` tickets at `P + 8`, then 64 cut-off words;
//! - the job block at `C = P + 8 + B + 64`: the jobs posted at `C + 2` (the consumer's), the jobs
//!   taken at `C + 3` (the module's), then `J` slots of `W` words at `C + 4`.
//!
//! Everything the module publishes is untrusted by this side too: each word is read through the
//! shm crates, which check the header against the shape they derive, and an entry that is not a
//! buffer entry is an error, not a frame.

use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use shm_ring::{Mem, Next, Reader};
use shm_ring_buffers::{BufferIndex, Marker, entry, page};
pub use shm_ring_holds::Marks;

use crate::mmap::Mapping;
use crate::stream::Registration;

/// Words before the pool page's tickets.
const PAGE_HEADER: usize = 8;
/// Cut-off words after the tickets (`shm-ring-buffers`' `MAX_READERS`).
const PAGE_READERS: usize = 64;
/// The first index word of the shm ring.
const INDEX_AT: usize = 17;
/// Words per frame slot: a 4-word header and the 48-byte entry.
const SLOT_WORDS: usize = 10;
/// The pool page's magic word and its shape word (`page_at + 2`: 1 | buffers << 32).
const POOL_MAGIC: u64 = 0x314c_4f4f_504d_4853;
/// The shm ring's magic word ("SHMRING1"), which the tests write.
#[cfg(test)]
const SHM_MAGIC: u64 = 0x3147_4e49_524d_4853;
const POOL_SHAPE: usize = 2;
/// The shm ring's shape word: 1 | frame_slots << 32, and its layout word (2).
const RING_LAYOUT: usize = 1;
const RING_LAYOUT_VALUE: u64 = 2;
const RING_SHAPE: usize = 2;
const RING_SIZES: usize = 3;
const RING_SIZES_VALUE: u64 = 48;
/// The job block's head and tail, from its control word.
const JOB_HEAD: usize = 2;
const JOB_TAIL: usize = 3;
const JOB_SLOTS: usize = 4;
/// The metadata the module writes after an entry's one buffer: sequence, identity, bytes and
/// fence, and the end time.
const META_BYTES: usize = 32;

/// The sizes a stream was made with, which the ring must match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Frame slots: a power of two, at least 2.
    pub frame_slots: u32,
    /// Buffers in the pool.
    pub buffers: u32,
    /// Job slots: a power of two.
    pub job_slots: u32,
    /// Bytes per job: the job structure's size, a multiple of 8.
    pub job_bytes: u32,
}

/// One frame the module published: the buffer it filled and the job's identity and result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The pool buffer.
    pub buffer: u32,
    /// Counts frames from the start of the stream.
    pub sequence: u64,
    /// The job's identity.
    pub identity: u64,
    /// The bytes the job wrote; 0 when it was stopped.
    pub bytes: u32,
    /// The fence the job carried.
    pub fence: u32,
    /// `CLOCK_MONOTONIC` nanoseconds when the frame was published.
    pub timestamp_ns: u64,
    /// `CLOCK_MONOTONIC` nanoseconds when the job ended, 0 when the block stamps none.
    pub end_ns: u64,
}

/// The mapping as memory: the words from `base`, read atomically.
#[derive(Clone)]
struct Words {
    map: Arc<Mapping>,
    base: usize,
}

impl Mem for Words {
    fn words(&self) -> usize {
        self.map
            .load_words()
            .map_or(0, |w| w.len().saturating_sub(self.base))
    }

    fn load(&self, index: usize, order: Ordering) -> u64 {
        self.map
            .load_words()
            .and_then(|w| w.load(self.base + index, order))
            .unwrap_or(0)
    }
}

/// A stream's ring, mapped read-write: frames are read from it, jobs posted to it.
pub struct Ring {
    map: Arc<Mapping>,
    reader: Reader<Words>,
    job_slots: usize,
    job_words: usize,
    /// Word of the first job slot.
    jobs_at: usize,
    /// Word of the job block's control word (the head is at `control + JOB_HEAD`).
    control: usize,
    jobs_head: u64,
    frames_tail: u64,
    page_at: usize,
    dropped: u64,
    /// The reader's marks page, once given to the module (`add_marks`).
    holds: Option<Holds>,
}

/// A consumer's holds on the pool's buffers: its marks page, the module's registration of it, how
/// many frames it holds in each buffer, and how many it may have in hand at once.
struct Holds {
    marks: Marks,
    _registration: Registration,
    counts: Vec<u32>,
    /// Of `counts`, the frames passed on ([`Ring::pass_on`]): still held, no longer in hand.
    passed: Vec<u32>,
    held: u32,
    /// Of `held`, the frames passed on.
    passed_on: u32,
    /// A relay ([`Ring::hold_passing`]): a reader of a source that runs on whether or not it is
    /// read, so frames it was too late for are frames dropped, not a broken stream.
    relay: bool,
    /// The frames this consumer may have in hand: held and not passed on.
    max_held: u32,
}

impl Holds {
    /// Whether taking another frame would put more in hand than the limit allows.
    fn at_limit(&self) -> bool {
        self.held - self.passed_on >= self.max_held
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

impl std::fmt::Debug for Ring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ring")
            .field("jobs_posted", &self.jobs_head)
            .field("frames_taken", &self.frames_tail)
            .finish_non_exhaustive()
    }
}

impl Ring {
    /// Maps the ring of a stream made with `ring_bytes` bytes and checks it against `layout`.
    /// Fails with `InvalidData` when the module's words don't say what the layout says.
    pub fn map(fd: BorrowedFd<'_>, ring_bytes: usize, layout: &Layout) -> io::Result<Self> {
        if layout.frame_slots < 2 || !layout.frame_slots.is_power_of_two() {
            return Err(invalid("frame slots must be a power of two, at least 2"));
        }
        if layout.job_slots == 0 || !layout.job_slots.is_power_of_two() {
            return Err(invalid("job slots must be a power of two"));
        }
        if layout.job_bytes == 0 || !layout.job_bytes.is_multiple_of(8) {
            return Err(invalid("job bytes must be a multiple of 8"));
        }
        let frame_slots = layout.frame_slots as usize;
        let buffers = layout.buffers as usize;
        let job_words = 1 + layout.job_bytes as usize / 8;
        let index_end = INDEX_AT + frame_slots;
        let page_at = index_end + frame_slots * SLOT_WORDS;
        let control = page_at + PAGE_HEADER + buffers + PAGE_READERS;
        let jobs_at = control + JOB_SLOTS;
        let end = jobs_at + layout.job_slots as usize * job_words;
        if ring_bytes < end * 8 {
            return Err(invalid("the ring is smaller than its layout"));
        }
        let map = Arc::new(Mapping::new(fd, ring_bytes, 0, true)?);
        let reader = Reader::over_from_start(Words {
            map: Arc::clone(&map),
            base: 0,
        })
        .map_err(|e| invalid(&format!("the frame ring: {e:?}")))?;
        let words = map
            .as_atomic_words()
            .ok_or_else(|| invalid("the ring is not mapped"))?;
        let word = |at: usize| words.get(at).map(|w| w.load(Ordering::Acquire));
        if word(RING_LAYOUT) != Some(RING_LAYOUT_VALUE)
            || word(RING_SHAPE) != Some(1 | (layout.frame_slots as u64) << 32)
            || word(RING_SIZES) != Some(RING_SIZES_VALUE)
            || word(page_at) != Some(POOL_MAGIC)
            || word(page_at + POOL_SHAPE) != Some(1 | (layout.buffers as u64) << 32)
        {
            return Err(invalid(
                "the ring's header is not the layout the stream was made with",
            ));
        }
        Ok(Self {
            map,
            reader,
            job_slots: layout.job_slots as usize,
            job_words,
            jobs_at,
            control,
            jobs_head: 0,
            frames_tail: 0,
            page_at,
            dropped: 0,
            holds: None,
        })
    }

    /// Gives the module this consumer's marks page, so buffers it holds are not reused, and
    /// starts holding: from now on every frame taken is marked, and [`release`](Self::release)
    /// lets its buffer go. `max_held` frames at most may be held at once; past that
    /// [`next_frame`](Self::next_frame) takes none, and the frames that go by are dropped and
    /// counted. `stream` is the stream's descriptor.
    pub fn add_marks(
        &mut self,
        stream: BorrowedFd<'_>,
        marks: Marks,
        buffers: u32,
        max_held: u32,
    ) -> io::Result<()> {
        if self.holds.is_some() || max_held == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "marks already given, or no hold",
            ));
        }
        let page = marks.page_fd().map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("the marks page: {e:?}"))
        })?;
        let registration = crate::stream::add_reader(stream, page.as_fd(), max_held, 0)
            .map_err(|e| io::Error::other(format!("give the module the marks page: {e}")))?;
        marks.set_slot(registration.slot);
        self.attach_holds(marks, registration, buffers, max_held, false)
    }

    /// Starts holding for a consumer that passes its frames on ([`pass_on`](Self::pass_on)): a
    /// relay, whose frame goes to a reader of its own and is held here until that reader is done
    /// with it. `max_in_hand` bounds only the frames taken and not yet passed on. The module is
    /// told this consumer may hold every buffer, because how many frames stay held after they are
    /// passed on is up to that reader, and holding all of them stops the block without a fault.
    ///
    /// A relay reads a source that does not wait for it. When it falls more than the ring's frame
    /// slots behind (it was not scheduled for that many frame periods), the module has written over
    /// the frames it had not taken. It held none of them, so nothing it holds is touched: they are
    /// dropped and counted in [`dropped`](Self::dropped), exactly as frames that go by at the limit
    /// are, and [`next_frame`](Self::next_frame) goes on with the frames still on the ring. Each
    /// frame carries the module's own sequence, so the drop shows as a gap in it. A reader without
    /// this (a job's results) is out of step when it is overrun, and that stays an error.
    pub fn hold_passing(
        &mut self,
        stream: BorrowedFd<'_>,
        buffers: u32,
        max_in_hand: u32,
    ) -> io::Result<()> {
        if self.holds.is_some() || max_in_hand == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "marks already given, or no hold",
            ));
        }
        let marks = Marks::create(buffers, 0, buffers).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("make the marks page: {e:?}"),
            )
        })?;
        let page = marks.page_fd().map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("the marks page: {e:?}"))
        })?;
        let registration = crate::stream::add_reader(stream, page.as_fd(), buffers, 0)
            .map_err(|e| io::Error::other(format!("give the module the marks page: {e}")))?;
        marks.set_slot(registration.slot);
        self.attach_holds(marks, registration, buffers, max_in_hand, true)
    }

    /// Starts holding with `marks` already given to the module as `registration`.
    fn attach_holds(
        &mut self,
        marks: Marks,
        registration: Registration,
        buffers: u32,
        max_held: u32,
        relay: bool,
    ) -> io::Result<()> {
        if self.holds.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "marks already given",
            ));
        }
        self.holds = Some(Holds {
            marks,
            _registration: registration,
            counts: vec![0; buffers as usize],
            passed: vec![0; buffers as usize],
            held: 0,
            passed_on: 0,
            relay,
            max_held,
        });
        Ok(())
    }

    /// The frame held in `buffer` went on to this consumer's own reader. Its buffer stays held, so
    /// the module does not reuse it, until [`release`](Self::release); it no longer counts against
    /// the frames this consumer may have in hand, so the reader keeping it does not stop the next
    /// frame from being taken. `InvalidInput` when no frame held in `buffer` is still in hand.
    pub fn pass_on(&mut self, buffer: u32) -> io::Result<()> {
        let holds = self.holds.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "no holds on this stream")
        })?;
        let in_hand = holds
            .counts
            .get(buffer as usize)
            .zip(holds.passed.get(buffer as usize))
            .is_some_and(|(count, passed)| count > passed);
        if !in_hand {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("buffer {buffer} holds no frame in hand"),
            ));
        }
        holds.passed[buffer as usize] += 1;
        holds.passed_on += 1;
        Ok(())
    }

    /// [`add_marks`](Self::add_marks) with a marks page made here, of `buffers` buffers.
    pub fn hold_frames(
        &mut self,
        stream: BorrowedFd<'_>,
        buffers: u32,
        max_held: u32,
    ) -> io::Result<()> {
        let marks = Marks::create(buffers, 0, max_held).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("make the marks page: {e:?}"),
            )
        })?;
        self.add_marks(stream, marks, buffers, max_held)
    }

    /// Lets go of one frame's buffer: the marks come off, so the module may reuse it. `InvalidInput`
    /// when this consumer holds no frame in `buffer` (a double release, or one never taken).
    pub fn release(&mut self, buffer: u32) -> io::Result<()> {
        let holds = self.holds.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "no holds on this stream")
        })?;
        let count = holds.counts.get_mut(buffer as usize).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "a buffer outside the pool")
        })?;
        if *count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("buffer {buffer} is not held"),
            ));
        }
        *count -= 1;
        holds.held -= 1;
        // A frame passed on is the one that comes back first.
        if let Some(passed) = holds.passed.get_mut(buffer as usize)
            && *passed > 0
        {
            *passed -= 1;
            holds.passed_on -= 1;
        }
        holds.marks.unmark(BufferIndex(buffer));
        Ok(())
    }

    fn word(&self, at: usize) -> io::Result<&AtomicU64> {
        self.map
            .as_atomic_words()
            .and_then(|w| w.get(at))
            .ok_or_else(|| invalid("a word outside the ring"))
    }

    /// The job slot the next [`post_job`](Self::post_job) fills, or `None` while the block is full.
    pub fn next_job_slot(&self) -> io::Result<Option<u32>> {
        let tail = self.word(self.control + JOB_TAIL)?.load(Ordering::Acquire);
        if self.jobs_head.wrapping_sub(tail) >= self.job_slots as u64 {
            return Ok(None);
        }
        Ok(Some(
            (self.jobs_head as usize & (self.job_slots - 1)) as u32,
        ))
    }

    /// Posts one job: its words, `job_bytes / 8` of them. `WouldBlock` when the job block is full.
    pub fn post_job(&mut self, job: &[u64]) -> io::Result<()> {
        if job.len() + 1 != self.job_words {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a job of the wrong size",
            ));
        }
        let tail = self.word(self.control + JOB_TAIL)?.load(Ordering::Acquire);
        if self.jobs_head.wrapping_sub(tail) >= self.job_slots as u64 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let slot = self.jobs_head as usize & (self.job_slots - 1);
        let at = self.jobs_at + slot * self.job_words;
        self.word(at)?
            .store((job.len() * 8) as u64, Ordering::Relaxed);
        for (i, w) in job.iter().enumerate() {
            self.word(at + 1 + i)?.store(*w, Ordering::Relaxed);
        }
        self.jobs_head += 1;
        self.word(self.control + JOB_HEAD)?
            .store(self.jobs_head, Ordering::Release);
        Ok(())
    }

    /// The next frame published, or `None` when there is none yet. `InvalidData` for an entry
    /// that is not a buffer entry, or when a reader that is not a relay fell behind and skipped
    /// frames. A relay ([`hold_passing`](Self::hold_passing)) that fell behind drops the frames it
    /// missed, counts them, and takes the next one still on the ring.
    pub fn next_frame(&mut self) -> io::Result<Option<Frame>> {
        if self.holds.as_ref().is_some_and(Holds::at_limit) {
            // Taking another would exceed the limit. The frames go by unheld and are counted as
            // dropped: left on the ring they would be lapped, and a lapped reader fails.
            self.drop_frames();
            return Ok(None);
        }
        self.take_one()
    }

    /// Discards every frame on the ring without holding any, counting each in [`dropped`](Self::dropped).
    fn drop_frames(&mut self) {
        loop {
            match self.reader.next() {
                Next::Sample(_) => self.dropped += 1,
                Next::Skipped(n) => self.dropped += n,
                Next::Empty => break,
            }
        }
    }

    /// Frames that went by untaken, since the ring was mapped: discarded at the hold limit, and, for
    /// a relay, written over before it read them or reused before it could hold them.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// One sample off the ring. With holds, its buffer is marked first and then checked, so a
    /// buffer the module reused before the mark is skipped, not read.
    fn take_one(&mut self) -> io::Result<Option<Frame>> {
        let relay = self.holds.as_ref().is_some_and(|holds| holds.relay);
        loop {
            let sample = match self.reader.next() {
                Next::Sample(sample) => sample,
                Next::Empty => return Ok(None),
                // A relay that was overrun lost frames it never held: dropped, and on to the next.
                Next::Skipped(n) if relay => {
                    self.dropped = self.dropped.saturating_add(n);
                    continue;
                }
                Next::Skipped(n) => {
                    return Err(invalid(&format!(
                        "the reader was overrun: {n} frames skipped"
                    )));
                }
            };
            let timestamp_ns = sample.time.as_nanos();
            let Some(entry::Read::Buffers(e)) = entry::read(sample.payload) else {
                return Err(invalid("a frame entry that names no buffer"));
            };
            let (buffer, ticket) = e.buffers[0];
            if let Some(holds) = &mut self.holds {
                let index = buffer.0;
                let count = holds
                    .counts
                    .get_mut(index as usize)
                    .ok_or_else(|| invalid("a frame naming a buffer outside the pool"))?;
                if !holds.marks.mark(buffer) {
                    return Err(invalid("the marks page refused a hold below its limit"));
                }
                *count += 1;
                holds.held += 1;
                let page = Words {
                    map: Arc::clone(&self.map),
                    base: self.page_at,
                };
                if !page::still(&page, buffer, ticket) {
                    // Reused before the mark: the frame's bytes are gone. Let the mark go and skip it.
                    holds.counts[index as usize] -= 1;
                    holds.held -= 1;
                    holds.marks.unmark(buffer);
                    if holds.relay {
                        // The relay was late for this one too: dropped, and on to the next, so the
                        // frames behind it do not wait for another wake.
                        self.dropped = self.dropped.saturating_add(1);
                        continue;
                    }
                    return Ok(None);
                }
            }
            let meta = e
                .meta
                .get(..META_BYTES)
                .ok_or_else(|| invalid("a frame entry with no metadata"))?;
            let u64_at =
                |at: usize| u64::from_le_bytes(meta[at..at + 8].try_into().unwrap_or([0; 8]));
            let u32_at =
                |at: usize| u32::from_le_bytes(meta[at..at + 4].try_into().unwrap_or([0; 4]));
            self.frames_tail += 1;
            return Ok(Some(Frame {
                buffer: buffer.0,
                sequence: u64_at(0),
                identity: u64_at(8),
                bytes: u32_at(16),
                fence: u32_at(20),
                timestamp_ns,
                end_ns: u64_at(24),
            }));
        }
    }

    /// Frames taken so far by [`next_frame`](Self::next_frame).
    #[must_use]
    pub fn frames_taken(&self) -> u64 {
        self.frames_tail
    }

    /// Jobs posted so far.
    #[must_use]
    pub fn jobs_posted(&self) -> u64 {
        self.jobs_head
    }
}

#[cfg(test)]
impl Ring {
    /// Takes a frame's hold on `buffer` as [`next_frame`](Self::next_frame) does, for a test that
    /// has no frame to read.
    fn hold_for_test(&mut self, buffer: u32) -> io::Result<()> {
        let holds = self
            .holds
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no holds"))?;
        let count = holds
            .counts
            .get_mut(buffer as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "outside the pool"))?;
        if !holds.marks.mark(BufferIndex(buffer)) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "at the hold limit",
            ));
        }
        *count += 1;
        holds.held += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::FileExt;

    /// A ring of `frame_slots` frames and `buffers` buffers in an in-memory region, with the words
    /// the module writes at make time (`kernel/rs/stream_ring.rs`, `init_ring`): no frame.
    fn module_ring(frame_slots: u32, buffers: u32) -> (OwnedFd, usize, Layout) {
        let layout = Layout {
            frame_slots,
            buffers,
            job_slots: 2,
            job_bytes: 8,
        };
        let fd = crate::dma_buf::memfd(c"lemnos-ring-test").expect("memfd");
        let bytes = 4096;
        let file = File::from(fd);
        file.set_len(bytes as u64).expect("size the region");
        let page_at = INDEX_AT + frame_slots as usize + frame_slots as usize * SLOT_WORDS;
        let put = |word: usize, value: u64| {
            file.write_all_at(&value.to_le_bytes(), (word * 8) as u64)
                .expect("write a word");
        };
        put(0, SHM_MAGIC);
        put(RING_LAYOUT, RING_LAYOUT_VALUE);
        put(RING_SHAPE, 1 | u64::from(frame_slots) << 32);
        put(RING_SIZES, RING_SIZES_VALUE);
        put(4, page_at as u64);
        put(page_at, POOL_MAGIC);
        put(page_at + POOL_SHAPE, 1 | u64::from(buffers) << 32);
        (OwnedFd::from(file), bytes, layout)
    }

    /// The ring's release refuses what the shm ring refuses: a second release of one hold, and a
    /// release of a buffer no frame was taken from. Runs over the in-memory region, no stream.
    #[test]
    fn release_refuses_a_double_release_and_a_buffer_never_held() {
        let (fd, bytes, layout) = module_ring(2, 2);
        let mut ring = Ring::map(fd.as_fd(), bytes, &layout).expect("the ring maps");
        let marks = Marks::create(2, 0, 2).expect("marks");
        let dummy = Registration {
            slot: 0,
            file: crate::dma_buf::memfd(c"lemnos-registration-test").expect("memfd"),
        };
        ring.attach_holds(marks, dummy, 2, 2, false)
            .expect("holds attach");
        ring.hold_for_test(0).expect("a frame's buffer is held");
        ring.release(0).expect("the hold is let go once");
        assert_eq!(
            ring.release(0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "a second release of the same hold is refused"
        );
        assert_eq!(
            ring.release(1).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "a release of a buffer no frame was taken from is refused"
        );
    }

    /// Publishes one frame in `buffer` on the in-memory ring, as the module does: a buffer entry
    /// with the buffer's ticket and the module's 32 bytes of metadata.
    fn publish(ring: &Ring, buffer: u32, sequence: u64) {
        /// The test's mapping of the region, written as the module writes its own.
        struct Module(Words);
        impl Mem for Module {
            fn words(&self) -> usize {
                self.0.words()
            }
            fn load(&self, index: usize, order: Ordering) -> u64 {
                self.0.load(index, order)
            }
        }
        impl shm_ring::MemMut for Module {
            fn store(&self, index: usize, value: u64, order: Ordering) {
                if let Some(word) = self.0.map.as_atomic_words().and_then(|w| w.get(index)) {
                    word.store(value, order);
                }
            }
        }
        let module = Module(Words {
            map: Arc::clone(&ring.map),
            base: 0,
        });
        let mut writer = shm_ring::Writer::over(module).expect("the frame ring");
        let mut meta = [0u8; META_BYTES];
        meta[..8].copy_from_slice(&sequence.to_le_bytes());
        let mut payload = [0u8; 48];
        let len =
            entry::write(&mut payload, 1, &[(BufferIndex(buffer), 0)], &meta).expect("an entry");
        writer
            .publish(
                shm_ring::FieldId(0),
                shm_ring::SampleTime::from_nanos(sequence),
                shm_ring::Flags::NONE,
                &payload[..len],
            )
            .expect("published");
    }

    /// A reader allowed one frame in hand, on a pool of four, with its marks as `hold_passing`
    /// makes them: the page allows every buffer, the limit is on what is in hand.
    fn passing_ring() -> (OwnedFd, Ring) {
        let (fd, bytes, layout) = module_ring(8, 4);
        let mut ring = Ring::map(fd.as_fd(), bytes, &layout).expect("the ring maps");
        let marks = Marks::create(4, 0, 4).expect("marks");
        let dummy = Registration {
            slot: 0,
            file: crate::dma_buf::memfd(c"lemnos-registration-test").expect("memfd"),
        };
        ring.attach_holds(marks, dummy, 4, 1, true)
            .expect("holds attach");
        (fd, ring)
    }

    /// The driver was not scheduled for eleven frame periods on a ring of eight slots: the module
    /// wrote over the three oldest frames before the relay read them. It held none of them, so they
    /// are dropped and counted, and the eight still on the ring are taken in order, each with the
    /// module's own sequence (the gap shows the drop). Before, this was `InvalidData`, which the
    /// receiver driver took as the end of the module's stream: a recovery restart, 250 ms of
    /// backoff and every frame fired in it lost, for a relay that was late by a few frames.
    #[test]
    fn a_relay_that_was_overrun_drops_what_it_missed_and_reads_on() {
        let (_fd, mut ring) = passing_ring();
        for sequence in 0..11 {
            publish(&ring, (sequence % 4) as u32, sequence);
        }
        for sequence in 3..11 {
            let frame = ring
                .next_frame()
                .expect("an overrun relay reads on")
                .unwrap_or_else(|| panic!("frame {sequence} is still on the ring"));
            assert_eq!(frame.sequence, sequence);
            ring.pass_on(frame.buffer).expect("the frame goes on");
            ring.release(frame.buffer).expect("and comes back");
        }
        assert_eq!(ring.dropped(), 3, "the frames written over are counted");
        assert!(ring.next_frame().expect("reads").is_none());
    }

    /// Overrun by one frame, the smallest case (the one the QEMU trace caught): one frame dropped,
    /// the rest read, and a frame held from before the overrun is still held after it.
    #[test]
    fn an_overrun_by_one_frame_costs_one_frame_and_no_hold() {
        let (_fd, mut ring) = passing_ring();
        publish(&ring, 0, 0);
        let kept = ring.next_frame().expect("reads").expect("frame 0");
        ring.pass_on(kept.buffer).expect("the frame goes on");
        for sequence in 1..10 {
            publish(&ring, 1 + (sequence % 3) as u32, sequence);
        }
        let next = ring.next_frame().expect("reads on").expect("frame 2");
        assert_eq!(next.sequence, 2);
        assert_eq!(ring.dropped(), 1);
        let holds = ring.holds.as_ref().expect("holds");
        assert_eq!(
            (holds.counts[0], holds.held),
            (1, 2),
            "frame 0 is still held"
        );
    }

    /// A reader that is not a relay (a job's results) is out of step when it is overrun: that is
    /// still an error, as it was.
    #[test]
    fn a_reader_that_is_not_a_relay_still_fails_when_overrun() {
        let (fd, bytes, layout) = module_ring(8, 4);
        let mut ring = Ring::map(fd.as_fd(), bytes, &layout).expect("the ring maps");
        for sequence in 0..11 {
            publish(&ring, (sequence % 4) as u32, sequence);
        }
        assert_eq!(
            ring.next_frame().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    /// A relay's reader hands each frame on to its own consumer and keeps the buffer held until
    /// that consumer is done. Frames handed on do not count against the limit: with one frame in
    /// hand allowed and two handed on and still held, the third frame is taken, not dropped.
    /// Before `pass_on`, the first held frame was the limit and every later frame was dropped for
    /// as long as the consumer kept it.
    #[test]
    fn a_frame_passed_on_stays_held_and_does_not_count_against_the_limit() {
        let (_fd, mut ring) = passing_ring();
        for (buffer, sequence) in [(0, 10), (1, 11), (2, 12)] {
            publish(&ring, buffer, sequence);
            let frame = ring
                .next_frame()
                .expect("the ring reads")
                .unwrap_or_else(|| panic!("frame {sequence} is taken, not dropped at the limit"));
            assert_eq!((frame.buffer, frame.sequence), (buffer, sequence));
            ring.pass_on(buffer).expect("the frame goes on");
        }
        assert_eq!(ring.dropped(), 0);
        // All three are still held: the module may not reuse them.
        let holds = ring.holds.as_ref().expect("holds");
        assert_eq!((holds.held, holds.passed_on), (3, 3));
        // Each comes back once, and only once.
        for buffer in 0..3 {
            ring.release(buffer).expect("its consumer is done with it");
        }
        assert_eq!(
            ring.release(0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let holds = ring.holds.as_ref().expect("holds");
        assert_eq!((holds.held, holds.passed_on), (0, 0));
    }

    /// A frame in hand, not passed on, is still the limit: the next frames go by and are counted,
    /// and once it is passed on (or released) the reader takes frames again.
    #[test]
    fn a_frame_in_hand_is_the_limit_until_it_is_passed_on() {
        let (_fd, mut ring) = passing_ring();
        publish(&ring, 0, 10);
        assert!(ring.next_frame().expect("reads").is_some());
        publish(&ring, 1, 11);
        publish(&ring, 2, 12);
        assert!(
            ring.next_frame().expect("reads").is_none(),
            "one in hand is the limit"
        );
        assert_eq!(ring.dropped(), 2, "the frames that went by are counted");
        ring.pass_on(0).expect("the frame goes on");
        assert_eq!(
            ring.pass_on(0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "it can go on once"
        );
        assert_eq!(
            ring.pass_on(3).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "a buffer with no frame in hand"
        );
        publish(&ring, 3, 13);
        let frame = ring.next_frame().expect("reads").expect("taken again");
        assert_eq!((frame.buffer, frame.sequence), (3, 13));
    }

    /// A ring the module did not lay out, or laid out for other sizes, is refused at map time.
    #[test]
    fn a_region_not_laid_out_like_the_module_is_refused() {
        let (fd, bytes, layout) = module_ring(2, 2);
        let wrong = Layout {
            buffers: 3,
            ..layout
        };
        assert_eq!(
            Ring::map(fd.as_fd(), bytes, &wrong).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
