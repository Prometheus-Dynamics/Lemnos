//! A reader's marks.

use std::os::fd::OwnedFd;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use shm_ring::{Mem, MemMut, RingError, WriteMap, read_only, sealed_memfd};
use shm_ring_buffers::{BufferIndex, MAX_BUFFERS, Marker};

/// The layout of a marks page. Layout 1 has the version and one mark per buffer: `1 + b` words.
/// Layout 2 adds a taken stamp per buffer after the marks: `1 + 2b` words. A page of layout 1
/// never wrote its layout, so its layout byte is zero and reads as 1. Every attach checks it
/// first, and refuses a page of another layout with [`RingError::Layout`].
pub const LAYOUT: u64 = 2;

/// Where the layout sits in word 0: its top byte. The rest of the word is the version.
const LAYOUT_SHIFT: u32 = 56;
/// The version: the low 56 bits of word 0. It never reaches the layout byte, so the layout is
/// the same in every write.
const VERSION_MASK: u64 = (1 << LAYOUT_SHIFT) - 1;

/// The word of a marks page that holds its layout and its version: the version is odd while the
/// reader is changing a mark, and different after every change. It is what lets the publisher
/// know that the marks it counted were all true at one instant.
pub(crate) const VERSION_WORD: usize = 0;
/// The word of buffer 0's mark. Buffer `b`'s is `b` words after it.
pub(crate) const MARKS_AT: usize = 1;

/// Word 0 of a page of this build's layout with `version`.
pub(crate) const fn stamp(version: u64) -> u64 {
    (LAYOUT << LAYOUT_SHIFT) | (version & VERSION_MASK)
}

/// The layout of a marks page from its word 0. A page of layout 1 has no layout byte.
const fn layout_of(word: u64) -> u64 {
    match word >> LAYOUT_SHIFT {
        0 => 1,
        layout => layout,
    }
}

/// Refuses a marks page that is not of this build's layout. Called first by every attach.
pub(crate) fn check_layout(page: &impl Mem) -> Result<(), RingError> {
    if page.words() == 0 {
        return Err(RingError::Malformed("the marks page is empty"));
    }
    let found = layout_of(page.load(VERSION_WORD, Ordering::SeqCst));
    if found == LAYOUT {
        Ok(())
    } else {
        Err(RingError::Layout {
            what: "this marks page",
            found,
            reads: LAYOUT,
        })
    }
}

/// The word of buffer `buffer`'s taken stamp in a page for `buffers` buffers: after every mark. It
/// holds one more than the ring sequence of the last frame the reader took from that buffer, and
/// zero before any.
pub(crate) fn taken_at(buffers: u32, buffer: usize) -> usize {
    MARKS_AT + buffers as usize + buffer
}

/// A reader's page of marks for one pool: the version, then one word per buffer, nonzero while
/// the reader holds that buffer, then one taken stamp per buffer. Only this reader writes it.
#[derive(Debug)]
pub struct Marks<M: MemMut = WriteMap> {
    page: M,
    buffers: u32,
    slot: AtomicU32,
    max_held: u32,
    /// How many holds this reader has on each buffer, and in all. A buffer's word in the page
    /// is nonzero exactly while its count is. Frames can be dropped on other threads than the
    /// one reading, so the count and the page change together under this lock; it is never
    /// contended in the common case and then costs no system call.
    counts: Mutex<Counts>,
}

#[derive(Debug)]
struct Counts {
    each: Box<[u32]>,
    total: u32,
    version: u64,
}

impl Marks<WriteMap> {
    /// A page of marks for a pool of `buffers` buffers. `slot` is the reader slot the publisher
    /// gave this reader; `max_held` is how many buffers it may hold at once, zero for no limit.
    ///
    /// # Errors
    ///
    /// If the page can't be made.
    pub fn create(buffers: u32, slot: u32, max_held: u32) -> Result<Self, RingError> {
        let page = WriteMap::new(sealed_memfd(Self::page_bytes(buffers)?)?)?;
        page.store(VERSION_WORD, stamp(0), Ordering::SeqCst);
        Self::over(page, buffers, slot, max_held)
    }

    /// The size of a marks page of this layout for a pool of `buffers` buffers, in bytes: what
    /// [`create`](Self::create) makes.
    ///
    /// # Errors
    ///
    /// If `buffers` is zero or more than a pool may hold.
    pub fn page_bytes(buffers: u32) -> Result<usize, RingError> {
        if buffers == 0 || buffers > MAX_BUFFERS {
            return Err(RingError::Spec("a pool holds 1 to 1024 buffers"));
        }
        Ok((MARKS_AT + 2 * buffers as usize) * 8)
    }

    /// The page, writable, for the reader to keep (the reader's own mapping of it is
    /// [`over`](Self::over)). The publisher gets [`page_fd`](Self::page_fd) instead.
    ///
    /// # Errors
    ///
    /// If the descriptor can't be duplicated.
    pub fn writable_fd(&self) -> Result<OwnedFd, RingError> {
        Ok(self.page.fd().try_clone_to_owned()?)
    }

    /// The page, read-only, to send to the publisher.
    ///
    /// # Errors
    ///
    /// If the descriptor can't be reopened, which needs `/proc`.
    pub fn page_fd(&self) -> Result<OwnedFd, RingError> {
        Ok(read_only(self.page.fd())?)
    }
}

impl<M: MemMut> Marks<M> {
    /// Marks kept in `page`, a page of this layout with the version and two words per buffer:
    /// one [`create`](Self::create) made, or one a reader was given.
    ///
    /// # Errors
    ///
    /// [`RingError::Layout`] if the page is not of this layout, which is checked before anything
    /// else. [`RingError::Malformed`] if it is empty.
    pub fn over(page: M, buffers: u32, slot: u32, max_held: u32) -> Result<Self, RingError> {
        check_layout(&page)?;
        let version = page.load(VERSION_WORD, Ordering::SeqCst) & VERSION_MASK;
        let buffers = (buffers as usize).min(page.words().saturating_sub(MARKS_AT) / 2);
        Ok(Self {
            page,
            buffers: buffers as u32,
            slot: AtomicU32::new(slot),
            max_held,
            counts: Mutex::new(Counts {
                each: vec![0; buffers].into_boxed_slice(),
                total: 0,
                version,
            }),
        })
    }

    /// Changes the reader slot, when the publisher's side gives this reader another one: a
    /// ring owner that restarted hands slots out afresh.
    pub fn set_slot(&self, slot: u32) {
        self.slot.store(slot, Ordering::Relaxed);
    }

    /// How many buffers this reader holds now.
    #[must_use]
    pub fn held(&self) -> u32 {
        self.counts().total
    }

    /// Writes one mark, with the version odd while it is being written and moved on after.
    /// Called with the lock held, so the version only ever has one writer.
    fn change(&self, counts: &mut Counts, buffer: BufferIndex, mark: u64) {
        counts.version = counts.version.wrapping_add(1) & VERSION_MASK;
        self.page
            .store(VERSION_WORD, stamp(counts.version), Ordering::SeqCst);
        self.page
            .store(MARKS_AT + buffer.0 as usize, mark, Ordering::SeqCst);
        counts.version = counts.version.wrapping_add(1) & VERSION_MASK;
        self.page
            .store(VERSION_WORD, stamp(counts.version), Ordering::SeqCst);
    }

    fn counts(&self) -> std::sync::MutexGuard<'_, Counts> {
        // A panic while holding the lock leaves the counts as they were, which is still right.
        self.counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl<M: MemMut> Marker for Marks<M> {
    fn mark(&self, buffer: BufferIndex) -> bool {
        let mut counts = self.counts();
        let Some(count) = counts.each.get(buffer.0 as usize).copied() else {
            return false;
        };
        if count == 0 {
            if self.max_held != 0 && counts.total >= self.max_held {
                return false;
            }
            counts.total += 1;
            // Visible to the publisher before the reader checks the ticket: the fence that
            // orders the two is in `page::still`, which the reader calls next.
            self.change(&mut counts, buffer, 1);
        }
        counts.each[buffer.0 as usize] = count + 1;
        true
    }

    fn unmark(&self, buffer: BufferIndex) {
        let mut counts = self.counts();
        let Some(count) = counts.each.get(buffer.0 as usize).copied() else {
            return;
        };
        match count {
            0 => {}
            1 => {
                counts.each[buffer.0 as usize] = 0;
                counts.total -= 1;
                self.change(&mut counts, buffer, 0);
            }
            more => counts.each[buffer.0 as usize] = more - 1,
        }
    }

    fn slot(&self) -> Option<u32> {
        Some(self.slot.load(Ordering::Relaxed))
    }

    fn taken(&self, buffer: BufferIndex, seq: u64) {
        if buffer.0 < self.buffers {
            self.page.store(
                taken_at(self.buffers, buffer.0 as usize),
                seq.saturating_add(1),
                Ordering::SeqCst,
            );
        }
    }
}

/// Whether the reader whose page is `page` marks `buffer`. The publisher's read.
pub(crate) fn marked(page: &impl Mem, buffer: BufferIndex) -> bool {
    page.load(MARKS_AT + buffer.0 as usize, Ordering::SeqCst) != 0
}

/// How many of `buffers` buffers the reader marks, counted at one instant. `None` if the reader
/// kept changing its marks while they were being counted, so there is no such instant to speak
/// of.
pub(crate) fn count(page: &impl Mem, buffers: u32) -> Option<u32> {
    for _ in 0..SNAPSHOT_TRIES {
        let before = page.load(VERSION_WORD, Ordering::SeqCst);
        if before & 1 == 1 {
            continue;
        }
        let marks = (0..buffers)
            .filter(|buffer| marked(page, BufferIndex(*buffer)))
            .count();
        if page.load(VERSION_WORD, Ordering::SeqCst) == before {
            return Some(marks as u32);
        }
    }
    None
}

/// How many times a count is tried before giving up on it.
const SNAPSHOT_TRIES: u32 = 3;
