//! `lemnos-shm-ring-check`: a frame ring the module wrote in the shm layout, read with the shm
//! crates and nothing else.
//!
//! ```text
//! lemnos-shm-ring-check <control node> <block name> <heap node>
//! ```
//!
//! Makes a stream on the test block with `LEMNOS_STREAM_CREATE_SHM`, starts the module's test
//! source, maps the ring read-only, and reads it with `shm_ring::Reader` (which checks the
//! header against the shape it derives) and `shm_ring_buffers` (the pool page and the entries). It
//! checks the first frames: their sequence numbers, times, buffers, tickets and entries. A frame
//! needs a free buffer and nothing returns them here, so the check takes as many frames as there
//! are buffers.
//!
//! The C writer's numbers are pinned here by the reader: a header, slot or page that the reader's
//! derived shape doesn't match is refused, so drift between the module and the crates fails the
//! check, not a board.

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
use lemnos_linux_sys::time::monotonic_ns;
use shm_ring::{Mem, Reader};
use shm_ring_buffers::{BufferIndex, entry, page};

const BUFFERS: u32 = 4;
const BUFFER_BYTES: u32 = 4096;
const FRAME_SLOTS: u32 = 4;
const JOB_SLOTS: u32 = 4;
/// `sizeof(struct lemnos_stream_job)`.
const JOB_BYTES: u32 = 80;
/// The shm ring's words for one field of `FRAME_SLOTS` samples: the header (16), the lane head,
/// the index (`FRAME_SLOTS`), then the slots (`FRAME_SLOTS` of 9 words: 4 header words and the
/// 40-byte entry).
const RING_WORDS: usize = 17 + 10 * FRAME_SLOTS as usize;
/// The pool page starts after the ring.
const PAGE_AT: usize = RING_WORDS;
/// The entry's metadata, after its 8-byte header and one buffer: sequence, identity, bytes, fence.
const META_BYTES: usize = 24;
const WAIT: Duration = Duration::from_secs(10);

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

/// One entry, read out of its payload.
#[derive(Debug)]
struct Entry {
    generation: u32,
    count: usize,
    buffer: u32,
    ticket: u32,
    meta: Option<Meta>,
}

/// The entry's metadata: the frame's sequence, identity, bytes and fence.
#[derive(Debug)]
struct Meta {
    sequence: u64,
    identity: u64,
    bytes: u32,
    fence: u32,
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
    let pool_bytes = u64::from(BUFFERS) * u64::from(BUFFER_BYTES.next_multiple_of(4096));
    let pool = step(
        "allocate the pool",
        dma_heap::allocate(heap.as_fd(), pool_bytes),
    )?;
    let started = step("read the clock", monotonic_ns())?;

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
    let map = step(
        "map the ring",
        Mapping::new(stream.fd.as_fd(), stream.ring_bytes, 0, false),
    )?;
    step(
        "start the test source",
        stream::test_source(stream.fd.as_fd(), 1000, BUFFERS),
    )?;
    *kept.borrow_mut() = Some(stream);

    // The reader checks the header against the shape the crate derives, and refuses a mismatch.
    let mut reader = step(
        "attach to the ring",
        Reader::over_from_start(Words { map: &map, base: 0 }),
    )?;
    let mut samples = Vec::new();
    let deadline = Instant::now() + WAIT;
    while (samples.len() as u32) < BUFFERS && Instant::now() < deadline {
        let next = reader.next();
        match next {
            shm_ring::Next::Sample(sample) => {
                let time = sample.time.as_nanos();
                let payload = sample.payload;
                let entry = entry::read(payload).map(|e| Entry {
                    generation: e.generation,
                    count: e.count,
                    buffer: e.buffers[0].0.0,
                    ticket: e.buffers[0].1,
                    meta: e.meta.get(..META_BYTES).map(|m| Meta {
                        sequence: u64::from_le_bytes(m[0..8].try_into().unwrap_or([0; 8])),
                        identity: u64::from_le_bytes(m[8..16].try_into().unwrap_or([0; 8])),
                        bytes: u32::from_le_bytes(m[16..20].try_into().unwrap_or([0; 4])),
                        fence: u32::from_le_bytes(m[20..24].try_into().unwrap_or([0; 4])),
                    }),
                });
                samples.push((sample.seq, time, entry));
            }
            shm_ring::Next::Empty => std::thread::sleep(Duration::from_millis(1)),
            shm_ring::Next::Skipped(n) => {
                return Err(format!("the reader was overrun: {n} samples skipped"));
            }
        }
    }
    let finished = step("read the clock", monotonic_ns())?;

    report.check(
        samples.len() as u32 == BUFFERS,
        format_args!(
            "{} frames read through the shm crates (wanted {BUFFERS})",
            samples.len()
        ),
    );
    report.check(
        samples.iter().enumerate().all(|(i, s)| s.0 == i as u64),
        "the sample sequence numbers count up from zero",
    );
    report.check(
        samples.iter().all(|s| s.1 >= started && s.1 <= finished),
        "every sample time is between the check's start and its end (CLOCK_MONOTONIC)",
    );
    report.check(
        samples.windows(2).all(|w| w[0].1 <= w[1].1),
        "the sample times never go back",
    );

    let mut buffers_seen = [false; BUFFERS as usize];
    let mut entries_ok = true;
    let mut detail = String::new();
    let page_words = Words {
        map: &map,
        base: PAGE_AT,
    };
    for (seq, _, entry) in &samples {
        let ok = match entry {
            Some(Entry {
                generation,
                count,
                buffer,
                ticket,
                meta: Some(meta),
            }) => {
                let index = *buffer as usize;
                let fresh = index < BUFFERS as usize && !buffers_seen[index];
                if fresh {
                    buffers_seen[index] = true;
                }
                // The ticket the entry carries is the buffer's even ticket at publication, and the
                // page says the same: nothing returned yet, so zero.
                let page_ticket = page::ticket(&page_words, BufferIndex(*buffer));
                // The frame's sequence is the test source's count, from zero, and its identity 0.
                *generation == 1
                    && *count == 1
                    && fresh
                    && meta.sequence == *seq
                    && meta.identity == 0
                    && meta.bytes as usize <= BUFFER_BYTES as usize
                    && meta.fence != 0
                    && *ticket == 0
                    && page_ticket == 0
            }
            _ => false,
        };
        if !ok {
            entries_ok = false;
            detail = format!("sample {seq}: {entry:?}");
            break;
        }
    }
    report.check(
        entries_ok,
        format_args!(
            "every entry names a fresh buffer, generation 1, its frame's sequence, its ticket (even) {detail}"
        ),
    );

    let shape: Result<(u32, u32), String> = page::read(&page_words).map_err(|e| format!("{e:?}"));
    report.check(
        shape.as_ref().is_ok_and(|s| *s == (1, BUFFERS)),
        format_args!("the pool page's generation and buffers ({shape:?})"),
    );
    drop(reader);
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [node, block, heap] = args.as_slice() else {
        let _ = writeln!(
            std::io::stderr(),
            "usage: lemnos-shm-ring-check <control node> <block name> <heap node>"
        );
        return ExitCode::from(2);
    };
    let mut report = Report { failures: 0 };
    if let Err(error) = run(node, block, heap, &mut report) {
        report.check(false, error);
    }
    let _ = writeln!(
        std::io::stdout(),
        "lemnos-shm-ring-check: {} failed",
        report.failures
    );
    if report.failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use shm_ring::{Geometry, Spec};

    /// The shape the module writes for `FRAME_SLOTS` samples of 40-byte entries is the one this
    /// check reads: the derived size of the ring is the words it lays out.
    #[test]
    fn the_module_shape_is_the_derived_one() {
        let geometry = Geometry::for_spec(&Spec {
            fields: 1,
            history: super::FRAME_SLOTS,
            max_payload: super::META_BYTES as u32 + 16,
            expiry_ms: 0,
        })
        .unwrap();
        assert_eq!(geometry.bytes(), super::RING_WORDS * 8);
    }
}
