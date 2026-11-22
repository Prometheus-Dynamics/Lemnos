//! `lemnos-lend-raw-check`: a receiver's frames lent, with no copy, to the PiSP back end, in the
//! QEMU harness, through `lemnosd`.
//!
//! ```text
//! lemnos-lend-raw-check <lemnosd> <receiver node name> <back end node name> <heap node>
//! ```
//!
//! The board is written here: the fake receiver (`rp1-csi2`, `frames` in dma-bufs from the CMA
//! heap) linked to the PiSP back end (`pisp-be`), which names `frames` with `lend`, so each job
//! that names a buffer of `frames` is given that buffer, lent. The back end has no `input` stream:
//! the only input a job can have is a lent frame, so no job can be served by a copy.
//!
//! Checked, in order:
//!
//! 1. Zero copy, per job. The back end says it takes a lent input (`info().input_lend`). For each
//!    lent job, the dma-buf the back end queued as its input (`Counters::last_lent_inode`, read
//!    right after that job's output) is the same dma-buf, by inode, as the receiver's buffer that
//!    carried the frame. One job is in flight at a time, so the last lent inode is that job's. The
//!    back end has no `input` stream, so a copy has nowhere to go.
//!
//! 2. The hold is honoured under lending. The check holds two frames (its declared hold, `HOLD`)
//!    and reads more frames while they are held, lending some of them too. A frame delivered while
//!    the others are held is never in a held frame's buffer, and the held frames' bytes and
//!    sequences are the same at the end as when they were taken. No sleep synchronises: the reads
//!    and jobs themselves are what the check waits for.
//!
//! It prints one line per check and exits nonzero on any failure.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use lemnos_client::{Client, FrameStream, HeldFrame, Next};
use lemnos_contract_camera_isp::{BayerOrder, CameraIsp, IspInput, IspJob, IspOutput, NO_BUFFER};
use lemnos_contract_camera_receiver::{CameraReceiver, LinkConfig, Packing};
use lemnos_contract_isp_pisp::LAYOUT_BACK_END;
use lemnos_core::{Encode, Writer};
use lemnos_daemon_ready::Ready;
use lemnos_driver_pisp::be::BackEnd;
use lemnos_driver_pisp::format::{compute_stride_align, formats};
use lemnos_driver_pisp::uapi::{
    BayerOrder as Order, BeCcmConfig, BeTilesConfig, ImageFormatConfig, Wire,
};
use lemnos_linux_stream::LinuxStreams;
use lemnos_linux_sys::dma_buf;
use lemnos_linux_sys::test_csi2::{FE_ACK, FS, fire};

/// The receiver's name in the board file.
const RECEIVER: &str = "receiver";
/// The back end's name in the board file.
const BACK_END: &str = "be";
/// The receiver's stream, the one the back end lends from.
const FRAMES: &str = "frames";
/// The frames the consumer holds at once: its declared hold. Two are held through the hold phase and
/// one more is read while they are held, so the declaration is 3 (the producer keeps 2 of 6 buffers).
const HOLD: u32 = 3;
/// Frames lent one at a time before the hold phase.
const LENT_JOBS: usize = 12;
/// Frames read, and lent, while the consumer's frames are held.
const HOLD_READS: usize = 12;
/// The most a single frame or job may take to arrive.
const WAIT: Duration = Duration::from_secs(5);
/// How long lemnosd has to say it is ready.
const READY_WAIT: Duration = Duration::from_secs(10);
/// The fake receiver's node, made by the test module.
const TEST_NODE: &str = "/dev/lemnos-test-csi2";
/// Packed 16-bit, 32 by 16: a 64-byte line and a 1024-byte frame. 16 by 16 is the smallest image the
/// configuration library takes (`BE_MIN_TILE_WIDTH` and `BE_MIN_TILE_HEIGHT`).
const LINK: LinkConfig = LinkConfig {
    lanes: 2,
    virtual_channel: 0,
    data_type: 0x2b,
    bit_depth: 16,
    continuous_clock: false,
    width: 32,
    height: 16,
    pixel_rate_hz: 0,
    link_rate_mbps: 800,
};
/// The receiver's frame buffers and their size: the frame's 2048 bytes.
const FRAME_BUFFERS: u32 = 6;
const FRAME_BYTES: u32 = 1024;
/// The back end's output buffers and their size: the NV12 frame of the input, with room to spare.
const OUTPUT_BUFFERS: u32 = 4;
const OUTPUT_BYTES: u32 = 4096;
/// The black level, white balance and colour matrix of the configuration.
const BLACK: u16 = 4096;

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

fn step<T, E: std::fmt::Debug>(what: &str, result: Result<T, E>) -> Result<T, String> {
    result.map_err(|e| format!("{what}: {e:?}"))
}

fn heap_name(node: &str) -> &str {
    node.strip_prefix("/dev/dma_heap/").unwrap_or(node)
}

fn board(heap: &str, receiver: &str, back_end: &str) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"lend-raw\"\n\n\
         [[devices]]\nid = \"{RECEIVER}\"\ndriver = \"rp1-csi2\"\nlink = \"{BACK_END}\"\n\
         config = {{ device = \"{receiver}\" }}\n\n\
         [devices.streams.{FRAMES}]\nbuffers = {FRAME_BUFFERS}\nbuffer_bytes = {FRAME_BYTES}\n\
         memory = \"dma-buf\"\nheap = \"{heap}\"\n\n\
         [[devices]]\nid = \"{BACK_END}\"\ndriver = \"pisp-be\"\nlend = \"{FRAMES}\"\n\
         config = {{ device = \"{back_end}\", heap = \"{heap}\" }}\n\n\
         [devices.streams.config]\nbuffers = 2\nbuffer_bytes = {cfg}\njob_slots = 4\njob_bytes = 128\n\n\
         [devices.streams.output0]\nbuffers = {OUTPUT_BUFFERS}\nbuffer_bytes = {OUTPUT_BYTES}\n\
         memory = \"dma-buf\"\nheap = \"{heap}\"\n",
        cfg = BeTilesConfig::SIZE,
    )
}

/// Starts lemnosd on `board`, waits until it says it is ready, and returns it with its socket.
fn start_daemon(lemnosd: &Path, dir: &Path, board: &str) -> Result<(Daemon, PathBuf), String> {
    let board_path = dir.join("board.toml");
    step("write the board", std::fs::write(&board_path, board))?;
    let socket = lemnos_socket_dir::socket(dir, "sock").unwrap();
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
        }
        Ok(fired)
    })
}

/// The inode of the dma-buf the frame's buffer is, as the check holds it (the same dma-buf, by
/// `fstat`, the daemon hands out for that buffer).
fn own_inode(frames: &FrameStream, buffer: u32) -> Option<u64> {
    let fd = frames.buffer_handle(buffer)?;
    dma_buf::identity(fd).ok().map(|(_, ino)| ino)
}

/// A lent job: the frame's buffer as the input, the frame's identity, and `output` as output 0.
fn lent_job(identity: u64, config: u32, lent: u32, output: u32) -> ([u8; 128], usize) {
    let job = IspJob {
        identity,
        layout: LAYOUT_BACK_END,
        config_buffer: config,
        input_buffer: NO_BUFFER,
        outputs: [output, NO_BUFFER, NO_BUFFER, NO_BUFFER],
        input_lent: true,
        lent_buffer: lent,
    };
    let mut buf = [0u8; 128];
    let mut w = Writer::new(&mut buf);
    let n = job.encode(&mut w).map(|()| w.len()).unwrap_or(0);
    (buf, n)
}

/// Posts the lent job for `frame` on config buffer `config`, output buffer `output`.
fn post_lent(
    cfg: &mut FrameStream,
    frame: &HeldFrame,
    config: u32,
    output: u32,
) -> Result<(), String> {
    let (buf, n) = lent_job(frame.identity(), config, frame.buffer(), output);
    match cfg.post_job(buf.get(..n).unwrap_or(&[])) {
        Ok(true) => Ok(()),
        Ok(false) => Err("the job queue is full".into()),
        Err(e) => Err(format!("post: {e:?}")),
    }
}

/// The next frame of `stream` within `WAIT`, or an error naming what it was waiting for.
fn next(stream: &mut FrameStream, what: &str) -> Result<HeldFrame, String> {
    step(what, stream.next_frame(WAIT))?.ok_or_else(|| format!("{what}: no frame within {WAIT:?}"))
}

/// The output frame carrying `identity`, read in order from `output`.
fn output_for(output: &mut FrameStream, identity: u64) -> Result<HeldFrame, String> {
    loop {
        let frame = next(output, "the output frame of a lent job")?;
        if frame.identity() == identity {
            return Ok(frame);
        }
        step(
            "release an output frame of another job",
            output.release(frame),
        )?;
    }
}

/// The configuration the back end runs, built with the configuration library for the input.
fn configuration(input: ImageFormatConfig) -> Result<Vec<u8>, String> {
    let cfg = BackEnd::simple_bayer(
        input,
        Order::Bggr,
        BLACK,
        (1.5, 1.0, 1.8),
        None,
        formats::NV12,
        Some(BeCcmConfig {
            coeffs: [306, 601, 117, -173, -339, 512, 512, -429, -83],
            pad: [0; 2],
            offsets: [0, 33_554_432, 33_554_432],
        }),
    )
    .prepare()
    .map_err(|e| format!("the configuration library refused: {e:?}"))?;
    let mut bytes = vec![0u8; BeTilesConfig::SIZE];
    cfg.write_to(&mut bytes)
        .map_err(|_| "configuration size".to_string())?;
    Ok(bytes)
}

fn run(
    lemnosd: &Path,
    receiver: &str,
    back_end: &str,
    heap: &str,
    report: &mut Report,
) -> Result<(), String> {
    let dir = lemnos_socket_dir::scratch_dir().unwrap();
    step("make the directory", std::fs::create_dir_all(&dir))?;
    let (daemon, socket) = start_daemon(lemnosd, &dir, &board(heap, receiver, back_end))?;
    let client = step("connect", Client::connect(&socket, "lend-raw-check"))?;
    let list = step("list", client.list())?;
    let index = |name: &str| {
        list.iter()
            .find(|d| d.name == name)
            .map(|d| d.index)
            .ok_or(format!("{name} is not on the daemon's list"))
    };
    let receiver_index = index(RECEIVER)?;
    let be_index = index(BACK_END)?;

    // The receiver makes frames of the layout the link gives.
    let mut camera = step(
        "open the receiver",
        client.open::<dyn CameraReceiver>(RECEIVER),
    )?;
    let layout = step(
        "configure the receiver",
        camera.configure(LINK, Packing::Packed),
    )?;
    report.check(
        layout.size == FRAME_BYTES,
        format_args!("the receiver's frame is {} bytes", layout.size),
    );

    // The back end takes a lent input, and its output is NV12 at the input's size.
    let mut isp = step("open the back end", client.open::<dyn CameraIsp>(BACK_END))?;
    let info = step("info", isp.info())?;
    report.check(
        info.input_lend,
        "the back end takes a lent input (info.input_lend)",
    );
    let mut input = ImageFormatConfig {
        width: LINK.width as u16,
        height: LINK.height as u16,
        format: formats::BAYER16,
        ..Default::default()
    };
    compute_stride_align(&mut input, 64);
    step(
        "set_input",
        isp.set_input(IspInput {
            width: LINK.width,
            height: LINK.height,
            bit_depth: 16,
            bayer: BayerOrder::Bggr,
        }),
    )?;
    let out_layout = step(
        "set_output",
        isp.set_output(
            0,
            IspOutput {
                width: LINK.width,
                height: LINK.height,
                fourcc: u32::from_le_bytes(*b"NV12"),
                pad_bytes: 0,
            },
        ),
    )?;
    report.check(
        out_layout.size <= OUTPUT_BYTES,
        format_args!(
            "the output frame is {} bytes, in a {OUTPUT_BYTES}-byte buffer",
            out_layout.size
        ),
    );

    // Only the lent input's stream exists: there is no `input` for a copy to go into.
    let no_input = client
        .open_stream_writable(be_index, "input", Arc::new(LinuxStreams))
        .is_err();
    report.check(
        no_input,
        "the back end has no input stream: a copy has nowhere to go",
    );

    let platform = Arc::new(LinuxStreams);
    let mut cfg = step(
        "open the config stream",
        client.open_stream_writable(be_index, "config", platform.clone()),
    )?;
    let mut output = step(
        "open output0",
        client.open_stream(be_index, "output0", platform.clone()),
    )?;
    let configs = cfg.layout().buffers;
    let outputs = output.layout().buffers;
    let bytes = configuration(input)?;
    for buffer in 0..configs {
        step(
            "write a configuration",
            cfg.write(buffer, 0, &bytes).map_err(|e| format!("{e:?}")),
        )?;
    }
    step("start the back end", isp.start())?;

    let stop = Arc::new(AtomicBool::new(false));
    let firing = fire_frames(stop.clone());
    step("start the receiver", camera.start())?;
    let mut frames = step(
        "open the frames with a declared hold",
        client.open_stream_held(receiver_index, FRAMES, HOLD, platform.clone()),
    )?;

    // Phase 1: one frame at a time, each lent to one job, so the back end's last lent inode is the
    // job being checked (no two jobs are in flight).
    let mut lent_ok = 0usize;
    let mut same_inode = 0usize;
    let mut frames_ok = true;
    for n in 0..LENT_JOBS {
        let frame = next(&mut frames, "a frame to lend")?;
        let buffer = frame.buffer();
        if buffer >= FRAME_BUFFERS {
            frames_ok = false;
        }
        let mine = own_inode(&frames, buffer);
        let identity = frame.identity();
        let out_buffer = (n as u32) % outputs;
        post_lent(&mut cfg, &frame, (n as u32) % configs, out_buffer)?;
        let out = match output_for(&mut output, identity) {
            Ok(out) => out,
            Err(e) => {
                let counters = isp.counters();
                let mut ended = Vec::new();
                for _ in 0..4 {
                    match cfg.next_or_end(Duration::from_millis(500)) {
                        Ok(Next::JobEnded { identity, reason }) => {
                            ended.push(format!("job {identity} ended: {reason:?}"));
                        }
                        Ok(Next::TimedOut) | Err(_) => break,
                        Ok(other) => ended.push(format!("{other:?}")),
                    }
                }
                let reason = step("list", client.list())?
                    .into_iter()
                    .find(|d| d.name == BACK_END)
                    .map_or_else(|| "not listed".to_owned(), |d| d.reason);
                return Err(format!(
                    "{e}; counters {counters:?}; config stream {ended:?}; back end reason {reason:?}"
                ));
            }
        };
        if out.failure().is_none() && out.len() as u32 == out_layout.size {
            lent_ok += 1;
        }
        let queued = step("counters", isp.counters())?.last_lent_inode;
        if mine.is_some() && mine == Some(queued) {
            same_inode += 1;
        }
        step("release an output frame", output.release(out))?;
        step("release a lent frame", frames.release(frame))?;
    }
    report.check(
        same_inode == LENT_JOBS,
        format_args!(
            "each lent job queued its frame's own dma-buf, by inode ({same_inode} of {LENT_JOBS})"
        ),
    );
    report.check(
        lent_ok == LENT_JOBS,
        format_args!("each lent job made its output frame ({lent_ok} of {LENT_JOBS})"),
    );
    report.check(frames_ok, "every frame names a buffer of the stream");
    let counters = step("counters", isp.counters())?;
    report.check(
        counters.lent >= LENT_JOBS as u64,
        format_args!(
            "the back end counts {} lent jobs (at least {LENT_JOBS} posted)",
            counters.lent
        ),
    );

    // Phase 2: the consumer holds two frames while more are read, and some of those are lent too.
    let held_a = next(&mut frames, "the first frame to hold")?;
    let held_b = next(&mut frames, "the second frame to hold")?;
    let (a_buffer, b_buffer) = (held_a.buffer(), held_b.buffer());
    let (a_seq, b_seq) = (held_a.sequence(), held_b.sequence());
    let a_bytes = frames.bytes(&held_a).to_vec();
    let b_bytes = frames.bytes(&held_b).to_vec();
    let mut refilled = 0usize;
    let mut last_seq = b_seq;
    let mut ordered = true;
    let mut lent_while_held = 0usize;
    let mut lent_identity = 0usize;
    for n in 0..HOLD_READS {
        let frame = next(&mut frames, "a frame while two are held")?;
        if frame.buffer() == a_buffer || frame.buffer() == b_buffer {
            refilled += 1;
        }
        if frame.sequence() <= last_seq {
            ordered = false;
        }
        last_seq = frame.sequence();
        if n % 2 == 0 {
            let identity = frame.identity();
            post_lent(&mut cfg, &frame, (n as u32) % configs, (n as u32) % outputs)?;
            let mine = own_inode(&frames, frame.buffer());
            let out = output_for(&mut output, identity)?;
            if out.failure().is_none() {
                lent_while_held += 1;
            }
            let queued = step("counters", isp.counters())?.last_lent_inode;
            if mine.is_some() && mine == Some(queued) {
                lent_identity += 1;
            }
            step("release an output frame", output.release(out))?;
        }
        step("release a frame read while held", frames.release(frame))?;
    }
    report.check(
        refilled == 0,
        format_args!(
            "no frame was delivered into a buffer the consumer holds ({refilled} into held buffers)"
        ),
    );
    report.check(
        ordered,
        "the frames read while held carry sequences that only go up",
    );
    report.check(
        frames.bytes(&held_a) == a_bytes.as_slice() && frames.bytes(&held_b) == b_bytes.as_slice(),
        "the two held frames read back byte for byte as they were taken",
    );
    report.check(
        held_a.sequence() == a_seq && held_b.sequence() == b_seq,
        "the two held frames keep their sequence numbers",
    );
    report.check(
        lent_identity == lent_while_held && lent_identity >= HOLD_READS / 2,
        format_args!("the jobs lent while two were held queued their own frames' dma-bufs ({lent_identity} of {lent_while_held})"),
    );
    report.check(
        lent_while_held >= HOLD_READS / 2,
        format_args!(
            "frames were lent while two were held ({lent_while_held} of {})",
            HOLD_READS / 2
        ),
    );
    step("release a held frame", frames.release(held_a))?;
    step("release a held frame", frames.release(held_b))?;

    stop.store(true, Ordering::Release);
    let fired = step(
        "the fire thread",
        firing
            .join()
            .map_err(|_| "the fire thread panicked".to_string()),
    )?;
    report.check(fired.is_ok(), "the fake receiver fired its frames");
    let _ = camera.stop();
    let _ = isp.stop();
    drop(daemon);
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [lemnosd, receiver, back_end, heap] = args.as_slice() else {
        let _ = writeln!(
            std::io::stderr(),
            "usage: lemnos-lend-raw-check <lemnosd> <receiver node name> <back end node name> <heap node>"
        );
        return ExitCode::from(2);
    };
    let mut report = Report { failures: 0 };
    if let Err(error) = run(
        Path::new(lemnosd),
        receiver,
        back_end,
        heap_name(heap),
        &mut report,
    ) {
        let _ = writeln!(std::io::stdout(), "FAIL {error}");
        report.failures += 1;
    }
    let _ = writeln!(
        std::io::stdout(),
        "lend-raw-check: {} failure(s)",
        report.failures
    );
    if report.failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
