//! `lemnos-shm-bell-check`: the module rings a consumer's doorbell after each publish, so a
//! consumer that sleeps on it reads each frame as it arrives.
//!
//! ```text
//! lemnos-shm-bell-check <control node> <block name> <heap node>
//! ```
//!
//! 1. First wait: frames are published before the consumer has ever waited, and its first wait
//!    still returns at once, with the earliest of them. It doesn't sleep for the next frame.
//! 2. Latency: the consumer sleeps on its doorbell and reads every frame as it arrives. A frame's
//!    latency is from the module's publish time (CLOCK_MONOTONIC) to the read. The same stream is
//!    then read by a 1 ms poll, for comparison. QEMU numbers are for the regression check only:
//!    the bell's median must beat the poll's, no frame may wait out the timeout, and none may be
//!    skipped.

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
use shm_ring::{Mem, Next, Reader, SampleTime};
use shm_ring_doorbell::{Doorbell, Wake};

const BUFFERS: u32 = 4;
const BUFFER_BYTES: u32 = 4096;
/// Each buffer is this many bytes in the pool: the buffer size, aligned as the module aligns it.
const STRIDE: usize = 4096;
const FRAME_SLOTS: u32 = 16;
const JOB_SLOTS: u32 = 4;
/// `sizeof(struct lemnos_stream_job)`.
const JOB_BYTES: u32 = 80;
/// The ring's head word: the number of samples published (`shm/README.md`, word 8).
const HEAD_WORD: usize = 8;
/// Frames in the latency runs.
const FRAMES: u64 = 2000;
/// The frame period of the latency runs, in microseconds.
const PERIOD_US: u32 = 2000;
/// The longest a wait sleeps before it looks again. A frame that waits this long was a lost bell.
const TIMEOUT: Duration = Duration::from_millis(100);
/// The most a phase may take.
const WAIT: Duration = Duration::from_secs(30);

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

fn words(map: &Mapping) -> Words<'_> {
    Words { map, base: 0 }
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

/// A stream on the test block with its ring mapped. Its source stops when it drops.
struct Fixture {
    stream: stream::Stream,
    ring: Mapping,
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
    // The check reads only the ring: the buffers are the module's to write.
    drop(pool_fd);
    Ok(Fixture { stream, ring })
}

/// Samples published so far.
fn head(fx: &Fixture) -> u64 {
    Mem::load(&words(&fx.ring), HEAD_WORD, Ordering::Acquire)
}

/// Makes the consumer's doorbell and gives its eventfd and page to the module.
fn attach_bell(fx: &Fixture) -> Result<Doorbell, String> {
    let doorbell = step("make the doorbell", Doorbell::create())?;
    let (eventfd, page) = step("the module's doorbell fds", doorbell.bell_fds())?;
    step(
        "give the module the doorbell",
        stream::set_bell(fx.stream.fd.as_fd(), eventfd.as_fd(), page.as_fd()),
    )?;
    Ok(doorbell)
}

/// Reads every sample the reader has, from `consumed` on. A sample's latency is from its publish
/// time to now, in nanoseconds; a gap in the sequence is a skip.
fn drain(
    reader: &mut Reader<Words<'_>>,
    consumed: &mut u64,
    latencies: &mut Vec<u64>,
    skipped: &mut u64,
) {
    loop {
        match reader.next() {
            Next::Sample(sample) => {
                if sample.seq > *consumed {
                    *skipped += sample.seq - *consumed;
                }
                *consumed = sample.seq + 1;
                latencies.push(SampleTime::now().saturating_since(sample.time));
            }
            Next::Skipped(_) => *skipped += 1,
            Next::Empty => return,
        }
    }
}

/// The first wait with frames already published.
fn first_wait(node: &str, block: &str, heap: &str, report: &mut Report) -> Result<(), String> {
    let fx = fixture(node, block, heap)?;
    step(
        "start the test source",
        stream::test_source(fx.stream.fd.as_fd(), 20_000, 40),
    )?;
    // About three frames, published while nothing waits and nothing is attached.
    std::thread::sleep(Duration::from_millis(70));
    let published = head(&fx);
    report.check(
        published >= 2,
        format_args!("{published} frames published before the consumer waits"),
    );

    let mut reader = step(
        "attach to the ring",
        Reader::over_from_start(words(&fx.ring)),
    )?;
    let mut doorbell = attach_bell(&fx)?;
    let mut consumed = 0_u64;
    let started = Instant::now();
    let wake = step(
        "the first wait",
        doorbell.wait(Some(WAIT), || head(&fx) > consumed),
    )?;
    let waited = started.elapsed();
    report.check(
        wake == Wake::Ready,
        format_args!("first wait: {wake:?} (something to read, so no sleep)"),
    );
    report.check(
        waited < Duration::from_millis(5),
        format_args!("first wait returned in {waited:?}, not a frame period later"),
    );

    let mut latencies = Vec::new();
    let mut skipped = 0_u64;
    drain(&mut reader, &mut consumed, &mut latencies, &mut skipped);
    report.check(
        !latencies.is_empty() && consumed >= 1 && consumed <= published + 1,
        format_args!("the first wait read {consumed} sample(s) published before it"),
    );
    report.check(
        skipped == 0,
        format_args!("first wait: no samples skipped ({skipped})"),
    );
    Ok(())
}

/// Reads every frame of the stream as it arrives, sleeping on the doorbell between them.
fn bell_run(fx: &Fixture) -> Result<(Vec<u64>, u32, u64), String> {
    let mut reader = step(
        "attach to the ring",
        Reader::over_from_start(words(&fx.ring)),
    )?;
    let mut doorbell = attach_bell(fx)?;
    let mut latencies = Vec::with_capacity(FRAMES as usize);
    let mut consumed = 0_u64;
    let mut skipped = 0_u64;
    let mut timeouts = 0_u32;
    let deadline = Instant::now() + WAIT;
    while consumed < FRAMES && Instant::now() < deadline {
        let wake = step(
            "the wait",
            doorbell.wait(Some(TIMEOUT), || head(fx) > consumed),
        )?;
        if wake == Wake::TimedOut {
            timeouts += 1;
        }
        drain(&mut reader, &mut consumed, &mut latencies, &mut skipped);
    }
    Ok((latencies, timeouts, skipped))
}

/// Reads the same stream with a 1 ms poll, for comparison.
fn poll_run(fx: &Fixture) -> Result<(Vec<u64>, u64), String> {
    let mut reader = step(
        "attach to the ring",
        Reader::over_from_start(words(&fx.ring)),
    )?;
    let mut latencies = Vec::with_capacity(FRAMES as usize);
    let mut consumed = 0_u64;
    let mut skipped = 0_u64;
    let deadline = Instant::now() + WAIT;
    while consumed < FRAMES && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
        drain(&mut reader, &mut consumed, &mut latencies, &mut skipped);
    }
    Ok((latencies, skipped))
}

/// The value at quantile `q` (0 to 1) of sorted `values`; 0 for none.
fn quantile(sorted: &[u64], q: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let at = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[at.min(sorted.len() - 1)]
}

/// One line of a run's latency distribution, in microseconds.
fn describe(name: &str, latencies: &[u64]) -> (u64, String) {
    let mut sorted = latencies.to_vec();
    sorted.sort_unstable();
    let p50 = quantile(&sorted, 0.5);
    let line = format!(
        "{name}: frames {} min {} us, median {} us, p90 {} us, p99 {} us, max {} us",
        sorted.len(),
        quantile(&sorted, 0.0) / 1000,
        p50 / 1000,
        quantile(&sorted, 0.9) / 1000,
        quantile(&sorted, 0.99) / 1000,
        quantile(&sorted, 1.0) / 1000,
    );
    (p50, line)
}

/// The latency runs: the bell against the poll, on two streams of the same shape.
fn latency(node: &str, block: &str, heap: &str, report: &mut Report) -> Result<(), String> {
    let bell = {
        let fx = fixture(node, block, heap)?;
        step(
            "start the test source",
            stream::test_source(fx.stream.fd.as_fd(), PERIOD_US, FRAMES as u32),
        )?;
        bell_run(&fx)?
    };
    let poll = {
        let fx = fixture(node, block, heap)?;
        step(
            "start the test source",
            stream::test_source(fx.stream.fd.as_fd(), PERIOD_US, FRAMES as u32),
        )?;
        poll_run(&fx)?
    };

    let (bell_p50, bell_line) = describe("bell", &bell.0);
    let (poll_p50, poll_line) = describe("poll (1 ms)", &poll.0);
    let _ = writeln!(std::io::stdout(), "{bell_line}");
    let _ = writeln!(std::io::stdout(), "{poll_line}");
    let _ = writeln!(
        std::io::stdout(),
        "bell: waits that timed out {}, samples skipped {}; poll: samples skipped {}",
        bell.1,
        bell.2,
        poll.1
    );

    report.check(
        bell.0.len() as u64 == FRAMES,
        format_args!("bell: every frame read ({} of {FRAMES})", bell.0.len()),
    );
    report.check(
        bell.1 == 0,
        format_args!("bell: no wait timed out ({} did: a lost bell)", bell.1),
    );
    report.check(
        bell.2 == 0,
        format_args!("bell: no frame skipped ({})", bell.2),
    );
    report.check(
        bell_p50 < poll_p50,
        format_args!("bell: median below the 1 ms poll's ({bell_p50} ns against {poll_p50} ns)"),
    );
    Ok(())
}

fn run(node: &str, block: &str, heap: &str, report: &mut Report) -> Result<(), String> {
    first_wait(node, block, heap, report)?;
    latency(node, block, heap, report)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [node, block, heap] = args.as_slice() else {
        let _ = writeln!(
            std::io::stderr(),
            "usage: lemnos-shm-bell-check <control node> <block name> <heap node>"
        );
        return ExitCode::from(2);
    };
    let mut report = Report { failures: 0 };
    if let Err(error) = run(node, block, heap, &mut report) {
        report.check(false, error);
    }
    let _ = writeln!(
        std::io::stdout(),
        "lemnos-shm-bell-check: {} failed",
        report.failures
    );
    if report.failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
