//! `lemnos-stream-compose-check`: the whole frame path in the QEMU harness, in one run.
//!
//! ```text
//! lemnos-stream-compose-check <lemnosd> <receiver node name> <heap node>
//! ```
//!
//! The fake camera receiver of the test module (`/dev/lemnos-test-csi2`) fires frame starts and
//! ends. Each frame goes through the module's stream to the `rp1-csi2` driver, which publishes it
//! on the host's endpoint, which is the shared-memory ring a client reads. The check starts the
//! production `lemnosd` with a board that has that receiver, takes its `frames` stream with
//! `lemnos-client` over the daemon's socket, and checks what arrives:
//!
//! - frames arrive, each of the layout's size, with sequence numbers that only go up;
//! - each frame's capture time is on the monotonic clock, between the start of the check and now,
//!   and never goes back;
//! - each frame's bytes are one value throughout (the fake block fills a frame with one pattern),
//!   and each frame carries the pattern its own sequence number names: the fake makes one pattern
//!   step per frame it produces, so `pattern - sequence` is the same for every delivered frame,
//!   even where frames were dropped (a gap in the sequence is allowed). That holds between
//!   recoveries of the receiver, and only there: a recovery restart carries the sequence on from
//!   the last frame delivered while the fake's pattern has stepped for every frame fired
//!   meanwhile (`docs/streams.md`, "Sequence across a stall"), so the frames are checked in runs,
//!   cut where the stream reported a recovery (`Next::Stalled`, the receiver's own health);
//! - while frames are taken the receiver recovers from nothing but a frame stop (its own
//!   judgement that no frame came within its bound, which here means the guest was paused): any
//!   other recovery is a fault of the frame path and fails the check with the receiver's reason;
//! - a frame the client holds keeps its bytes: two frames held while the fake fires on read back
//!   byte for byte what they were when they were taken, and so does a frame held through a stall
//!   and the receiver's recovery restart.
//!
//! It prints one line per check and exits nonzero on any failure. Each step's failure is printed
//! with its cause.

use std::fmt::Debug;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lemnos_client::{Client, FrameStream, HeldFrame, Next, ProducerState, Stall};
use lemnos_contract_camera_receiver::{CameraReceiver, LinkConfig, MAX_STREAM_NAME, Packing};
use lemnos_daemon_ready::Ready;
use lemnos_linux_stream::LinuxStreams;
use lemnos_linux_sys::test_csi2::{FE_ACK, FS, fire};
use lemnos_linux_sys::time::monotonic_ns;

/// The receiver's name in the board file.
const DEVICE: &str = "receiver";
/// The frames taken, at least.
const FRAMES: usize = 12;
/// How long the whole take may run.
const DEADLINE: Duration = Duration::from_secs(20);
/// The frames the hold phase keeps: two of the four buffers, so the receiver has two left to fill
/// and frames go on arriving through the hold. That is what the phase is for: frames written
/// beside the held ones, none into them. (A consumer may hold all four; that stops the producer
/// without a fault and nothing is written at all, which the driver's own tests cover.)
const HELD: usize = 2;
/// How long the held buffers stay held while the fake keeps firing frames.
const HOLD: Duration = Duration::from_millis(300);
/// How long the fake receiver stays silent in the stall phase: past the frame-stop bound, and the
/// one-second check that judges it, plus the first recovery delay.
const STALL: Duration = Duration::from_secs(3);
/// How long lemnosd has to say it is ready.
const READY_WAIT: Duration = Duration::from_secs(10);
/// The fake receiver's node, made by the test module.
const TEST_NODE: &str = "/dev/lemnos-test-csi2";
/// Packed 16-bit, 256 by 4: a 512-byte line and a 2048-byte frame.
const LINK: LinkConfig = LinkConfig {
    lanes: 2,
    virtual_channel: 0,
    data_type: 0x2b,
    bit_depth: 16,
    continuous_clock: false,
    width: 256,
    height: 4,
    pixel_rate_hz: 0,
    link_rate_mbps: 800,
};

/// How the receiver's reason begins while it recovers from a frame stop (`drivers/rp1-csi2`,
/// `Lifecycle::health`).
const FRAME_STOP: &str = "no frame arrived within the bound";

/// What the stream reported about its producer while the check read it: the receiver's own
/// health, as the host passes it on (`Next::Stalled` and `Next::Resumed`).
#[derive(Debug, Default)]
struct Watch {
    /// The run of frames being read: one more for each recovery reported. Frames of one run have
    /// no recovery between them.
    run: u32,
    /// The receiver's reason at each recovery, read when the stream reported it.
    reasons: Vec<String>,
    /// The times the stream reported the producer back.
    resumed: u32,
}

impl Watch {
    /// Whether every recovery reported was a frame stop.
    fn only_frame_stops(&self) -> bool {
        self.reasons
            .iter()
            .all(|reason| reason.starts_with(FRAME_STOP))
    }

    fn describe(&self) -> String {
        if self.reasons.is_empty() {
            "none reported".to_owned()
        } else {
            format!("{} reported: {:?}", self.reasons.len(), self.reasons)
        }
    }
}

/// Why the stream stalled: the reason of the stall the daemon kept with the start the stream
/// reported, on whichever device it was. The device may have recovered by the time the daemon is
/// asked, and its own reason is then empty: the kept stall is not.
fn reason(client: &Client, stalled: ProducerState) -> Result<String, String> {
    let stalls = step("last stalls", client.last_stalls())?;
    Ok(stall_reason(&stalls, stalled))
}

/// The reason of the stall in `stalls` that began when the stream says `stalled` did, or what was
/// found instead.
fn stall_reason(stalls: &[Stall], stalled: ProducerState) -> String {
    let ProducerState::Retrying { since_ns } = stalled else {
        return format!("the stream reported a stall as {stalled:?}");
    };
    stalls
        .iter()
        .find(|stall| stall.since_ns == since_ns)
        .map_or_else(
            || format!("no device kept a stall from {since_ns} ns (kept: {stalls:?})"),
            |stall| stall.reason.clone(),
        )
}

/// The next frame within `timeout`, or `None`. A recovery the stream reports on the way is noted
/// in `watch`: the frames after it are a new run.
fn next(
    stream: &mut FrameStream,
    client: &Client,
    watch: &mut Watch,
    timeout: Duration,
) -> Result<Option<HeldFrame>, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match step("next frame", stream.next_or_end(left))? {
            Next::Frame(frame) => return Ok(Some(frame)),
            Next::Stalled(stalled) => {
                watch.run += 1;
                watch.reasons.push(reason(client, stalled)?);
            }
            Next::Resumed { .. } => watch.resumed += 1,
            Next::TimedOut | Next::Ended(_) => return Ok(None),
            Next::AtLimit => return Err("the reader is at its hold limit".to_owned()),
            Next::JobEnded { .. } => {}
        }
    }
}

/// One frame as the client saw it.
#[derive(Debug, Clone, Copy)]
struct Taken {
    /// The run of frames it came in (`Watch::run`).
    run: u32,
    sequence: u64,
    timestamp_ns: u64,
    bytes: usize,
    first: u8,
    uniform: bool,
    /// When the client held the frame, CLOCK_MONOTONIC.
    seen_ns: u64,
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

/// Stops lemnosd (a child this check started) when the check ends, however it ends.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn step<T, E: Debug>(what: &str, result: Result<T, E>) -> Result<T, String> {
    result.map_err(|e| format!("{what}: {e:?}"))
}

/// The heap's name, from its node: `/dev/dma_heap/linux,cma` is `linux,cma`.
fn heap_name(node: &str) -> &str {
    node.strip_prefix("/dev/dma_heap/").unwrap_or(node)
}

fn board(heap: &str, receiver: &str) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"compose\"\n\n\
         [[devices]]\nid = \"{DEVICE}\"\ndriver = \"rp1-csi2\"\n\
         config = {{ device = \"{receiver}\" }}\n\n\
         [devices.streams.frames]\nbuffers = 4\nbuffer_bytes = 8192\n\
         memory = \"dma-buf\"\nheap = \"{heap}\"\n"
    )
}

/// Starts lemnosd on `board`, waits until it says it is ready (its socket listens), and returns it
/// with its socket's path.
fn start_daemon(lemnosd: &Path, dir: &Path, board: &str) -> Result<(Daemon, PathBuf), String> {
    let board_path = dir.join("board.toml");
    step("write the board", std::fs::write(&board_path, board))?;
    let socket = step("name the socket", lemnos_socket_dir::socket(dir, "sock"))?;
    let _ = std::fs::remove_file(&socket);
    let ready = Ready::new()?;
    // ROOT-STANZA: /dev/lemnos and the test module's receiver nodes stay root 0600 in the guest (no udev)
    // On a board 70-lemnos.rules gives them to the daemon's group; the unit's start of the daemon is
    // checked by unit-check, gadget-check, orion-link-check and route-a-check.
    let mut command = Command::new(lemnosd);
    command
        .arg("--board")
        .arg(&board_path)
        .arg("--socket")
        .arg(&socket);
    ready.give(&mut command);
    let mut daemon = Daemon(step("start lemnosd", command.spawn())?);
    ready.wait(&mut daemon.0, READY_WAIT)?;
    Ok((daemon, socket))
}

/// Fires frames at the fake receiver until told to stop. Returns the frames it fired.
fn fire_frames(stop: Arc<AtomicBool>) -> JoinHandle<Result<u64, String>> {
    std::thread::spawn(move || {
        let node = step(
            "open the fake receiver",
            OpenOptions::new().read(true).write(true).open(TEST_NODE),
        )?;
        let mut fired = 0u64;
        while !stop.load(Ordering::Acquire) {
            step("fire a frame start", fire(node.as_fd(), FS))?;
            step("fire a frame end", fire(node.as_fd(), FE_ACK))?;
            fired += 1;
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(fired)
    })
}

/// The next `wanted` frames of a stream the stall phase reads: each read and released, with its
/// sequence and length. The bytes are checked by the phases before the stall.
fn read_stalled(
    stream: &mut FrameStream,
    client: &Client,
    watch: &mut Watch,
    wanted: usize,
) -> Result<Vec<Taken>, String> {
    let mut taken = Vec::new();
    let deadline = Instant::now() + DEADLINE;
    while taken.len() < wanted && Instant::now() < deadline {
        let Some(frame) = next(stream, client, watch, Duration::from_millis(500))? else {
            continue;
        };
        let seen_ns = step("read the clock", monotonic_ns())?;
        let bytes = stream.bytes(&frame);
        let first = bytes.first().copied().unwrap_or(0);
        taken.push(Taken {
            run: watch.run,
            sequence: frame.sequence(),
            timestamp_ns: frame.timestamp_ns(),
            bytes: bytes.len(),
            first,
            uniform: bytes.iter().all(|b| *b == first),
            seen_ns,
        });
        step("release a frame", stream.release(frame))?;
    }
    Ok(taken)
}

/// A frame the client kept while the camera ran on: its sequence, its first byte and whether
/// every byte is that one value, read after the hold, and whether the bytes read after the hold
/// are the bytes copied out when the frame was taken.
#[derive(Debug, Clone, Copy)]
struct Held {
    /// The run of frames it was taken in (`Watch::run`).
    run: u32,
    sequence: u64,
    /// The first byte when the frame was taken.
    taken_first: u8,
    first: u8,
    uniform: bool,
    unchanged: bool,
}

/// The frames the client takes, in order, then the frames it holds (`HELD`, its grant) through a run
/// of frames while the fake keeps firing. Each held buffer must still carry its own frame's pattern
/// when it is read again: the module may not reuse a buffer its reader holds within the grant.
fn take(client: &Client, index: u32, name: &str) -> Result<(Vec<Taken>, Vec<Held>, Watch), String> {
    let mut stream = step(
        "open the frame stream",
        client.open_stream(index, name, Arc::new(LinuxStreams)),
    )?;
    let mut watch = Watch::default();
    let mut taken = Vec::new();
    let deadline = Instant::now() + DEADLINE;
    while taken.len() < FRAMES && Instant::now() < deadline {
        let Some(frame) = next(&mut stream, client, &mut watch, Duration::from_millis(500))? else {
            continue;
        };
        let seen_ns = step("read the clock", monotonic_ns())?;
        let bytes = stream.bytes(&frame);
        let first = bytes.first().copied().unwrap_or(0);
        taken.push(Taken {
            run: watch.run,
            sequence: frame.sequence(),
            timestamp_ns: frame.timestamp_ns(),
            bytes: bytes.len(),
            first,
            uniform: bytes.iter().all(|b| *b == first),
            seen_ns,
        });
        step("release a frame", stream.release(frame))?;
    }
    // Each kept frame with a copy of its bytes as they were when it was taken.
    let mut kept = Vec::new();
    let deadline = Instant::now() + DEADLINE;
    while kept.len() < HELD && Instant::now() < deadline {
        if let Some(frame) = next(&mut stream, client, &mut watch, Duration::from_millis(500))? {
            let copy = stream.bytes(&frame).to_vec();
            kept.push((frame, copy, watch.run));
        }
    }
    std::thread::sleep(HOLD);
    let mut held = Vec::new();
    for (frame, copy, run) in &kept {
        held.push(read_held(&stream, frame, copy, *run));
    }
    for (frame, _, _) in kept {
        step("release a held frame", stream.release(frame))?;
    }
    Ok((taken, held, watch))
}

/// Reads a held frame again, against the copy of its bytes made when it was taken.
fn read_held(stream: &FrameStream, frame: &HeldFrame, copy: &[u8], run: u32) -> Held {
    let bytes = stream.bytes(frame);
    let first = bytes.first().copied().unwrap_or(0);
    Held {
        run,
        sequence: frame.sequence(),
        taken_first: copy.first().copied().unwrap_or(0),
        first,
        uniform: bytes.iter().all(|b| *b == first),
        unchanged: bytes == copy,
    }
}

fn run(lemnosd: &Path, receiver: &str, heap: &str, report: &mut Report) -> Result<(), String> {
    let dir = step(
        "make the scratch directory",
        lemnos_socket_dir::scratch_dir(),
    )?;
    step("make the directory", std::fs::create_dir_all(&dir))?;
    let started = step("read the clock", monotonic_ns())?;
    let (_daemon, socket) = start_daemon(lemnosd, &dir, &board(heap, receiver))?;
    let client = step("connect", Client::connect(&socket, "compose-check"))?;
    let index = step("list", client.list())?
        .iter()
        .find(|d| d.name == DEVICE)
        .map(|d| d.index)
        .ok_or("the receiver is not on the daemon's list")?;
    let mut camera = step(
        "open the receiver",
        client.open::<dyn CameraReceiver>(DEVICE),
    )?;
    let layout = step("configure", camera.configure(LINK, Packing::Packed))?;
    report.check(
        layout.stride == 512 && layout.size == 2048,
        format_args!(
            "the receiver answers the layout ({}x{}, stride {}, size {})",
            layout.width, layout.height, layout.stride, layout.size
        ),
    );
    step("start", camera.start())?;
    let mut name = [0u8; MAX_STREAM_NAME];
    let len = step("frame_stream", camera.frame_stream(&mut name))?;
    let name = String::from_utf8_lossy(&name[..len]).into_owned();
    report.check(
        name == "frames",
        format_args!("the receiver names its stream \"{name}\""),
    );

    let stop = Arc::new(AtomicBool::new(false));
    let firing = fire_frames(stop.clone());
    let taken = take(&client, index, &name);
    stop.store(true, Ordering::Release);
    let fired = firing.join().map_err(|_| "the fire thread panicked")?;
    let (frames, held, taking) = taken?;
    let size = layout.size as usize;
    let fired = fired?;

    // The stall: the fake receiver fires nothing for STALL while the client holds one frame of the
    // four buffers, so the driver's frame-stop judgement restarts the receiver's module stream (a
    // real stall, not the consumer's doing: the driver has buffers to arm). Recovery is asserted by
    // the receiver's frame-start count beginning again, by the stream reporting it to the reader
    // (which reads on through the stall, as a consumer waiting for frames does), and the numbering
    // by the frames after the stall: each numbered above the last one read before. The frame held
    // through the stall, the restart and the frames after it must read back as it was taken.
    let stall_stop = Arc::new(AtomicBool::new(false));
    let firing = fire_frames(stall_stop.clone());
    let mut stalled = step(
        "open the frame stream for the stall",
        client.open_stream(index, &name, Arc::new(LinuxStreams)),
    )?;
    let mut stalling = Watch::default();
    let before = read_stalled(&mut stalled, &client, &mut stalling, 4)?;
    let mut through = None;
    let deadline = Instant::now() + DEADLINE;
    while through.is_none() && Instant::now() < deadline {
        if let Some(frame) = next(
            &mut stalled,
            &client,
            &mut stalling,
            Duration::from_millis(500),
        )? {
            let copy = stalled.bytes(&frame).to_vec();
            through = Some((frame, copy, stalling.run));
        }
    }
    stall_stop.store(true, Ordering::Release);
    firing.join().map_err(|_| "the fire thread panicked")??;
    // The fire thread has returned: every frame start it made is stamped at or before this
    // (the fake delivers each one to the receiver's handler inside its fire call).
    let stopped_ns = step("read the clock", monotonic_ns())?;
    let starts_before = step("frame_starts", camera.frame_starts())?.map(|s| s.count);
    let mut last_before = through
        .as_ref()
        .map(|(frame, _, _)| frame.sequence())
        .or(before.last().map(|f| f.sequence))
        .unwrap_or(0);
    // The reader waits through the stall: no frame comes, and the stream tells it why. A frame
    // that started before the fire thread stopped may still be on its way through the relay; it
    // is a leftover from before the silence, not a frame in it, so it is released and the reader
    // waits on until the silence has lasted STALL. A report that comes while leftovers are read
    // is counted: the snapshot is taken first.
    let before_stall = (stalling.reasons.len(), stalling.resumed);
    let silence_ends = Instant::now() + STALL;
    let mut left_over = 0usize;
    let silent = loop {
        let wait = silence_ends.saturating_duration_since(Instant::now());
        match next(&mut stalled, &client, &mut stalling, wait)? {
            Some(frame) if started_before_the_stop(frame.timestamp_ns(), stopped_ns) => {
                last_before = last_before.max(frame.sequence());
                left_over += 1;
                step(
                    "release a frame left over from before the stall",
                    stalled.release(frame),
                )?;
            }
            Some(frame) => {
                step("release the frame in the silence", stalled.release(frame))?;
                break true;
            }
            None => break false,
        }
    };
    let reported: Vec<String> = stalling.reasons[before_stall.0..].to_vec();
    let resumed = stalling.resumed - before_stall.1;
    let starts_after = step("frame_starts", camera.frame_starts())?.map(|s| s.count);
    let resume_stop = Arc::new(AtomicBool::new(false));
    let firing = fire_frames(resume_stop.clone());
    let after = read_stalled(&mut stalled, &client, &mut stalling, 4)?;
    resume_stop.store(true, Ordering::Release);
    firing.join().map_err(|_| "the fire thread panicked")??;
    let through = match through {
        Some((frame, copy, run)) => {
            let held = read_held(&stalled, &frame, &copy, run);
            step(
                "release the frame held through the stall",
                stalled.release(frame),
            )?;
            Some(held)
        }
        None => None,
    };
    drop(stalled);
    report.check(
        before.len() == 4 && after.len() == 4,
        format_args!(
            "frames came before the stall ({}) and after it ({})",
            before.len(),
            after.len()
        ),
    );
    report.check(
        matches!((starts_before, starts_after), (Some(b), Some(a)) if a < b),
        format_args!(
            "the stall restarted the receiver: its frame-start count began again ({starts_before:?} to {starts_after:?})"
        ),
    );
    report.check(
        !silent
            && reported.len() == 1
            && reported[0].starts_with(FRAME_STOP)
            && resumed == 1,
        format_args!(
            "the stream reported the stall to its reader, and the receiver back, before any frame after it \
             ({} frames in the silence, {left_over} left over from before it, {} stalls reported \
             {reported:?}, {resumed} resumed)",
            usize::from(silent),
            reported.len()
        ),
    );
    report.check(
        after.first().is_some_and(|f| f.sequence > last_before)
            && after.windows(2).all(|w| w[0].sequence < w[1].sequence),
        format_args!(
            "after the stall the numbering goes on above the last frame before it ({last_before}): {:?}",
            after.iter().map(|f| f.sequence).collect::<Vec<_>>()
        ),
    );
    report.check(
        through.is_some_and(|h| h.unchanged && h.uniform),
        format_args!(
            "a frame held through the stall and the restart reads back as it was taken ({})",
            match &through {
                Some(h) => format!(
                    "sequence {}, first byte {} when taken and {} after, uniform {}",
                    h.sequence, h.taken_first, h.first, h.uniform
                ),
                None => "no frame was held".to_owned(),
            }
        ),
    );
    step("stop", camera.stop())?;
    let finished = step("read the clock", monotonic_ns())?;

    report.check(
        frames.len() >= FRAMES,
        format_args!("{} frames reached the client (fired {fired})", frames.len()),
    );
    report.check(
        frames.iter().all(|f| f.bytes == size),
        format_args!("every frame is {size} bytes"),
    );
    let ordered = frames.windows(2).all(|w| w[0].sequence < w[1].sequence);
    if !ordered {
        // What the client read, in order: the sequence numbers the module and the driver gave.
        let seqs: Vec<u64> = frames.iter().map(|f| f.sequence).collect();
        let _ = writeln!(std::io::stdout(), "sequences read: {seqs:?}");
    }
    report.check(ordered, "the sequence numbers only go up");
    let stamped = frames
        .iter()
        .all(|f| f.timestamp_ns >= started && f.timestamp_ns <= finished);
    report.check(
        stamped,
        "every capture time is between the check's start and its end (CLOCK_MONOTONIC)",
    );
    report.check(
        frames
            .windows(2)
            .all(|w| w[0].timestamp_ns <= w[1].timestamp_ns),
        "the capture times never go back",
    );
    let mut latency: Vec<u64> = frames
        .iter()
        .map(|f| f.seen_ns.saturating_sub(f.timestamp_ns))
        .collect();
    latency.sort_unstable();
    if let (Some(&least), Some(&most)) = (latency.first(), latency.last()) {
        let median = latency[latency.len() / 2];
        let _ = writeln!(
            std::io::stdout(),
            "meas relay latency, frame start to client: median {:.1} us, min {:.1} us, max {:.1} us over {} frames",
            median as f64 / 1000.0,
            least as f64 / 1000.0,
            most as f64 / 1000.0,
            latency.len()
        );
        // A regression bound, not a board figure: a relay woken by the doorbell measures well under
        // a millisecond in QEMU; a median near the driver's 33 ms fallback means no wake came.
        report.check(
            median < 5_000_000,
            format_args!(
                "the relay's median latency is under 5 ms ({:.1} us; the fallback is 33 ms)",
                median as f64 / 1000.0
            ),
        );
    }
    report.check(
        frames.iter().all(|f| f.uniform),
        "every frame's bytes are one pattern value",
    );
    // What the receiver did while the frames were taken, by its own report. A frame stop is the
    // guest paused for longer than the bound (the fake fires from this process); anything else is
    // the frame path failing under a steady source.
    report.check(
        taking.only_frame_stops(),
        format_args!(
            "the receiver recovered from nothing but a frame stop while frames were taken ({})",
            taking.describe()
        ),
    );
    let follows = pattern_follows_sequence(
        &frames
            .iter()
            .map(|f| (f.run, f.sequence, f.first))
            .collect::<Vec<_>>(),
    );
    report.check(
        follows.is_ok(),
        format_args!(
            "each frame carries the pattern its sequence names ({})",
            match &follows {
                Ok(bases) => format!("{}, {} frames", describe_bases(bases), frames.len()),
                Err(why) => why.clone(),
            }
        ),
    );
    // The held buffers, read after the fake fired on through the hold: each still carries its
    // own frame's pattern, and the pattern follows the sequence on from the frames before it.
    report.check(
        held.len() == HELD && held.iter().all(|h| h.uniform),
        format_args!(
            "{} frames held through a run of frames each keep one pattern value",
            held.len()
        ),
    );
    // The hold itself: what the client reads after the hold is what it copied out at the take.
    report.check(
        held.len() == HELD && held.iter().all(|h| h.unchanged),
        format_args!(
            "the held frames read back byte for byte as they were taken ({})",
            held.iter()
                .map(|h| format!(
                    "sequence {}: {} then {}",
                    h.sequence, h.taken_first, h.first
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    let all: Vec<(u32, u64, u8)> = frames
        .iter()
        .map(|f| (f.run, f.sequence, f.first))
        .chain(held.iter().map(|h| (h.run, h.sequence, h.first)))
        .collect();
    let held_follow = pattern_follows_sequence(&all);
    if held_follow.is_err() {
        // What the client read for each held frame: its buffer, sequence, identity, length and first byte.
        for h in &held {
            let _ = writeln!(
                std::io::stdout(),
                "held: run {} sequence {} first {} (when taken {}) uniform {} unchanged {}",
                h.run,
                h.sequence,
                h.first,
                h.taken_first,
                h.uniform,
                h.unchanged
            );
        }
        for f in &frames {
            let _ = writeln!(
                std::io::stdout(),
                "taken: run {} sequence {} first {} bytes {}",
                f.run,
                f.sequence,
                f.first,
                f.bytes
            );
        }
    }
    report.check(
        held_follow.is_ok(),
        format_args!(
            "the held frames still carry the pattern their sequence names ({})",
            match &held_follow {
                Ok(bases) => format!("{}, {} frames held", describe_bases(bases), held.len()),
                Err(why) => why.clone(),
            }
        ),
    );
    drop(camera);
    drop(client);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [lemnosd, receiver, heap] = args.as_slice() else {
        let _ = writeln!(
            std::io::stderr(),
            "usage: lemnos-stream-compose-check <lemnosd> <receiver node name> <heap node>"
        );
        return ExitCode::from(2);
    };
    let mut report = Report { failures: 0 };
    let heap = heap_name(heap);
    if let Err(error) = run(Path::new(lemnosd), receiver, heap, &mut report) {
        report.check(false, error);
    }
    let _ = writeln!(
        std::io::stdout(),
        "lemnos-stream-compose-check: {} failed",
        report.failures
    );
    if report.failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Whether every frame carries the pattern its own sequence number names. Each frame is its run
/// (the recoveries the stream had reported when it was read), its sequence and its pattern. The
/// fake makes one step per frame produced, so within a run `pattern - sequence` (mod 256) is one
/// base for all its frames; a gap in the sequence is a frame the driver dropped and is allowed.
/// Across a reported recovery the base moves (the fake stepped for frames that were never
/// numbered), so each run has its own. Returns each run with its base, or the first frame that
/// breaks its run's. A run number that goes down, or an empty list, is refused: nothing sound was
/// checked.
fn pattern_follows_sequence(frames: &[(u32, u64, u8)]) -> Result<Vec<(u32, u8)>, String> {
    let mut bases: Vec<(u32, u8)> = Vec::new();
    for &(run, sequence, pattern) in frames {
        let base = match bases.last() {
            Some(&(last, base)) if last == run => base,
            Some(&(last, _)) if last > run => {
                return Err(format!(
                    "sequence {sequence} is of run {run}, read after run {last}"
                ));
            }
            _ => {
                let base = pattern.wrapping_sub(sequence as u8);
                bases.push((run, base));
                base
            }
        };
        let expected = base.wrapping_add(sequence as u8);
        if pattern != expected {
            return Err(format!(
                "sequence {sequence} carries pattern {pattern}, expected {expected} (base {base})"
            ));
        }
    }
    if bases.is_empty() {
        return Err("no frames to check".to_owned());
    }
    Ok(bases)
}

/// Whether a frame the reader gets after the fake stopped firing started before the stop: its
/// capture time (the frame start, CLOCK_MONOTONIC) is at or before the instant read once the fire
/// thread returned. Such a frame was still in the relay when the silence began; it is a leftover
/// from before the stall, not a frame in it. A frame stamped after the stop started in the
/// silence, which the stall phase must not see.
fn started_before_the_stop(timestamp_ns: u64, stopped_ns: u64) -> bool {
    timestamp_ns <= stopped_ns
}

/// The bases as the report prints them: one base, or one per run of frames.
fn describe_bases(bases: &[(u32, u8)]) -> String {
    match bases {
        [(_, base)] => format!("base {base}"),
        _ => format!("a base per run between recoveries {bases:?}"),
    }
}

/// The old rule, kept for the test that shows it passes a stale frame across a gap.
#[cfg(test)]
fn consecutive_pairs_step_by_one(frames: &[(u32, u64, u8)]) -> bool {
    let pairs: Vec<bool> = frames
        .windows(2)
        .filter(|w| w[1].1 == w[0].1 + 1)
        .map(|w| w[1].2 == w[0].2.wrapping_add(1))
        .collect();
    !pairs.is_empty() && pairs.iter().all(|ok| *ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(name: &str, since_ns: u64, ended_ns: Option<u64>, why: &str) -> Stall {
        Stall {
            index: 0,
            name: name.to_owned(),
            since_ns,
            ended_ns,
            reason: why.to_owned(),
        }
    }

    #[test]
    fn a_stall_that_ended_before_the_daemon_was_asked_still_gives_its_reason() {
        // T146: the receiver recovered before the list was read, so its own reason was empty.
        let stalls = [kept(
            DEVICE,
            40,
            Some(90),
            "no frame arrived within the bound: x",
        )];
        let stalled = ProducerState::Retrying { since_ns: 40 };
        assert_eq!(
            stall_reason(&stalls, stalled),
            "no frame arrived within the bound: x"
        );
    }

    #[test]
    fn a_stall_a_linked_device_caused_is_read_from_that_device() {
        let stalls = [
            kept("sensor", 7, Some(9), "the sensor stopped"),
            kept(DEVICE, 3, Some(5), "no frame arrived within the bound"),
        ];
        let reason = stall_reason(&stalls, ProducerState::Retrying { since_ns: 7 });
        assert_eq!(reason, "the sensor stopped");
    }

    #[test]
    fn a_stall_no_device_kept_is_not_taken_for_a_frame_stop() {
        // An older stall with the frame-stop reason must not stand in for this one.
        let stalls = [kept(
            DEVICE,
            3,
            Some(5),
            "no frame arrived within the bound",
        )];
        let reason = stall_reason(&stalls, ProducerState::Retrying { since_ns: 8 });
        assert!(!reason.starts_with(FRAME_STOP), "{reason}");
        let reason = stall_reason(&stalls, ProducerState::Running);
        assert!(!reason.starts_with(FRAME_STOP), "{reason}");
    }

    #[test]
    fn a_forced_drop_leaves_a_gap_and_every_frame_still_follows_its_sequence() {
        // Frames 0, 1, then frame 2 dropped by the driver (its sequence number is taken, its
        // pattern is advanced by the fake when it was produced), then 3 and 4.
        let frames = [(0, 0u64, 1u8), (0, 1, 2), (0, 3, 4), (0, 4, 5)];
        assert_eq!(pattern_follows_sequence(&frames), Ok(vec![(0, 1)]));
        // The old rule also passes this one: it only looks across consecutive sequence numbers.
        assert!(consecutive_pairs_step_by_one(&frames));
    }

    #[test]
    fn a_stale_frame_across_a_gap_is_caught_by_the_sequence_rule_and_not_the_old_one() {
        // Frame 3 was delivered with frame 1's pattern: its fill is missing. Its pair with frame 1
        // is across a gap, which the old rule never looked at, so the old rule passes it wrongly.
        let frames = [(0, 0u64, 1u8), (0, 1, 2), (0, 3, 2)];
        assert!(
            consecutive_pairs_step_by_one(&frames),
            "old rule: passes it wrongly"
        );
        let why = pattern_follows_sequence(&frames).unwrap_err();
        assert!(why.contains("sequence 3 carries pattern 2"), "{why}");
    }

    #[test]
    fn a_stale_frame_next_to_its_neighbour_is_caught_by_both_rules() {
        let frames = [(0, 0u64, 1u8), (0, 1, 2), (0, 2, 2)];
        assert!(!consecutive_pairs_step_by_one(&frames));
        assert!(pattern_follows_sequence(&frames).is_err());
    }

    #[test]
    fn a_forced_drop_on_the_old_fake_fails_the_new_rule_and_passes_the_old_one_wrongly() {
        // The old fake stepped the pattern only when a buffer was latched: frame 2 was dropped with
        // no step, so frame 3 carries 3. The new rule catches it; the old rule passes it, because
        // the pair is across the gap.
        let frames = [(0, 0u64, 1u8), (0, 1, 2), (0, 3, 3)];
        assert!(
            consecutive_pairs_step_by_one(&frames),
            "old rule: passes wrongly"
        );
        assert!(
            pattern_follows_sequence(&frames).is_err(),
            "new rule: catches the old fake"
        );
    }

    #[test]
    fn the_base_may_be_any_value_and_wraps_at_256() {
        let frames = [(0, 255u64, 7u8), (0, 256, 8), (0, 257, 9)];
        assert_eq!(pattern_follows_sequence(&frames), Ok(vec![(0, 8)]));
    }

    /// The lander's failure (train 115), as the check read it: sequences 0 to 9 on base 20, then
    /// two recovery restarts, each carrying the sequence on while the fake fired about 95 frames.
    /// With the recoveries reported, each run is checked on its own base. Without them (the frames
    /// all in one run, as the check read them before it listened to the stream) it is the failure
    /// the lander printed: a base that moves with no recovery reported is a wrong frame.
    #[test]
    fn a_reported_recovery_starts_a_new_run_and_an_unreported_jump_is_a_wrong_frame() {
        let reported = [
            (0, 8u64, 28u8),
            (0, 9, 29),
            (1, 10, 125),
            (2, 11, 224),
            (2, 12, 225),
        ];
        assert_eq!(
            pattern_follows_sequence(&reported),
            Ok(vec![(0, 20), (1, 115), (2, 213)])
        );
        let unreported = reported.map(|(_, sequence, pattern)| (0, sequence, pattern));
        let why = pattern_follows_sequence(&unreported).unwrap_err();
        assert_eq!(
            why,
            "sequence 10 carries pattern 125, expected 30 (base 20)"
        );
    }

    #[test]
    fn a_wrong_frame_inside_a_run_after_a_recovery_is_still_caught() {
        // Run 1 begins at sequence 10 on a new base; sequence 12 carries sequence 11's bytes.
        let frames = [(0, 9u64, 29u8), (1, 10, 125), (1, 11, 126), (1, 12, 126)];
        let why = pattern_follows_sequence(&frames).unwrap_err();
        assert!(why.contains("sequence 12 carries pattern 126"), "{why}");
    }

    #[test]
    fn a_run_read_out_of_order_is_refused() {
        let frames = [(1, 10u64, 125u8), (0, 9, 29)];
        assert!(pattern_follows_sequence(&frames).is_err());
    }

    #[test]
    fn only_a_frame_stop_is_a_recovery_the_take_allows() {
        let mut watch = Watch::default();
        assert!(watch.only_frame_stops(), "no recovery at all");
        watch.reasons.push(
            "no frame arrived within the bound for the configured mode while buffers were armed: \
             restarting the receiver"
                .to_owned(),
        );
        assert!(watch.only_frame_stops());
        watch
            .reasons
            .push("the receiver stopped while streaming: restarting it".to_owned());
        assert!(!watch.only_frame_stops(), "the frame path failed");
    }

    #[test]
    fn a_frame_started_at_or_before_the_stop_is_a_leftover_and_one_after_it_is_in_the_silence() {
        let stopped = 5_000_000_000;
        // Fired before the stop and relayed late: a leftover, however late it is read.
        assert!(started_before_the_stop(stopped - 1_200_000, stopped));
        // Stamped at the very instant the clock was read after the join: still before the silence.
        assert!(started_before_the_stop(stopped, stopped));
        // Started after the stop: the receiver produced a frame in the silence.
        assert!(!started_before_the_stop(stopped + 1, stopped));
    }

    #[test]
    fn no_frames_is_refused_not_passed() {
        assert!(pattern_follows_sequence(&[]).is_err());
    }
}
