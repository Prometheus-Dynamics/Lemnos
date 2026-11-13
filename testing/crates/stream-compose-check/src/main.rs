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
//!   and where two frames follow each other in the sequence, the pattern goes up by one.
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

/// The frames the client takes, in order.
fn take(client: &Client, index: u32, name: &str) -> Result<Vec<Taken>, String> {
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
    Ok(taken)
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
    let frames = taken?;
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
    }
    report.check(
        frames.iter().all(|f| f.uniform),
        "every frame's bytes are one pattern value",
    );
    let pairs: Vec<bool> = frames
        .windows(2)
        .filter(|w| w[1].sequence == w[0].sequence + 1)
        .map(|w| w[1].first == w[0].first.wrapping_add(1))
        .collect();
    report.check(
        !pairs.is_empty() && pairs.iter().all(|ok| *ok),
        format_args!(
            "consecutive frames carry the next pattern value ({} pairs)",
            pairs.len()
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
