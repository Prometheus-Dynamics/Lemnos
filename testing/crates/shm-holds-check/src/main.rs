//! `lemnos-shm-holds-check`: a reader's marks keep a buffer from reuse while the reader holds it.
//!
//! ```text
//! lemnos-shm-holds-check <control node> <block name> <heap node>
//! ```
//!
//! Makes a stream on the test block with `LEMNOS_STREAM_CREATE_SHM`, gives the module a reader's
//! marks page (`shm_ring_holds::Marks`) through `LEMNOS_STREAM_IOC_READER`, and starts the test
//! source. The ring is read with the shm crates, and each buffer through a read-only mapping of
//! the pool. Each frame's payload is its sequence number, so a buffer's first word says which
//! frame it holds.
//!
//! 1. Hold: the first frame whose buffer the reader can mark and still find in place (the same
//!    ticket) is held for `HOLD_FRAMES` more frames. While it is held, its buffer keeps that
//!    frame's sequence, no later frame is published into it, and other buffers do get reused.
//! 2. Release: the mark is removed. Within `WAIT`, the buffer is reused: its ticket changes and
//!    its first word holds a later frame.
//!
//! The module's side is pinned by these numbers: a held buffer that the module reuses fails the
//! check, as does a released one that never comes back.
//!
//! `release` mode: a reader's registration is freed when its file closes, and by the explicit
//! unregister. A reader that holds a buffer and exits without unregistering frees its buffer and
//! its slot; the same holds for `LEMNOS_READER_IOC_UNREGISTER`; and 200 arrive-and-leave cycles
//! leave the table with its slots.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsFd;
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lemnos_linux_sys::dma_heap;
use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::module;
use lemnos_linux_sys::stream::{self, CREATE_SHM};
use shm_ring::{Mem, Next, Reader};
use shm_ring_buffers::Marker;
use shm_ring_buffers::{BufferIndex, entry, page};
use shm_ring_holds::Marks;

const BUFFERS: u32 = 4;
const BUFFER_BYTES: u32 = 4096;
/// Each buffer is this many bytes in the pool: the buffer size, aligned as the module aligns it.
const STRIDE: usize = 4096;
const FRAME_SLOTS: u32 = 16;
const JOB_SLOTS: u32 = 4;
/// `sizeof(struct lemnos_stream_job)`.
const JOB_BYTES: u32 = 80;
/// The shm ring's words for one field of `FRAME_SLOTS` samples (as `shm-ring-check`).
const RING_WORDS: usize = 17 + 10 * FRAME_SLOTS as usize;
/// The pool page starts after the ring.
const PAGE_AT: usize = RING_WORDS;
/// Frames the reader holds its buffer across. Four buffers and one held leave three to reuse.
const HOLD_FRAMES: u64 = 40;
/// Frames the test source makes: a frame every millisecond, so the phases fit.
const FRAMES: u32 = 1000;
/// The most a phase may take.
const WAIT: Duration = Duration::from_secs(10);
/// The most readers' marks the reader allows itself to hold at once.
const MAX_HELD: u32 = 2;

/// A window of a mapping's words, from `base`, as a ring's memory.
struct Words<'a> {
    map: &'a Mapping,
    base: usize,
}

impl Mem for Words<'_> {
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

/// What the check prints and counts.
struct Report {
    failures: u32,
}

impl Report {
    fn check(&mut self, ok: bool, what: impl std::fmt::Display) {
        let _ = writeln!(
            std::io::stdout(),
            "{} {what}",
            if ok { "ok  " } else { "FAIL" }
        );
        self.failures += u32::from(!ok);
    }
}

fn step<T, E: std::fmt::Debug>(what: &str, result: Result<T, E>) -> Result<T, String> {
    result.map_err(|e| format!("{what}: {e:?}"))
}

/// Where the check is in its run.
enum Phase {
    /// Looking for a frame to hold.
    Seek,
    /// Holding `buffer`, which carries frame `sequence` with ticket `ticket`.
    Hold {
        buffer: u32,
        ticket: u32,
        sequence: u64,
        others: [bool; BUFFERS as usize],
        changed: Option<u64>,
        reused: bool,
    },
    /// Released `buffer`, which carried frame `sequence` with ticket `ticket`.
    Release {
        buffer: u32,
        ticket: u32,
        sequence: u64,
    },
}

fn run(
    node_path: &str,
    block_name: &str,
    heap_path: &str,
    report: &mut Report,
) -> Result<(), String> {
    // The module's test source keeps a stream busy until it stops, so every path that made one
    // stops it: the next check counts the streams the module holds.
    let kept = std::cell::RefCell::new(None);
    let outcome = run_with(node_path, block_name, heap_path, report, &kept);
    if let Some(stream) = kept.borrow_mut().take() {
        let _ = stream::test_source(stream.fd.as_fd(), 0, 0);
    }
    outcome
}

fn run_with(
    node_path: &str,
    block_name: &str,
    heap_path: &str,
    report: &mut Report,
    kept: &std::cell::RefCell<Option<stream::Stream>>,
) -> Result<(), String> {
    let node = step(
        "open the control node",
        OpenOptions::new().read(true).write(true).open(node_path),
    )?;
    let block = step(
        "find the block",
        module::find_block(node.as_fd(), block_name),
    )?
    .ok_or_else(|| format!("no bound block named {block_name}"))?;
    let heap = step(
        "open the heap",
        OpenOptions::new().read(true).write(true).open(heap_path),
    )?;
    let pool_bytes = u64::from(BUFFERS) * STRIDE as u64;
    let pool = step(
        "allocate the pool",
        dma_heap::allocate(heap.as_fd(), pool_bytes),
    )?;

    let stream = step(
        "create the stream",
        stream::create(
            node.as_fd(),
            &stream::Spec {
                block,
                flags: CREATE_SHM,
                buffers: BUFFERS,
                buffer_bytes: BUFFER_BYTES,
                frame_slots: FRAME_SLOTS,
                job_slots: JOB_SLOTS,
                job_bytes: JOB_BYTES,
                pool: Some(pool.as_fd()),
            },
        ),
    )?;
    let ring = step(
        "map the ring",
        Mapping::new(stream.fd.as_fd(), stream.ring_bytes, 0, false),
    )?;
    let buffers = step(
        "map the pool",
        Mapping::new(pool.as_fd(), pool_bytes as usize, 0, false),
    )?;

    // The reader's marks: the module pins the read-only descriptor and gives the slot back.
    let marks = step("make the marks page", Marks::create(BUFFERS, 0, MAX_HELD))?;
    let page_fd = step("read-only marks descriptor", marks.page_fd())?;
    let registration = step(
        "give the module the marks page",
        stream::add_reader(stream.fd.as_fd(), page_fd.as_fd(), MAX_HELD, 0),
    )?;
    marks.set_slot(registration.slot);

    step(
        "start the test source",
        stream::test_source(stream.fd.as_fd(), 1000, FRAMES),
    )?;
    *kept.borrow_mut() = Some(stream);

    let mut reader = step(
        "attach to the ring",
        Reader::over_from_start(Words {
            map: &ring,
            base: 0,
        }),
    )?;
    let page_words = Words {
        map: &ring,
        base: PAGE_AT,
    };
    let word_of = |buffer: u32| -> Option<u64> {
        buffers
            .load_words()?
            .load(buffer as usize * STRIDE / 8, Ordering::Acquire)
    };

    let mut phase = Phase::Seek;
    let mut holds_marked = 0_u32;
    let deadline = Instant::now() + WAIT * 3;
    let mut hold_done: Option<(u64, bool, bool, u32)> = None;
    let mut reuse_seen: Option<(u64, u64)> = None;
    while Instant::now() < deadline {
        match reader.next() {
            Next::Sample(sample) => {
                let sequence = sample.seq;
                let Some(e) = entry::read(sample.payload) else {
                    continue;
                };
                let buffer = e.buffers[0].0.0;
                let ticket = e.buffers[0].1;
                phase = match phase {
                    Phase::Seek => {
                        if marks.mark(BufferIndex(buffer)) {
                            holds_marked += 1;
                            if page::still(&page_words, BufferIndex(buffer), ticket) {
                                Phase::Hold {
                                    buffer,
                                    ticket,
                                    sequence,
                                    others: [false; BUFFERS as usize],
                                    changed: None,
                                    reused: false,
                                }
                            } else {
                                marks.unmark(BufferIndex(buffer));
                                Phase::Seek
                            }
                        } else {
                            Phase::Seek
                        }
                    }
                    Phase::Hold {
                        buffer: held,
                        ticket: held_ticket,
                        sequence: held_seq,
                        mut others,
                        mut changed,
                        mut reused,
                    } => {
                        if sequence > held_seq && buffer == held {
                            reused = true;
                        }
                        if sequence > held_seq && buffer != held {
                            others[buffer as usize] = true;
                        }
                        if let Some(word) = word_of(held).filter(|w| *w != held_seq) {
                            changed.get_or_insert(word);
                        }
                        if sequence >= held_seq + HOLD_FRAMES {
                            marks.unmark(BufferIndex(held));
                            hold_done = Some((
                                held_seq,
                                changed.is_none() && !reused,
                                others.iter().filter(|o| **o).count() >= 2,
                                held,
                            ));
                            Phase::Release {
                                buffer: held,
                                ticket: held_ticket,
                                sequence: held_seq,
                            }
                        } else {
                            Phase::Hold {
                                buffer: held,
                                ticket: held_ticket,
                                sequence: held_seq,
                                others,
                                changed,
                                reused,
                            }
                        }
                    }
                    Phase::Release {
                        buffer: released,
                        ticket: released_ticket,
                        sequence: released_seq,
                    } => {
                        // The ticket goes even when the buffer is freed, and the frame is written
                        // after that: reuse is seen when the first word holds a later frame.
                        let now = page::ticket(&page_words, BufferIndex(released));
                        let word = word_of(released).unwrap_or(0);
                        if now != u64::from(released_ticket) && word > released_seq {
                            reuse_seen = Some((released_seq, word));
                            break;
                        }
                        Phase::Release {
                            buffer: released,
                            ticket: released_ticket,
                            sequence: released_seq,
                        }
                    }
                };
            }
            Next::Empty => std::thread::sleep(Duration::from_millis(1)),
            // Frames the reader was too slow for: the ones it holds are still checked in place.
            Next::Skipped(_) => {}
        }
    }

    report.check(
        holds_marked > 0 && hold_done.is_some(),
        format_args!("a frame was held for {HOLD_FRAMES} frames (marked {holds_marked} times)"),
    );
    if let Some((held_seq, unchanged, others_reused, held)) = hold_done {
        report.check(
            unchanged,
            format_args!(
                "the held buffer {held} kept frame {held_seq}'s payload and was never published into again"
            ),
        );
        report.check(
            others_reused,
            "while it was held, the other buffers were reused (two or more published frames)",
        );
    } else {
        report.check(false, "the hold phase completed");
    }
    match reuse_seen {
        Some((seq, word)) => report.check(
            word > seq,
            format_args!("after release the buffer was reused: frame {seq} gave way to a later one (first word {word})"),
        ),
        None => report.check(false, "after release the buffer was reused within the wait"),
    }
    drop(reader);
    drop(marks);
    Ok(())
}

/// One cut-off scenario: how many buffers the reader holds, the limits the module is given, and
/// whether the module must cut it off.
struct Scenario {
    name: &'static str,
    holds: usize,
    max_held: u32,
    max_hold: Duration,
    cut_expected: bool,
}

/// A buffer the reader holds: its frame's ring sequence and the entry's ticket.
#[derive(Clone, Copy)]
struct Hold {
    buffer: u32,
    ticket: u32,
    sequence: u64,
}

/// Holds buffers under a limit and checks the module's cut-off. A reader that is over its limit
/// must be cut off, and then its buffers are reused. A reader within its limits is left alone.
fn scenario(
    node_path: &str,
    block_name: &str,
    heap_path: &str,
    report: &mut Report,
    sc: &Scenario,
) -> Result<(), String> {
    let node = step(
        "open the control node",
        OpenOptions::new().read(true).write(true).open(node_path),
    )?;
    let block = step(
        "find the block",
        module::find_block(node.as_fd(), block_name),
    )?
    .ok_or_else(|| format!("no bound block named {block_name}"))?;
    let heap = step(
        "open the heap",
        OpenOptions::new().read(true).write(true).open(heap_path),
    )?;
    let pool_bytes = u64::from(BUFFERS) * STRIDE as u64;
    let pool = step(
        "allocate the pool",
        dma_heap::allocate(heap.as_fd(), pool_bytes),
    )?;
    let stream = step(
        "create the stream",
        stream::create(
            node.as_fd(),
            &stream::Spec {
                block,
                flags: CREATE_SHM,
                buffers: BUFFERS,
                buffer_bytes: BUFFER_BYTES,
                frame_slots: FRAME_SLOTS,
                job_slots: JOB_SLOTS,
                job_bytes: JOB_BYTES,
                pool: Some(pool.as_fd()),
            },
        ),
    )?;
    let ring = step(
        "map the ring",
        Mapping::new(stream.fd.as_fd(), stream.ring_bytes, 0, false),
    )?;
    let buffers = step(
        "map the pool",
        Mapping::new(pool.as_fd(), pool_bytes as usize, 0, false),
    )?;
    // The consumer marks without a limit of its own: the module's limits are the ones tested.
    let marks = step("make the marks page", Marks::create(BUFFERS, 0, 0))?;
    let page_fd = step("read-only marks descriptor", marks.page_fd())?;
    let registration = step(
        "give the module the marks page",
        stream::add_reader(
            stream.fd.as_fd(),
            page_fd.as_fd(),
            sc.max_held,
            u64::try_from(sc.max_hold.as_nanos()).unwrap_or(u64::MAX),
        ),
    )?;
    let slot = registration.slot;
    marks.set_slot(slot);
    step(
        "start the test source",
        stream::test_source(stream.fd.as_fd(), 1000, FRAMES),
    )?;

    let mut reader = step(
        "attach to the ring",
        Reader::over_from_start(Words {
            map: &ring,
            base: 0,
        }),
    )?;
    let page_words = Words {
        map: &ring,
        base: PAGE_AT,
    };
    let word_of = |buffer: u32| -> Option<u64> {
        buffers
            .load_words()?
            .load(buffer as usize * STRIDE / 8, Ordering::Acquire)
    };
    // Whether a held buffer has been reused: its ticket moved on and a later frame is in it.
    let reused = |h: &Hold| -> bool {
        page::ticket(&page_words, BufferIndex(h.buffer)) != u64::from(h.ticket)
            && word_of(h.buffer).is_some_and(|w| w > h.sequence)
    };

    let mut held: Vec<Hold> = Vec::new();
    let mut after = 0_u64;
    let deadline = Instant::now() + WAIT * 3;
    let mut cut = false;
    while Instant::now() < deadline {
        match reader.next() {
            Next::Sample(sample) => {
                let Some(e) = entry::read(sample.payload) else {
                    continue;
                };
                let buffer = e.buffers[0].0.0;
                let ticket = e.buffers[0].1;
                if held.len() < sc.holds {
                    if !held.iter().any(|h| h.buffer == buffer) && marks.mark(BufferIndex(buffer)) {
                        if page::still(&page_words, BufferIndex(buffer), ticket) {
                            held.push(Hold {
                                buffer,
                                ticket,
                                sequence: sample.seq,
                            });
                        } else {
                            marks.unmark(BufferIndex(buffer));
                        }
                    }
                } else {
                    after += 1;
                }
                cut = page::is_cut_off(&page_words, BUFFERS, slot);
                if sc.cut_expected {
                    if cut && held.len() == sc.holds && held.iter().all(reused) {
                        break;
                    }
                } else if held.len() == sc.holds && after >= 300 {
                    break;
                }
            }
            Next::Empty => std::thread::sleep(Duration::from_millis(1)),
            Next::Skipped(_) => {}
        }
    }
    report.check(
        held.len() == sc.holds,
        format_args!("{}: {} of {} buffers held", sc.name, held.len(), sc.holds),
    );
    if sc.cut_expected {
        report.check(
            cut,
            format_args!("{}: the module cut the reader off", sc.name),
        );
        report.check(
            !held.is_empty() && held.iter().all(reused),
            format_args!(
                "{}: the buffers it held were reused after the cut-off",
                sc.name
            ),
        );
    } else {
        report.check(
            !cut,
            format_args!("{}: a reader within its limits is not cut off", sc.name),
        );
        report.check(
            !held.is_empty() && held.iter().all(|h| !reused(h)),
            format_args!("{}: its buffers are still its own", sc.name),
        );
    }
    drop(reader);
    drop(marks);
    drop(stream);
    Ok(())
}

/// The frames the release scenarios' source makes: a frame a millisecond, enough for every phase.
const RELEASE_FRAMES: u32 = 20_000;
/// Arrive-and-leave cycles the reader table must survive (its size is `LEMNOS_STREAM_MAX_READERS`).
const CYCLES: u32 = 200;

/// A stream on the test block with its ring and pool mapped. Its source stops when it drops.
struct Fixture {
    stream: stream::Stream,
    ring: Mapping,
    pool: Mapping,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = stream::test_source(self.stream.fd.as_fd(), 0, 0);
    }
}

fn fixture(node_path: &str, block_name: &str, heap_path: &str) -> Result<Fixture, String> {
    let node = step(
        "open the control node",
        OpenOptions::new().read(true).write(true).open(node_path),
    )?;
    let block = step(
        "find the block",
        module::find_block(node.as_fd(), block_name),
    )?
    .ok_or_else(|| format!("no bound block named {block_name}"))?;
    let heap = step(
        "open the heap",
        OpenOptions::new().read(true).write(true).open(heap_path),
    )?;
    let pool_bytes = u64::from(BUFFERS) * STRIDE as u64;
    let pool_fd = step(
        "allocate the pool",
        dma_heap::allocate(heap.as_fd(), pool_bytes),
    )?;
    let stream = step(
        "create the stream",
        stream::create(
            node.as_fd(),
            &stream::Spec {
                block,
                flags: CREATE_SHM,
                buffers: BUFFERS,
                buffer_bytes: BUFFER_BYTES,
                frame_slots: FRAME_SLOTS,
                job_slots: JOB_SLOTS,
                job_bytes: JOB_BYTES,
                pool: Some(pool_fd.as_fd()),
            },
        ),
    )?;
    let ring = step(
        "map the ring",
        Mapping::new(stream.fd.as_fd(), stream.ring_bytes, 0, false),
    )?;
    let pool = step(
        "map the pool",
        Mapping::new(pool_fd.as_fd(), pool_bytes as usize, 0, false),
    )?;
    Ok(Fixture { stream, ring, pool })
}

/// The first word of `buffer` in the pool: its frame's sequence, once a frame is in it.
fn first_word(pool: &Mapping, buffer: u32) -> Option<u64> {
    pool.load_words()?
        .load(buffer as usize * STRIDE / 8, Ordering::Acquire)
}

/// Whether `buffer`, which held frame `sequence` under `ticket`, has been reused since.
fn reused_since(fx: &Fixture, buffer: u32, ticket: u32, sequence: u64) -> bool {
    let page_words = Words {
        map: &fx.ring,
        base: PAGE_AT,
    };
    page::ticket(&page_words, BufferIndex(buffer)) != u64::from(ticket)
        && first_word(&fx.pool, buffer).is_some_and(|w| w > sequence)
}

/// Marks the first frame the reader can mark and still find in place, within `WAIT`. Returns its
/// buffer, its ticket and its sequence.
fn hold_first(fx: &Fixture, marks: &Marks) -> Option<(u32, u32, u64)> {
    let mut reader = Reader::over_from_start(Words {
        map: &fx.ring,
        base: 0,
    })
    .ok()?;
    let page_words = Words {
        map: &fx.ring,
        base: PAGE_AT,
    };
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match reader.next() {
            Next::Sample(sample) => {
                let Some(e) = entry::read(sample.payload) else {
                    continue;
                };
                let buffer = e.buffers[0].0.0;
                let ticket = e.buffers[0].1;
                if marks.mark(BufferIndex(buffer)) {
                    if page::still(&page_words, BufferIndex(buffer), ticket) {
                        return Some((buffer, ticket, sample.seq));
                    }
                    marks.unmark(BufferIndex(buffer));
                }
            }
            Next::Empty => std::thread::sleep(Duration::from_millis(1)),
            Next::Skipped(_) => {}
        }
    }
    None
}

/// Consumes the ring until `buffer` is reused, within `WAIT`: how long it took, if it did.
fn wait_reused(fx: &Fixture, buffer: u32, ticket: u32, sequence: u64) -> Option<Duration> {
    let mut reader = Reader::over_from_start(Words {
        map: &fx.ring,
        base: 0,
    })
    .ok()?;
    let start = Instant::now();
    while start.elapsed() < WAIT {
        if reused_since(fx, buffer, ticket, sequence) {
            return Some(start.elapsed());
        }
        match reader.next() {
            Next::Sample(_) | Next::Skipped(_) => {}
            Next::Empty => std::thread::sleep(Duration::from_micros(200)),
        }
    }
    None
}

/// Registers a reader on the stream with its own marks page, and the page's marks.
fn register(fx: &Fixture, marks: &Marks) -> Result<stream::Registration, String> {
    let page_fd = step("read-only marks descriptor", marks.page_fd())?;
    let registration = step(
        "register the reader",
        stream::add_reader(fx.stream.fd.as_fd(), page_fd.as_fd(), 0, 0),
    )?;
    marks.set_slot(registration.slot);
    Ok(registration)
}

/// The release scenarios: a registration's file frees its slot and its marks when it closes, or
/// through the unregister ioctl, and the reader table takes arrive-and-leave cycles.
fn release(
    node_path: &str,
    block_name: &str,
    heap_path: &str,
    report: &mut Report,
) -> Result<(), String> {
    // 1. A reader holds a buffer and exits without unregistering: its file closes, so the buffer
    // is reused and its slot is free.
    {
        let fx = fixture(node_path, block_name, heap_path)?;
        let marks = step("make the marks page", Marks::create(BUFFERS, 0, 0))?;
        let registration = register(&fx, &marks)?;
        let slot = registration.slot;
        step(
            "start the test source",
            stream::test_source(fx.stream.fd.as_fd(), 1000, RELEASE_FRAMES),
        )?;
        let held = hold_first(&fx, &marks);
        report.check(held.is_some(), "exit: the reader holds a buffer");
        if let Some((buffer, ticket, sequence)) = held {
            // The process exits: its file closes, and the marks it left behind hold nothing.
            drop(registration);
            let reused = wait_reused(&fx, buffer, ticket, sequence);
            report.check(
                reused.is_some(),
                format_args!(
                    "exit without unregistering: the buffer is reused ({})",
                    reused.map_or_else(|| format!("not within {WAIT:?}"), |d| format!("{d:?}")),
                ),
            );
            let spare = step("make the marks page", Marks::create(BUFFERS, 0, 0))?;
            let again = register(&fx, &spare)?;
            report.check(
                again.slot == slot,
                format_args!(
                    "exit without unregistering: slot {slot} is free again ({})",
                    again.slot
                ),
            );
        } else {
            drop(registration);
        }
    }

    // 2. The same, through LEMNOS_READER_IOC_UNREGISTER: at once, and only once.
    {
        let fx = fixture(node_path, block_name, heap_path)?;
        let marks = step("make the marks page", Marks::create(BUFFERS, 0, 0))?;
        let registration = register(&fx, &marks)?;
        step(
            "start the test source",
            stream::test_source(fx.stream.fd.as_fd(), 1000, RELEASE_FRAMES),
        )?;
        if let Some((buffer, ticket, sequence)) = hold_first(&fx, &marks) {
            let freed = stream::unregister(registration.file.as_fd());
            report.check(freed.is_ok(), format_args!("unregister: {freed:?}"));
            let twice = stream::unregister(registration.file.as_fd());
            report.check(
                twice
                    .as_ref()
                    .err()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound),
                format_args!("unregister again: {twice:?}"),
            );
            let reused = wait_reused(&fx, buffer, ticket, sequence);
            report.check(
                reused.is_some(),
                format_args!(
                    "unregister: the buffer is reused ({})",
                    reused.map_or_else(|| format!("not within {WAIT:?}"), |d| format!("{d:?}")),
                ),
            );
        } else {
            report.check(false, "unregister: the reader holds a buffer");
        }
        drop(registration);
    }

    // 3. Arrive and leave, over and over: every registration gets a slot.
    {
        let fx = fixture(node_path, block_name, heap_path)?;
        let marks =
            Marks::create(BUFFERS, 0, 0).map_err(|e| format!("make the marks page: {e:?}"))?;
        let page_fd = step("read-only marks descriptor", marks.page_fd())?;
        let mut granted = 0_u32;
        let mut refused = None;
        for _ in 0..CYCLES {
            match stream::add_reader(fx.stream.fd.as_fd(), page_fd.as_fd(), 0, 0) {
                Ok(registration) => {
                    granted += 1;
                    drop(registration);
                }
                Err(e) => {
                    refused = Some(e);
                    break;
                }
            }
        }
        report.check(
            granted == CYCLES,
            format_args!("{CYCLES} arrive-and-leave cycles: {granted} granted ({refused:?})"),
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (node, block, heap, mode) = match args.as_slice() {
        [node, block, heap] => (node, block, heap, ""),
        [node, block, heap, m] if m == "cutoff" || m == "release" => {
            (node, block, heap, m.as_str())
        }
        _ => {
            let _ = writeln!(
                std::io::stderr(),
                "usage: lemnos-shm-holds-check <control node> <block name> <heap node> [cutoff|release]"
            );
            return ExitCode::from(2);
        }
    };
    let mut report = Report { failures: 0 };
    let outcome = if mode == "release" {
        release(node, block, heap, &mut report)
    } else if mode == "cutoff" {
        [
            Scenario {
                name: "over the hold count",
                holds: 2,
                max_held: 1,
                max_hold: Duration::ZERO,
                cut_expected: true,
            },
            Scenario {
                name: "over the hold time",
                holds: 1,
                max_held: 0,
                max_hold: Duration::from_millis(100),
                cut_expected: true,
            },
            Scenario {
                name: "within its limits",
                holds: 1,
                max_held: 1,
                max_hold: Duration::ZERO,
                cut_expected: false,
            },
        ]
        .iter()
        .try_for_each(|sc| scenario(node, block, heap, &mut report, sc))
    } else {
        run(node, block, heap, &mut report)
    };
    if let Err(error) = outcome {
        report.check(false, error);
    }
    let _ = writeln!(
        std::io::stdout(),
        "lemnos-shm-holds-check: {} failed",
        report.failures
    );
    if report.failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
