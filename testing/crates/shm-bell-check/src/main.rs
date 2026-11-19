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
//!    the bell's median must beat the poll's, and no frame may wait out the timeout.
//! 3. Losses: every frame is either read or counted as skipped, the reader's count of what it
//!    skipped is exactly the frames missing from the sequence it read, and a skip comes only when
//!    the ring had run more than its window past the reader (docs/streams.md, "Waiting on the
//!    doorbell"; shm/README.md, "Reading"). A reader that is not run for longer than the window
//!    (16 frames, 32 ms here) loses frames, counted: the guest's vCPUs are host threads, and on a
//!    loaded host one can be off for that long. So the check holds the ring to its count, not the
//!    guest to a schedule. The reader's time on the guest's run queue and the guest's steal time
//!    are printed, to tell a guest that did not run the reader from a host that did not run the
//!    guest.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsFd;
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lemnos_linux_sys::dma_heap;
use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::module;
use lemnos_linux_sys::stream;
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
                flags: 0,
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

/// What a reader saw of a stream, kept two ways: what the reader said it skipped, and what is
/// missing from the sequence numbers of the samples it read.
#[derive(Default)]
struct Tally {
    /// Each read sample's latency, from its publish time to the read, in nanoseconds.
    latencies: Vec<u64>,
    /// The sequence the reader wants next: one past the last it read.
    consumed: u64,
    /// The sum of the reader's `Skipped(n)` reports.
    reported: u64,
    /// Sequence numbers passed over between samples read.
    missed: u64,
    /// Samples read out of order (a sequence number at or below one already read).
    backwards: u64,
    /// Skips reported while the ring had not run the reader's window past it.
    early: u64,
}

impl Tally {
    /// One `next` result. `window` is the ring's, `head` the ring's head word read after the call.
    fn take(&mut self, next: Next<'_>, window: u64, head: impl FnOnce() -> u64) -> bool {
        match next {
            Next::Sample(sample) => {
                self.sample(sample.seq);
                self.latencies
                    .push(SampleTime::now().saturating_since(sample.time));
                true
            }
            Next::Skipped(n) => {
                self.skipped(n, window, head());
                true
            }
            Next::Empty => false,
        }
    }

    fn sample(&mut self, seq: u64) {
        if seq < self.consumed {
            self.backwards += 1;
            return;
        }
        self.missed += seq - self.consumed;
        self.consumed = seq + 1;
    }

    /// A reported skip of `n` from `consumed`, with the ring's head at `head` after the report.
    /// The reader skips past the window (head - consumed > window, so head >= consumed + window +
    /// n), or past slots the writer has overwritten, the newest of them perhaps still being
    /// written (head >= consumed + window + n - 1). Anything less lost a sample the ring still had.
    fn skipped(&mut self, n: u64, window: u64, head: u64) {
        self.reported += n;
        if head + 1 < self.consumed + window + n {
            self.early += 1;
        }
    }

    /// Each of `frames` read or missing from the sequence, once and in order.
    fn whole(&self, frames: u64) -> bool {
        self.consumed == frames
            && self.latencies.len() as u64 + self.missed == frames
            && self.backwards == 0
    }

    /// The reader's own count of its losses is the frames missing, and each skip was one the ring
    /// forced.
    const fn honest(&self) -> bool {
        self.reported == self.missed && self.early == 0
    }
}

/// Reads every sample the reader has, from `tally.consumed` on.
fn drain(reader: &mut Reader<Words<'_>>, fx: &Fixture, tally: &mut Tally) {
    let window = reader.geometry().window();
    while tally.take(reader.next(), window, || head(fx)) {}
}

/// Checks a run's tally against the `FRAMES` the source published.
fn account(name: &str, tally: &Tally, report: &mut Report) {
    let read = tally.latencies.len();
    report.check(
        tally.whole(FRAMES),
        format_args!(
            "{name}: every frame read or skipped ({read} read, {} missing from the sequence, \
             {} out of order, of {FRAMES})",
            tally.missed, tally.backwards
        ),
    );
    report.check(
        tally.honest(),
        format_args!(
            "{name}: the reader counted each frame it lost, and lost none the ring still held \
             ({} reported skipped, {} missing, {} skip(s) inside the window)",
            tally.reported, tally.missed, tally.early
        ),
    );
}

/// This process's time on the run queue, runnable and not run, in nanoseconds
/// (`/proc/self/schedstat`, its second number). 0 where the kernel doesn't keep it.
fn run_delay() -> u64 {
    std::fs::read_to_string("/proc/self/schedstat")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse().ok())
        .unwrap_or(0)
}

/// The guest's steal time over all its CPUs, in clock ticks (`/proc/stat`, the eighth number of
/// the `cpu` line): time a vCPU wanted to run and the host ran something else.
fn steal_ticks() -> u64 {
    std::fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|s| s.lines().next()?.split_whitespace().nth(8)?.parse().ok())
        .unwrap_or(0)
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
    let started = Instant::now();
    let wake = step(
        "the first wait",
        doorbell.wait(Some(WAIT), || head(&fx) > 0),
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

    let mut tally = Tally::default();
    drain(&mut reader, &fx, &mut tally);
    let consumed = tally.consumed;
    // The source goes on publishing, one frame every 20 ms, while this process attaches and
    // reads, and it may not be run for several of those periods. So the count read is bounded by
    // what the ring says, not by a guess at how long the attach took: every frame published
    // before the wait was read, and nothing was read that the ring had not published by the end.
    let published_after = head(&fx);
    report.check(
        !tally.latencies.is_empty() && first_wait_read_all(published, consumed, published_after),
        format_args!(
            "the first wait read {consumed} sample(s): the {published} published before it, \
             and no more than the {published_after} published by the end of the read"
        ),
    );
    // The first wait reads from the start: its earliest frame is the point (20 ms a frame, with a
    // 16-frame window). A skip there is the earliest frame lost, whatever the count.
    report.check(
        tally.reported == 0 && tally.missed == 0,
        format_args!(
            "first wait: no samples skipped ({} reported, {} missing)",
            tally.reported, tally.missed
        ),
    );
    Ok(())
}

/// Whether the first read took what it should: every sample published before the wait
/// (`before`), and no sample the ring had not published when the read ended (`after`).
fn first_wait_read_all(before: u64, consumed: u64, after: u64) -> bool {
    before >= 1 && consumed >= before && consumed <= after
}

/// What the bell run saw: its tally, the waits that timed out, and what the guest did meanwhile.
struct BellRun {
    tally: Tally,
    timeouts: u32,
    /// The reader's time on the guest's run queue, nanoseconds.
    run_delay: u64,
    /// The guest's steal time, clock ticks.
    steal: u64,
}

/// Reads every frame of the stream as it arrives, sleeping on the doorbell between them.
fn bell_run(fx: &Fixture) -> Result<BellRun, String> {
    let mut reader = step(
        "attach to the ring",
        Reader::over_from_start(words(&fx.ring)),
    )?;
    let mut doorbell = attach_bell(fx)?;
    let mut tally = Tally {
        latencies: Vec::with_capacity(FRAMES as usize),
        ..Tally::default()
    };
    let mut timeouts = 0_u32;
    let (delay_before, steal_before) = (run_delay(), steal_ticks());
    let deadline = Instant::now() + WAIT;
    while tally.consumed < FRAMES && Instant::now() < deadline {
        let consumed = tally.consumed;
        let wake = step(
            "the wait",
            doorbell.wait(Some(TIMEOUT), || head(fx) > consumed),
        )?;
        if wake == Wake::TimedOut {
            timeouts += 1;
        }
        drain(&mut reader, fx, &mut tally);
    }
    Ok(BellRun {
        tally,
        timeouts,
        run_delay: run_delay().saturating_sub(delay_before),
        steal: steal_ticks().saturating_sub(steal_before),
    })
}

/// Reads the same stream with a 1 ms poll, for comparison.
fn poll_run(fx: &Fixture) -> Result<Tally, String> {
    let mut reader = step(
        "attach to the ring",
        Reader::over_from_start(words(&fx.ring)),
    )?;
    let mut tally = Tally {
        latencies: Vec::with_capacity(FRAMES as usize),
        ..Tally::default()
    };
    let deadline = Instant::now() + WAIT;
    while tally.consumed < FRAMES && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
        drain(&mut reader, fx, &mut tally);
    }
    Ok(tally)
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

    let (bell_p50, bell_line) = describe("bell", &bell.tally.latencies);
    let (poll_p50, poll_line) = describe("poll (1 ms)", &poll.latencies);
    let _ = writeln!(std::io::stdout(), "{bell_line}");
    let _ = writeln!(std::io::stdout(), "{poll_line}");
    let _ = writeln!(
        std::io::stdout(),
        "bell: waits that timed out {}, samples skipped {}; poll: samples skipped {}",
        bell.timeouts,
        bell.tally.reported,
        poll.reported
    );
    let _ = writeln!(
        std::io::stdout(),
        "bell: the reader was runnable and not run for {} us in the guest; the guest's vCPUs were \
         not run by the host for {} tick(s) (steal)",
        bell.run_delay / 1000,
        bell.steal
    );

    account("bell", &bell.tally, report);
    account("poll (1 ms)", &poll, report);
    report.check(
        bell.timeouts == 0,
        format_args!(
            "bell: no wait timed out ({} did: a lost bell)",
            bell.timeouts
        ),
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

#[cfg(test)]
mod tests {
    use super::{Tally, first_wait_read_all};

    const WINDOW: u64 = 16;

    /// A run of `frames` in which the reader reads `0..at`, is not run while the ring goes `late`
    /// past it, is told it skipped `told`, and reads the rest.
    fn late_reader(frames: u64, at: u64, late: u64, told: u64) -> Tally {
        let mut t = Tally::default();
        for seq in 0..at {
            t.sample(seq);
            t.latencies.push(0);
        }
        let lost = late.saturating_sub(WINDOW);
        if told > 0 {
            t.skipped(told, WINDOW, at + late);
        }
        for seq in at + lost..frames {
            t.sample(seq);
            t.latencies.push(0);
        }
        t
    }

    #[test]
    fn a_reader_that_keeps_up_reads_every_frame() {
        let t = late_reader(2000, 1000, 10, 0);
        assert!(t.whole(2000) && t.honest());
        assert_eq!((t.latencies.len(), t.missed), (2000, 0));
    }

    #[test]
    fn a_reader_not_run_past_the_window_loses_frames_and_counts_them() {
        // The red this replaces: two frames lost, the reader said 2, the old count said 3.
        let t = late_reader(2000, 1000, WINDOW + 2, 2);
        assert!(t.whole(2000) && t.honest());
        assert_eq!((t.latencies.len(), t.missed, t.reported), (1998, 2, 2));
    }

    #[test]
    fn a_frame_lost_and_not_counted_fails() {
        let t = late_reader(2000, 1000, WINDOW + 2, 1);
        assert!(t.whole(2000) && !t.honest());
        let t = late_reader(2000, 1000, WINDOW + 2, 0);
        assert!(t.whole(2000) && !t.honest());
    }

    #[test]
    fn a_skip_the_ring_did_not_force_fails() {
        // Told of a skip while the ring was only 3 ahead: a frame the ring still held was lost.
        let mut t = Tally::default();
        t.sample(0);
        t.skipped(1, WINDOW, 4);
        t.sample(2);
        assert!(!t.honest());
        // The slot overwritten by a publish still in progress: the head is one short of it.
        let mut t = Tally::default();
        t.sample(0);
        t.skipped(1, WINDOW, 1 + WINDOW);
        t.sample(2);
        assert!(t.honest());
    }

    #[test]
    fn a_short_run_or_one_read_twice_is_not_whole() {
        let t = late_reader(2000, 1000, 0, 0);
        assert!(!t.whole(2001));
        let mut t = late_reader(10, 5, 0, 0);
        t.sample(3);
        assert!(!t.whole(10));
    }

    #[test]
    fn frames_published_while_the_reader_attached_are_read_too() {
        // As a loaded harness run showed it: 4 published before the wait, and the process was not
        // run for two more frame periods before it read, so it read 6. The old rule allowed one.
        assert!(first_wait_read_all(4, 6, 6));
        assert!(first_wait_read_all(4, 4, 4));
        assert!(
            first_wait_read_all(4, 5, 7),
            "one published during the read itself"
        );
    }

    #[test]
    fn a_read_that_missed_a_published_frame_or_read_one_never_published_fails() {
        assert!(
            !first_wait_read_all(4, 3, 6),
            "a frame published before the wait was not read"
        );
        assert!(
            !first_wait_read_all(4, 7, 6),
            "more read than the ring published"
        );
        assert!(
            !first_wait_read_all(0, 0, 0),
            "nothing was published before the wait"
        );
    }
}
