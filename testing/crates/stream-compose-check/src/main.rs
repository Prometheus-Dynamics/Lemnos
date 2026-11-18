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
//!   even where frames were dropped (a gap in the sequence is allowed).
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

use lemnos_client::Client;
use lemnos_contract_camera_receiver::{CameraReceiver, LinkConfig, MAX_STREAM_NAME, Packing};
use lemnos_linux_stream::LinuxStreams;
use lemnos_linux_sys::test_csi2::{FE_ACK, FS, fire};
use lemnos_linux_sys::time::monotonic_ns;

/// The receiver's name in the board file.
const DEVICE: &str = "receiver";
/// The frames taken, at least.
const FRAMES: usize = 12;
/// How long the whole take may run.
const DEADLINE: Duration = Duration::from_secs(20);
/// The stream's buffers (`buffers` in the board): all of them are held in the hold phase.
const BUFFERS: usize = 4;
/// How long the held buffers stay held while the fake keeps firing frames.
const HOLD: Duration = Duration::from_millis(300);
/// How long lemnosd has to make its socket.
const SOCKET_WAIT: Duration = Duration::from_secs(10);
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

/// One frame as the client saw it.
#[derive(Debug, Clone, Copy)]
struct Taken {
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

/// Starts lemnosd on `board`, waits for its socket, and returns it with its path.
fn start_daemon(lemnosd: &Path, dir: &Path, board: &str) -> Result<(Daemon, PathBuf), String> {
    let board_path = dir.join("board.toml");
    step("write the board", std::fs::write(&board_path, board))?;
    let socket = dir.join("sock");
    let _ = std::fs::remove_file(&socket);
    let child = step(
        "start lemnosd",
        Command::new(lemnosd)
            .arg("--board")
            .arg(&board_path)
            .arg("--socket")
            .arg(&socket)
            .spawn(),
    )?;
    let mut daemon = Daemon(child);
    let started = Instant::now();
    while !socket.exists() {
        if let Some(status) = step("poll lemnosd", daemon.0.try_wait())? {
            return Err(format!("lemnosd exited before its socket: {status}"));
        }
        if started.elapsed() > SOCKET_WAIT {
            return Err("lemnosd made no socket".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
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

/// A frame the client kept while the camera ran on: its sequence, its first byte, and whether
/// every byte is that one value, all read after the hold.
#[derive(Debug, Clone, Copy)]
struct Held {
    sequence: u64,
    first: u8,
    uniform: bool,
}

/// The frames the client takes, in order, then the frames it holds through a run of frames: every
/// buffer is kept while the fake keeps firing, so the driver finds none free at a frame end. Each
/// held buffer must still carry its own frame's pattern when it is read again.
fn take(client: &Client, index: u32, name: &str) -> Result<(Vec<Taken>, Vec<Held>), String> {
    let mut stream = step(
        "open the frame stream",
        client.open_stream(index, name, Arc::new(LinuxStreams)),
    )?;
    let mut taken = Vec::new();
    let deadline = Instant::now() + DEADLINE;
    while taken.len() < FRAMES && Instant::now() < deadline {
        let Some(frame) = step("next frame", stream.next_frame(Duration::from_millis(500)))? else {
            continue;
        };
        let seen_ns = step("read the clock", monotonic_ns())?;
        let bytes = stream.bytes(&frame);
        let first = bytes.first().copied().unwrap_or(0);
        taken.push(Taken {
            sequence: frame.sequence(),
            timestamp_ns: frame.timestamp_ns(),
            bytes: bytes.len(),
            first,
            uniform: bytes.iter().all(|b| *b == first),
            seen_ns,
        });
        step("release a frame", stream.release(frame))?;
    }
    let mut kept = Vec::new();
    let deadline = Instant::now() + DEADLINE;
    while kept.len() < BUFFERS && Instant::now() < deadline {
        if let Some(frame) = step("next frame", stream.next_frame(Duration::from_millis(500)))? {
            kept.push(frame);
        }
    }
    std::thread::sleep(HOLD);
    let mut held = Vec::new();
    for frame in &kept {
        let bytes = stream.bytes(frame);
        let first = bytes.first().copied().unwrap_or(0);
        held.push(Held {
            sequence: frame.sequence(),
            first,
            uniform: bytes.iter().all(|b| *b == first),
        });
    }
    for frame in kept {
        step("release a held frame", stream.release(frame))?;
    }
    Ok((taken, held))
}

fn run(lemnosd: &Path, receiver: &str, heap: &str, report: &mut Report) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("lemnos-compose-{}", std::process::id()));
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
    let (frames, held) = taken?;
    let size = layout.size as usize;
    let fired = fired?;
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
    report.check(
        frames.windows(2).all(|w| w[0].sequence < w[1].sequence),
        "the sequence numbers only go up",
    );
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
    let follows = pattern_follows_sequence(
        &frames
            .iter()
            .map(|f| (f.sequence, f.first))
            .collect::<Vec<_>>(),
    );
    report.check(
        follows.is_ok(),
        format_args!(
            "each frame carries the pattern its sequence names ({})",
            match &follows {
                Ok(base) => format!("base {base}, {} frames", frames.len()),
                Err(why) => why.clone(),
            }
        ),
    );
    // The held buffers, read after the fake fired on through the hold: each still carries its
    // own frame's pattern, and the pattern follows the sequence on from the frames before it.
    report.check(
        held.len() == BUFFERS && held.iter().all(|h| h.uniform),
        format_args!(
            "{} frames held through a run of frames each keep one pattern value",
            held.len()
        ),
    );
    let all: Vec<(u64, u8)> = frames
        .iter()
        .map(|f| (f.sequence, f.first))
        .chain(held.iter().map(|h| (h.sequence, h.first)))
        .collect();
    let held_follow = pattern_follows_sequence(&all);
    report.check(
        held_follow.is_ok(),
        format_args!(
            "the held frames still carry the pattern their sequence names ({})",
            match &held_follow {
                Ok(base) => format!("base {base}, {} frames held", held.len()),
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

/// Whether every frame carries the pattern its own sequence number names. The fake makes one step
/// per frame produced, so `pattern - sequence` (mod 256) is one base for all frames; a gap in the
/// sequence is a frame the driver dropped and is allowed. Returns the base, or the first frame that
/// breaks it. An empty list is refused: nothing was checked.
fn pattern_follows_sequence(frames: &[(u64, u8)]) -> Result<u8, String> {
    let Some(&(first_seq, first)) = frames.first() else {
        return Err("no frames to check".to_owned());
    };
    let base = first.wrapping_sub(first_seq as u8);
    for &(sequence, pattern) in frames {
        let expected = base.wrapping_add(sequence as u8);
        if pattern != expected {
            return Err(format!(
                "sequence {sequence} carries pattern {pattern}, expected {expected} (base {base})"
            ));
        }
    }
    Ok(base)
}

/// The old rule, kept for the test that shows it passes a stale frame across a gap.
#[cfg(test)]
fn consecutive_pairs_step_by_one(frames: &[(u64, u8)]) -> bool {
    let pairs: Vec<bool> = frames
        .windows(2)
        .filter(|w| w[1].0 == w[0].0 + 1)
        .map(|w| w[1].1 == w[0].1.wrapping_add(1))
        .collect();
    !pairs.is_empty() && pairs.iter().all(|ok| *ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forced_drop_leaves_a_gap_and_every_frame_still_follows_its_sequence() {
        // Frames 0, 1, then frame 2 dropped by the driver (its sequence number is taken, its
        // pattern is advanced by the fake when it was produced), then 3 and 4.
        let frames = [(0u64, 1u8), (1, 2), (3, 4), (4, 5)];
        assert_eq!(pattern_follows_sequence(&frames), Ok(1));
        // The old rule also passes this one: it only looks across consecutive sequence numbers.
        assert!(consecutive_pairs_step_by_one(&frames));
    }

    #[test]
    fn a_stale_frame_across_a_gap_is_caught_by_the_sequence_rule_and_not_the_old_one() {
        // Frame 3 was delivered with frame 1's pattern: its fill is missing. Its pair with frame 1
        // is across a gap, which the old rule never looked at, so the old rule passes it wrongly.
        let frames = [(0u64, 1u8), (1, 2), (3, 2)];
        assert!(
            consecutive_pairs_step_by_one(&frames),
            "old rule: passes it wrongly"
        );
        let why = pattern_follows_sequence(&frames).unwrap_err();
        assert!(why.contains("sequence 3 carries pattern 2"), "{why}");
    }

    #[test]
    fn a_stale_frame_next_to_its_neighbour_is_caught_by_both_rules() {
        let frames = [(0u64, 1u8), (1, 2), (2, 2)];
        assert!(!consecutive_pairs_step_by_one(&frames));
        assert!(pattern_follows_sequence(&frames).is_err());
    }

    #[test]
    fn a_forced_drop_on_the_old_fake_fails_the_new_rule_and_passes_the_old_one_wrongly() {
        // The old fake stepped the pattern only when a buffer was latched: frame 2 was dropped with
        // no step, so frame 3 carries 3. The new rule catches it; the old rule passes it, because
        // the pair is across the gap.
        let frames = [(0u64, 1u8), (1, 2), (3, 3)];
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
        let frames = [(255u64, 7u8), (256, 8), (257, 9)];
        assert_eq!(pattern_follows_sequence(&frames), Ok(8));
    }

    #[test]
    fn no_frames_is_refused_not_passed() {
        assert!(pattern_follows_sequence(&[]).is_err());
    }
}
