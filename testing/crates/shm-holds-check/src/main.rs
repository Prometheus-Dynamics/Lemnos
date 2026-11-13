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
    let slot = step(
        "give the module the marks page",
        stream::add_reader(stream.fd.as_fd(), page_fd.as_fd(), MAX_HELD),
    )?;
    marks.set_slot(slot);

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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [node, block, heap] = args.as_slice() else {
        let _ = writeln!(
            std::io::stderr(),
            "usage: lemnos-shm-holds-check <control node> <block name> <heap node>"
        );
        return ExitCode::from(2);
    };
    let mut report = Report { failures: 0 };
    if let Err(error) = run(node, block, heap, &mut report) {
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
