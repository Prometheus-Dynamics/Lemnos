//! A module stream's shared ring, read and written the way `kernel/lemnos_stream.c` lays it out
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
/// many frames it holds in each buffer, and how many it may hold at once.
struct Holds {
    marks: Marks,
    _registration: Registration,
    counts: Vec<u32>,
    held: u32,
    max_held: u32,
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
    /// [`next_frame`](Self::next_frame) waits, and the frame stays on the ring. `stream` is the
    /// stream's descriptor.
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
        self.attach_holds(marks, registration, buffers, max_held)
    }

    /// Starts holding with `marks` already given to the module as `registration`.
    fn attach_holds(
        &mut self,
        marks: Marks,
        registration: Registration,
        buffers: u32,
        max_held: u32,
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
            held: 0,
            max_held,
        });
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
    /// that is not a buffer entry, or when the reader fell behind and skipped frames.
    pub fn next_frame(&mut self) -> io::Result<Option<Frame>> {
        if self.holds.as_ref().is_some_and(|h| h.held >= h.max_held) {
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

    /// Frames discarded at the hold limit, since the ring was mapped.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// One sample off the ring. With holds, its buffer is marked first and then checked, so a
    /// buffer the module reused before the mark is skipped, not read.
    fn take_one(&mut self) -> io::Result<Option<Frame>> {
        let sample = match self.reader.next() {
            Next::Sample(sample) => sample,
            Next::Empty => return Ok(None),
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
                return Ok(None);
            }
        }
        let meta = e
            .meta
            .get(..META_BYTES)
            .ok_or_else(|| invalid("a frame entry with no metadata"))?;
        let u64_at = |at: usize| u64::from_le_bytes(meta[at..at + 8].try_into().unwrap_or([0; 8]));
        let u32_at = |at: usize| u32::from_le_bytes(meta[at..at + 4].try_into().unwrap_or([0; 4]));
        self.frames_tail += 1;
        Ok(Some(Frame {
            buffer: buffer.0,
            sequence: u64_at(0),
            identity: u64_at(8),
            bytes: u32_at(16),
            fence: u32_at(20),
            timestamp_ns,
            end_ns: u64_at(24),
        }))
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
    /// the module writes at make time (`kernel/lemnos_stream.c` `lem_stream_init_ring`): no frame.
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
        ring.attach_holds(marks, dummy, 2, 2).expect("holds attach");
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
