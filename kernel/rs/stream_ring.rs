// SPDX-License-Identifier: GPL-2.0
//! The frame ring and the hold table (the stream port, slice 1): the words the module writes into the
//! memory its consumer maps, and what it reads of the consumers' marks pages.
//!
//! The frame ring is shm/README.md's ring (layout 2), one field, one sample per frame, with the pool
//! page of shm/ring-buffers/src/page.rs after it and the job block after that. Every number here is
//! what the Rust crates say (testing/crates/shm-ring-check reads what this writes, and its own tests
//! pin the derived shape): the word numbers, the slot size, the entry, the stamps, and the pool page's
//! ticket words.
//!
//! Everything the consumer can touch is hostile: the ring is mapped into its process, so any word can
//! change at any moment. Each value is read once into a local, checked, and only the local is used.
//! A buffer's ownership is only ever in kernel memory (`Buf`), so the consumer can't claim a state it
//! doesn't have.
//!
//! Nothing here allocates or sleeps: every function runs under the stream's lock, on the frame path.

use crate::bindings as lb;
use crate::stream::{Job, Producer};
use crate::stream_region::Region;
use core::{ffi::c_int, ptr};
use kernel::{
    alloc::{KVVec, KVec},
    bindings,
};

/// What creation fixes: the ring and every size. Never written after; read without a lock.
pub(crate) struct Geom {
    /// vmalloc_user: mapped by the consumer.
    pub(crate) ring: *mut u64,
    pub(crate) ring_bytes: usize,
    // The module's copies. The ring's header is for the consumer only.
    pub(crate) buffers: u32,
    pub(crate) buffer_bytes: u32,
    pub(crate) frame_slots: u32,
    pub(crate) job_slots: u32,
    pub(crate) job_bytes: u32,
    pub(crate) jobs_at: usize,
    pub(crate) job_words: usize,
    pub(crate) stride: u64,
    /// Words of the shm ring.
    pub(crate) shm_total: usize,
    /// Its slots.
    pub(crate) shm_slots_at: usize,
    /// The pool page's first word.
    pub(crate) page_at: usize,
    pub(crate) w_state: usize,
    pub(crate) w_jobs_head: usize,
    pub(crate) w_jobs_tail: usize,
}

/// One buffer's state, in kernel memory only.
#[derive(Clone, Copy)]
pub(crate) struct Buf {
    /// Its current generation.
    pub(crate) fence: u32,
    /// BUF_*.
    pub(crate) state: u8,
}

/// A reader's marks page, pinned for the stream's life: the reader's own memfd, mapped read-only
/// here. Its words are shm/ring-holds' layout 2 (marks.rs): the layout and version in word 0, one
/// mark per buffer, nonzero while the reader holds it, then one taken stamp per buffer.
pub(crate) struct Reader {
    /// The pinned pages; rs/stream_reader.rs pins and unpins them.
    pub(crate) pages: KVec<*mut bindings::page>,
    /// vmap of the pages, read-only; null: no reader in this slot.
    pub(crate) marks: *mut u64,
    /// What the reader may hold at once; 0 for no limit.
    pub(crate) max_held: u32,
    /// How long one hold may last; 0 for no limit.
    pub(crate) max_hold_ns: u64,
    /// Over a limit: its marks hold nothing, and the pool page says so.
    pub(crate) cut_off: bool,
    /// When its marks last could not be counted at one instant; 0 if they can.
    pub(crate) uncounted_since: u64,
    /// Per buffer: the use it was first seen marked in (+1), and when.
    pub(crate) seen: KVVec<u64>,
    /// The registration's id; 0 when the slot is free.
    pub(crate) id: u64,
}

/// The frame path's state: everything in it is under the stream's lock.
pub(crate) struct Frame {
    buf: KVVec<Buf>,
    /// `job_slots` validated jobs.
    queue: KVVec<Job>,
    pub(crate) queue_head: u64,
    pub(crate) queue_tail: u64,
    pub(crate) region: [Region; crate::stream_frame::REGIONS],
    /// Without a pool: each buffer's dma-buf; empty with one.
    own: KVVec<Region>,
    /// Where the next jobless frame looks.
    pub(crate) cursor: u32,

    // The producer's own counts, never read back from the ring.
    pub(crate) frames_head: u64,
    pub(crate) jobs_tail: u64,
    pub(crate) sequence: u64,
    pub(crate) published: u64,
    pub(crate) dropped: u64,
    pub(crate) returned: u64,
    pub(crate) jobs: u64,

    /// The errno that ended it; the ring is left alone after.
    pub(crate) fault: c_int,
    /// Over: the block went or the file closed.
    pub(crate) dead: bool,
    /// The test source: frames still to make.
    pub(crate) left: u32,

    /// The block feeding this stream, or `stop` None.
    pub(crate) producer: Producer,
    /// What the producer may write per buffer.
    pub(crate) frame_bytes: u32,
    /// A block's DMA could not be seen to stop: nothing is unpinned.
    pub(crate) wedged: bool,

    /// The consumers' marks: POOL_READERS slots, on the heap (they are most of the state's size).
    pub(crate) reader: KVec<Reader>,
    /// The buffer of the newest frame, kept for the frame path.
    pub(crate) newest: u32,
    /// That frame's sequence.
    pub(crate) newest_seq: u64,
    /// The last registration id given out.
    pub(crate) reader_ids: u64,
}

// SAFETY: the raw pointers in it are to dma-bufs and pages the stream holds references on, and to
// callbacks' private data their owner made for any context; nothing in it is tied to a thread.
unsafe impl Send for Frame {}

impl Frame {
    /// A new stream's frame state: the buffers' states, an empty job queue, and each buffer's own
    /// dma-buf slot on a stream without a pool (`own` empty with one), and the reader slots, all empty.
    pub(crate) fn new(buf: KVVec<Buf>, queue: KVVec<Job>, own: KVVec<Region>, reader: KVec<Reader>) -> Self {
        Self {
            buf,
            queue,
            queue_head: 0,
            queue_tail: 0,
            region: [Region::default(); crate::stream_frame::REGIONS],
            own,
            cursor: 0,
            frames_head: 0,
            jobs_tail: 0,
            sequence: 0,
            published: 0,
            dropped: 0,
            returned: 0,
            jobs: 0,
            fault: 0,
            dead: false,
            left: 0,
            producer: Producer::NONE,
            frame_bytes: 0,
            wedged: false,
            reader,
            newest: u32::MAX,
            newest_seq: 0,
            reader_ids: 0,
        }
    }

    /// The state of `buffer`, which is below the stream's buffers.
    pub(crate) fn buf(&mut self, buffer: u32) -> &mut Buf {
        &mut self.buf[buffer as usize]
    }

    /// Whether each buffer has a dma-buf of its own (a stream made without a pool).
    pub(crate) fn own_mode(&self) -> bool {
        !self.own.is_empty()
    }

    /// The dma-buf of `buffer` on a stream without a pool; `buffer` is below the stream's buffers.
    pub(crate) fn own_at(&mut self, buffer: u32) -> &mut Region {
        &mut self.own[buffer as usize]
    }

    /// Slot `at` of the job queue, which has `job_slots` (a power of two) entries.
    pub(crate) fn queue_slot(&mut self, at: u64, job_slots: u32) -> &mut Job {
        &mut self.queue[(at & (u64::from(job_slots) - 1)) as usize]
    }
}

pub(crate) const SHM_MAGIC: u64 = 0x3147_4e49_524d_4853; // "SHMRING1"
/// shm-ring's LAYOUT: a reader of another layout refuses the ring.
pub(crate) const SHM_LAYOUT: u64 = 2;
pub(crate) const SHM_W_MAGIC: usize = 0;
pub(crate) const SHM_W_LAYOUT: usize = 1;
pub(crate) const SHM_W_SHAPE: usize = 2;
pub(crate) const SHM_W_SIZES: usize = 3;
pub(crate) const SHM_W_TOTAL: usize = 4;
pub(crate) const SHM_W_HEAD: usize = 8;
pub(crate) const SHM_W_END: usize = 10;
/// The one field's lane head.
pub(crate) const SHM_LANE_HEAD: usize = 16;
/// `frame_slots` index words from here.
pub(crate) const SHM_INDEX: usize = 17;
/// Stamp, time, meta, lane sequence.
pub(crate) const SHM_SLOT_HEADER: usize = 4;
/// Generation, count, buffer, ticket, sequence, identity, bytes, fence, end time.
pub(crate) const SHM_ENTRY_BYTES: usize = 48;
pub(crate) const SHM_SLOT_WORDS: usize = SHM_SLOT_HEADER + SHM_ENTRY_BYTES / 8;
pub(crate) const POOL_MAGIC: u64 = 0x314c_4f4f_504d_4853; // "SHMPOOL1"
/// shm-ring-buffers' page::LAYOUT: a reader of another layout refuses the page.
pub(crate) const POOL_LAYOUT: u64 = 2;
pub(crate) const POOL_LAYOUT_WORD: usize = 1;
pub(crate) const POOL_SHAPE_WORD: usize = 2;
/// One word per buffer, from here.
pub(crate) const POOL_TICKETS: usize = 8;
/// shm-ring-buffers' MAX_READERS: cut-off words after the tickets.
pub(crate) const POOL_READERS: usize = 64;
/// The job block, from the control words (control = page_at + POOL_TICKETS + buffers + POOL_READERS):
/// the jobs posted (the consumer's), the jobs taken (the module's), then the job slots, each
/// 1 + job_bytes/8 words: the length, then the job.
pub(crate) const JOB_HEAD: usize = 2;
pub(crate) const JOB_TAIL: usize = 3;
pub(crate) const JOB_SLOTS: usize = 4;
/// A marks page's first mark: word 0 is its layout and version (marks.rs).
pub(crate) const MARKS_AT: usize = 1;
/// shm-ring-holds' LAYOUT: a marks page of another layout is refused.
pub(crate) const MARKS_LAYOUT: u64 = 2;
/// The layout is word 0's top byte; a layout 1 page has zero there.
const MARKS_LAYOUT_SHIFT: u32 = 56;
/// How long a reader's marks may stay uncountable when it has no hold time of its own (table.rs).
const UNCOUNTED_NS: u64 = 10 * 1_000_000_000;

const _: () = assert!(SHM_ENTRY_BYTES % 8 == 0 && SHM_SLOT_WORDS == 10);
const _: () = assert!(lb::LEMNOS_STREAM_MAX_READERS as usize == POOL_READERS);
const _: () = assert!(lb::LEMNOS_STREAM_JOB_HEAD as usize == JOB_HEAD && lb::LEMNOS_STREAM_JOB_TAIL as usize == JOB_TAIL);
const _: () = assert!(lb::LEMNOS_STREAM_JOB_SLOTS as usize == JOB_SLOTS);
const _: () = assert!(cfg!(target_endian = "little"));

/// The kernel has it, nobody asked for it.
pub(crate) const BUF_FREE: u8 = 0;
/// A job names it.
pub(crate) const BUF_QUEUED: u8 = 1;
/// The source is filling it.
pub(crate) const BUF_ACTIVE: u8 = 2;
/// Published, due back with its fence.
pub(crate) const BUF_CONSUMER: u8 = 3;
/// No memory yet: only on a stream without a pool.
pub(crate) const BUF_ABSENT: u8 = 4;

/// smp_mb(): a full barrier.
pub(crate) fn smp_mb() {
    // SAFETY: a memory barrier, valid in any context.
    unsafe { bindings::smp_mb() }
}

/// smp_rmb(): orders this CPU's loads.
pub(crate) fn smp_rmb() {
    // SAFETY: a memory barrier, valid in any context.
    unsafe { bindings::smp_rmb() }
}

/// smp_wmb(): orders this CPU's stores.
pub(crate) fn smp_wmb() {
    // SAFETY: a memory barrier, valid in any context.
    unsafe { bindings::smp_wmb() }
}

impl Geom {
    /// The ring's words.
    fn words(&self) -> usize {
        self.ring_bytes / 8
    }

    /// One word of the ring, read once (READ_ONCE). A word past the ring reads 0.
    pub(crate) fn read(&self, at: usize) -> u64 {
        if at >= self.words() {
            return 0;
        }
        // SAFETY: `ring` is the stream's own vmalloc of `ring_bytes`, live as long as the stream, and
        // `at` is one of its words (checked above); the consumer may write it at any time, hence volatile.
        unsafe { ptr::read_volatile(self.ring.wrapping_add(at)) }
    }

    /// A head the consumer publishes: read, then the loads after it are ordered behind it.
    pub(crate) fn read_acquire(&self, at: usize) -> u64 {
        let v = self.read(at);
        smp_rmb();
        v
    }

    /// One word of the ring, written once (WRITE_ONCE). A word past the ring is not written.
    pub(crate) fn write(&self, at: usize, v: u64) {
        if at >= self.words() {
            return;
        }
        // SAFETY: as read: a word of the stream's own ring, which only the module's frame path writes.
        unsafe { ptr::write_volatile(self.ring.wrapping_add(at), v) }
    }

    /// Publishes a head, a tail or the state. A full barrier, not smp_wmb: moving a tail releases a
    /// slot this CPU has just read, so the earlier loads must be ordered too, or the consumer can
    /// reuse the slot under the read.
    pub(crate) fn write_release(&self, at: usize, v: u64) {
        smp_mb();
        self.write(at, v);
    }

    /// The pool page's ticket word of `buffer`.
    pub(crate) fn ticket_at(&self, buffer: u32) -> usize {
        self.page_at + POOL_TICKETS + buffer as usize
    }

    /// The pool page's cut-off word of reader slot `reader`.
    pub(crate) fn cut_off_at(&self, reader: usize) -> usize {
        self.page_at + POOL_TICKETS + self.buffers as usize + reader
    }

    /// The pool page's layout and shape, then its magic, then the ring's header: each magic last.
    pub(crate) fn init_ring(&self) {
        self.write(self.page_at + POOL_LAYOUT_WORD, POOL_LAYOUT);
        self.write(self.page_at + POOL_SHAPE_WORD, 1 | (self.buffers as u64) << 32);
        self.write_release(self.page_at, POOL_MAGIC);
        self.write(SHM_W_LAYOUT, SHM_LAYOUT);
        self.write(SHM_W_SHAPE, 1 | (self.frame_slots as u64) << 32);
        self.write(SHM_W_SIZES, SHM_ENTRY_BYTES as u64);
        self.write(SHM_W_TOTAL, self.shm_total as u64);
        self.write_release(SHM_W_MAGIC, SHM_MAGIC);
    }
}

impl Reader {
    /// Whether this slot's marks hold anything: a reader is registered in it and is not cut off.
    /// A slot with no reader in it.
    pub(crate) const fn empty() -> Self {
        Self {
            pages: KVec::new(),
            marks: ptr::null_mut(),
            max_held: 0,
            max_hold_ns: 0,
            cut_off: false,
            uncounted_since: 0,
            seen: KVVec::new(),
            id: 0,
        }
    }

    fn holds(&self) -> bool {
        !self.marks.is_null() && !self.cut_off
    }

    /// Word `i` of the marks page, read once. The page is the reader's own memory, mapped read-only
    /// here: it may change at any moment. A word past what is pinned reads 0.
    pub(crate) fn word(&self, i: usize) -> u64 {
        if self.marks.is_null() || i >= self.pages.len() * (bindings::PAGE_SIZE / 8) {
            return 0;
        }
        // SAFETY: `marks` is the vmap of the pinned pages, live while the slot holds it (it is
        // freed only after the slot is cleared under the lock), and `i` is inside it (checked above).
        unsafe { ptr::read_volatile(self.marks.wrapping_add(i)) }
    }

    /// The layout of the marks page (marks.rs layout_of): zero in the top byte is layout 1.
    pub(crate) fn layout(&self) -> u64 {
        match self.word(0) >> MARKS_LAYOUT_SHIFT {
            0 => 1,
            layout => layout,
        }
    }

    /// For `buffer`, below the stream's buffers: the use it was first seen marked in (+1), and when.
    fn seen(&mut self, buffer: u32) -> &mut [u64] {
        let at = 2 * buffer as usize;
        &mut self.seen[at..at + 2]
    }

    /// How many buffers the marks hold, counted at one instant (table.rs's count): the version must be
    /// even and the same before and after. None if it kept changing, so there was no such instant.
    /// Memory reads only.
    fn count(&self, buffers: u32) -> Option<u32> {
        for _ in 0..3 {
            let before = self.word(0);
            if before & 1 != 0 {
                continue;
            }
            smp_rmb();
            let mut n = 0;
            for b in 0..buffers as usize {
                n += u32::from(self.word(MARKS_AT + b) != 0);
            }
            smp_rmb();
            if self.word(0) == before {
                return Some(n);
            }
        }
        None
    }
}

fn stamp(seq: u64) -> u64 {
    seq.wrapping_add(1) << 1
}

fn busy(seq: u64) -> u64 {
    stamp(seq) | 1
}

/// Publishes frame `sample` (the frame ring's head before it) as the next sample of the one field.
/// The order is the one the readers rely on: busy stamp, fence, time, meta, lane sequence, payload,
/// finished stamp, index entry, lane head, head. Lock held; the ring can't be full: a reader that
/// falls behind skips ahead, it doesn't hold the writer back.
#[allow(clippy::too_many_arguments)]
pub(crate) fn publish(
    g: &Geom,
    f: &mut Frame,
    buffer: u32,
    bytes: u32,
    sequence: u64,
    identity: u64,
    fence: u32,
    timestamp_ns: u64,
    end_ns: u64,
) {
    let sample = f.frames_head;
    let lane_head = g.read(SHM_LANE_HEAD);
    let mask = u64::from(g.frame_slots) - 1;
    let lane_slot = (lane_head & mask) as usize;
    let at = g.shm_slots_at + lane_slot * SHM_SLOT_WORDS;
    let ticket = g.read(g.ticket_at(buffer));
    // The entry: one buffer, its even ticket, the frame's sequence, identity, bytes and fence.
    let entry: [u64; SHM_ENTRY_BYTES / 8] = [
        1 | 1 << 32, // generation 1, one buffer
        u64::from(buffer) | (ticket & 0xffff_ffff) << 32,
        sequence,
        identity,
        u64::from(bytes) | u64::from(fence) << 32,
        end_ns,
    ];

    g.write(at, busy(sample));
    smp_wmb();
    g.write(at + 1, timestamp_ns);
    g.write(at + 2, SHM_ENTRY_BYTES as u64); // field 0, flags 0
    g.write(at + 3, lane_head);
    for (i, word) in entry.iter().enumerate() {
        g.write(at + SHM_SLOT_HEADER + i, *word);
    }
    g.write_release(at, stamp(sample));
    g.write(SHM_INDEX + (sample & mask) as usize, (lane_slot as u64) << 16);
    g.write_release(SHM_LANE_HEAD, lane_head.wrapping_add(1));
    f.frames_head = f.frames_head.wrapping_add(1);
    g.write_release(SHM_W_HEAD, f.frames_head);
}

/// Whether any reader's marks hold `buffer`. Lock held; `buffer` is below the stream's buffers.
fn held(f: &Frame, buffer: u32) -> bool {
    f.reader.iter().any(|rd| rd.holds() && rd.word(MARKS_AT + buffer as usize) != 0)
}

/// Whether the newest frame has been read by every reader that has a marks page: each reader's taken
/// stamp (1 + buffers + b) is one more than the sequence it last read from b, so it is past
/// `newest_seq` once the reader has read the frame. docs/streams.md, the replacement rule: a frame the
/// consumer has read and let go counts as taken, and the newest frame may then be taken back. No
/// reader at all means nothing has read it. Lock held.
pub(crate) fn newest_read(g: &Geom, f: &Frame, buffer: u32) -> bool {
    let mut any = false;
    for rd in f.reader.iter().filter(|rd| rd.holds()) {
        if rd.word(MARKS_AT + g.buffers as usize + buffer as usize) <= f.newest_seq {
            return false;
        }
        any = true;
    }
    any
}

/// Applies every reader's limits at `now` (table.rs's enforce). A reader over its hold count, or
/// holding one buffer on one use for longer than its hold time, is cut off; so is one whose marks
/// could not be counted for longer than its hold time (or UNCOUNTED_NS without one). A cut-off
/// reader's marks hold nothing from then on, and its pool-page word says so. Lock held. Runs before
/// the buffers are taken back, so a limit is seen before the reuse.
pub(crate) fn enforce(g: &Geom, f: &mut Frame, now: u64) {
    for (r, rd) in f.reader.iter_mut().enumerate() {
        if !rd.holds() {
            continue;
        }
        let (max_held, max_hold_ns) = (rd.max_held, rd.max_hold_ns);
        let allowed = if max_hold_ns != 0 { max_hold_ns } else { UNCOUNTED_NS };
        let (mut over, mut stuck) = (false, false);
        match rd.count(g.buffers) {
            Some(held) => {
                rd.uncounted_since = 0;
                over = max_held != 0 && held > max_held;
            }
            None => {
                if rd.uncounted_since == 0 {
                    rd.uncounted_since = now;
                }
                stuck = now.wrapping_sub(rd.uncounted_since) > allowed;
            }
        }
        for b in 0..g.buffers {
            if rd.word(MARKS_AT + b as usize) == 0 {
                rd.seen(b)[0] = 0;
                continue;
            }
            let using = g.read(g.ticket_at(b)) >> 1;
            let seen = rd.seen(b);
            if seen[0] == using + 1 {
                if max_hold_ns != 0 && now.wrapping_sub(seen[1]) > max_hold_ns {
                    stuck = true;
                }
            } else {
                seen[0] = using + 1;
                seen[1] = now;
            }
        }
        if over || stuck {
            rd.cut_off = true;
            g.write_release(g.cut_off_at(r), 1);
        }
    }
}

/// Takes a published buffer back if no reader holds it (shm/ring-buffers' writer: retire, check the
/// holders, release). Retiring makes the ticket odd, so no reader can newly take the frame; the full
/// barrier orders that before the marks are read, and pairs with the reader's fence. A held buffer
/// stays published. Lock held. True if the buffer is free now.
pub(crate) fn reclaim(g: &Geom, f: &mut Frame, buffer: u32) -> bool {
    let state = f.buf(buffer).state;
    if state != BUF_CONSUMER {
        return state == BUF_FREE;
    }
    let at = g.ticket_at(buffer);
    let ticket = g.read(at);
    if ticket & 1 == 0 {
        g.write(at, ticket | 1);
    }
    smp_mb();
    if held(f, buffer) {
        return false;
    }
    // Odd to the next even value: release.
    g.write_release(at, (g.read(at) | 1).wrapping_add(1));
    f.buf(buffer).state = BUF_FREE;
    f.returned += 1;
    true
}

/// The consumer sees an end once. It may scribble on this word; the kernel's copy is what counts.
pub(crate) fn fault_locked(g: &Geom, f: &mut Frame, err: c_int) {
    if f.fault != 0 {
        return;
    }
    f.fault = err;
    g.write_release(g.w_state, lb::LEMNOS_RING_STATE_FAULT as u64);
}
