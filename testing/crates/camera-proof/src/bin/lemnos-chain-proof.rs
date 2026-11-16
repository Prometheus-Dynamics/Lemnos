//! `lemnos-chain-proof`: the whole PiSP camera chain on a real board, through `lemnosd`'s socket.
//!
//! ```text
//! lemnos-chain-proof [--socket PATH] [--sensor NAME] [--receiver NAME] [--front-end NAME]
//!                    [--back-end NAME] [--frames N] [--frame-length N] [--pattern CODE]
//!                    [--reference FILE] [--jobs-queued N] [--fps RATE] [--latch-trials N]
//!                    [--min-mean-luma N] [--allow-skip lit-scene|latch|reference]...
//!                    [--clock-file PATH]
//! ```
//!
//! `--clock-file PATH` is test only: the simulated chain's clock (`lemnos-sim-clock`) replaces
//! `CLOCK_MONOTONIC` for the age check. A board image never passes it. A clock file that is
//! missing, short, unreadable or goes backwards fails the run by name; it never falls back to
//! `CLOCK_MONOTONIC`.
//!
//! A check that is not made is a failure, named in the output: `--fps` (the sensor mode's frame
//! rate, within 2 %), `--reference` (the first frame's lines), `--latch-trials` (the sensor's latch,
//! run as `lemnos-sensor-proof latch` beside the chain, on the same socket). A check is skipped
//! only by a named `--allow-skip` (repeatable), and the output says it was skipped: `lit-scene`
//! lets the checks that need light (the back end's mean luma reaching `--min-mean-luma`, and the
//! latch) be skipped on a dark scene; `latch` skips only the latch; `reference` skips only the
//! reference lines. The others are never skipped.
//!
//! The latch cannot run beside the chain on a board: the receiver's raw `frames` stream and the
//! front end are exclusive in the kernel module (`kernel/lemnos_rp1_csi2.c`, `lrc_start`: `EBUSY`
//! while the front end streams), and the latch takes that stream. So a board run starts the latch
//! on its own first (`lemnos-sensor-proof latch`) and gives this tool `--allow-skip latch`;
//! `--latch-trials` is for a chain whose receiver can share its stream.
//!
//! Timestamps: every front-end and back-end frame's `CLOCK_MONOTONIC` timestamp must be no later
//! than the moment the frame is taken, and no older than one second (a frame start of this run).
//!
//! Dependency: the receiver's counters are read while the chain is linked. They are refused (EBUSY)
//! while the module allows one open per node (kernel `lrc_open`); that is fixed by the kernel
//! change for the receiver's node that another worker is writing. Until it is on the board, the
//! receiver's line is a failure, as it should be.
//!
//! The devices are not named in the tool. It starts from the sensor (`--sensor`, or the board's one
//! camera sensor) and follows the board's `link` from device to device, as `lemnos-ctl chain` does:
//! the sensor feeds the receiver, which feeds the front end, which feeds the back end (the board
//! files `testing/boards/reference.toml` and `pisp-camera-chain.toml` declare such a chain, with
//! different names). `--receiver`, `--front-end` and `--back-end` name a stage instead. A stage it
//! cannot resolve is an error naming what it looked for and what the board has. The front end's
//! `output0` (16-bit Bayer, 2560 bytes a line) is LENT to the back end, frame by frame: one
//! back-end job per frame names the front-end buffer it is in (`input_lent`), with no copy. The
//! front-end frame is held until the back end's output for its identity has arrived, and then it
//! is released. Two front-end jobs are kept queued (`--jobs-queued`, 1 to 3), and the back end's
//! configuration buffers bound how many frames it holds at once.
//!
//! It takes `--frames` frames at the back end and checks, and prints:
//!
//! - the frame rate from the front end's timestamps, and the frame starts no job was queued for
//!   (the front end's `dropped` counter);
//! - the receiver's counters, the front end's and the back end's (jobs, dropped, errors);
//! - for each of the front end and the back end: the frames, the sequence gaps (a sequence that is
//!   not the previous one plus one), the timestamp steps that do not increase, and the identity
//!   mismatches (a front-end frame whose identity is not its job's, a back-end frame whose
//!   identity is not its front-end frame's, a statistics frame not its output frame's);
//! - the back end's frame interval (median and p99) and the time from a frame's timestamp to this
//!   program holding it (median and max), both on `CLOCK_MONOTONIC`;
//! - the back end's luma: the frames whose luma is all zero;
//! - with `--reference` (one packed RAW10 frame of the same scene), how many of the first front-end
//!   frame's lines equal the reference's, value for value, each moved up 6 bits.
//!
//! It exits 0 only if every frame arrived, with no gap, no timestamp step that does not increase,
//! no identity mismatch, no all-zero back-end luma, no dropped or errored job on any stage, no
//! receiver drop or error, and, with `--reference`, every line equal. It prints the failed checks
//! otherwise and exits 1.
//!
//! It does not check exposure (that needs a lit scene), colour, or the content of the frames
//! beyond the reference. It starts the chain (the sensor is stopped and set up first, as
//! `lemnos-fe-proof` does) and stops the back end and then the front end at the end. It does not
//! stop the sensor or the receiver: the daemon owns them.
//!
//! `--frame-length N` sets the sensor's frame length in lines before the start, and
//! `--pattern CODE` its test pattern, as `lemnos-fe-proof` does.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use lemnos_camera_proof::chain::{Stage, resolve};
use lemnos_camera_proof::setup::{self, post_back, post_back_copied, post_front};
use lemnos_client::{Client, FrameStream, HeldFrame, JobReason, Next};
use lemnos_contract_camera_isp::CameraIsp;
use lemnos_contract_camera_receiver::CameraReceiver;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
const RAW10_STRIDE: usize = 1600;
const USAGE: &str = "usage: lemnos-chain-proof [--socket PATH] [--sensor NAME] [--receiver NAME] [--front-end NAME] [--back-end NAME] [--frames N] [--frame-length N] [--pattern CODE] [--reference FILE] [--jobs-queued N] [--fps RATE] [--latch-trials N] [--min-mean-luma N] [--allow-skip lit-scene|latch|reference]... [--clock-file PATH (test only)]";
/// A timestamp older than this, at the moment the frame is taken, is not a frame start of this run.
const MAX_AGE_NS: u64 = 1_000_000_000;
/// The dark scene's back-end mean luma was about 20 on the board (black level 16); a lit scene is
/// above this.
const DEFAULT_MIN_MEAN_LUMA: u64 = 40;

/// Whether a measured frame rate is within 2 % of the expected one. No expectation: not so.
fn rate_ok(measured: f64, expected: f64) -> bool {
    expected > 0.0 && (measured - expected).abs() <= expected * 0.02
}

/// Whether a frame timestamp is not a `CLOCK_MONOTONIC` frame start taken at `now`: in the future,
/// or older than `MAX_AGE_NS`.
fn age_bad(stamp: u64, now: u64) -> bool {
    lemnos_sim_clock::age_bad(stamp, now, MAX_AGE_NS)
}

/// The clock the frame ages are read on: `CLOCK_MONOTONIC` on a board, the sim clock file under
/// test. Set once by `run`.
static CLOCK: std::sync::OnceLock<Box<dyn lemnos_sim_clock::Clock>> = std::sync::OnceLock::new();
/// The first clock read that failed, named. A failed read fails the run.
static CLOCK_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// `CLOCK_MONOTONIC`, the board's clock.
struct Monotonic;

impl lemnos_sim_clock::Clock for Monotonic {
    fn now_ns(&self) -> Result<u64, String> {
        lemnos_linux_sys::time::monotonic_now()
            .map(|t| u64::try_from(t.as_nanos()).unwrap_or(u64::MAX))
            .map_err(|e| format!("CLOCK_MONOTONIC: {e:?}"))
    }
}

/// A frame the driver made for a failed stream (its give-up or reconfigure path): no bytes, no
/// timestamp. It is counted as a placeholder, never as a stale frame.
fn placeholder(frame: &lemnos_client::HeldFrame) -> bool {
    frame.is_empty() && frame.timestamp_ns() == 0
}

struct Args {
    socket: String,
    /// The sensor the chain starts from. Not given: the board's one camera sensor.
    sensor: Option<String>,
    /// Each stage named on the command line instead of followed from the sensor's links.
    receiver: Option<String>,
    /// Test only: the sim clock file to read frame times from (`--clock-file`).
    clock_file: Option<PathBuf>,
    front_end: Option<String>,
    back_end: Option<String>,
    frames: u64,
    frame_length: Option<u32>,
    pattern: Option<u16>,
    reference: Option<PathBuf>,
    jobs_queued: u64,
    /// The frame rate the sensor's mode gives, to within 2 %. Not given: the rate is not checked.
    fps: Option<f64>,
    /// Trials for `lemnos-sensor-proof latch`, run while the chain runs. Not given: not checked.
    latch_trials: Option<u32>,
    /// The mean luma the back end's output must reach with a lit scene.
    min_mean_luma: u64,
    /// `--allow-skip lit-scene`: the checks that need a lit scene are skipped, not failed.
    allow_skip_lit: bool,
    /// `--allow-skip latch`: the sensor latch is not checked (it was run on its own, or cannot be).
    allow_skip_latch: bool,
    /// `--allow-skip reference`: no reference frame was available, so its lines are not checked.
    allow_skip_reference: bool,
}

fn parse() -> Option<Args> {
    parse_from(std::env::args().collect())
}

fn parse_from(argv: Vec<String>) -> Option<Args> {
    let mut args = Args {
        socket: "/run/lemnos/lemnosd.sock".into(),
        sensor: None,
        receiver: None,
        clock_file: None,
        front_end: None,
        back_end: None,
        frames: 600,
        frame_length: None,
        pattern: None,
        reference: None,
        jobs_queued: 2,
        fps: None,
        latch_trials: None,
        min_mean_luma: DEFAULT_MIN_MEAN_LUMA,
        allow_skip_lit: false,
        allow_skip_latch: false,
        allow_skip_reference: false,
    };
    let mut it = argv.into_iter().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next()?;
        match flag.as_str() {
            "--socket" => args.socket = value,
            "--sensor" => args.sensor = Some(value),
            "--receiver" => args.receiver = Some(value),
            "--clock-file" => args.clock_file = Some(PathBuf::from(value)),
            "--front-end" => args.front_end = Some(value),
            "--back-end" => args.back_end = Some(value),
            "--frames" => args.frames = value.parse().ok()?,
            "--frame-length" => args.frame_length = Some(value.parse().ok()?),
            "--pattern" => args.pattern = Some(value.parse().ok()?),
            "--reference" => args.reference = Some(PathBuf::from(value)),
            "--jobs-queued" => args.jobs_queued = value.parse().ok()?,
            "--fps" => args.fps = Some(value.parse().ok()?),
            "--latch-trials" => args.latch_trials = Some(value.parse().ok()?),
            "--min-mean-luma" => args.min_mean_luma = value.parse().ok()?,
            "--allow-skip" => match value.as_str() {
                "lit-scene" => args.allow_skip_lit = true,
                "latch" => args.allow_skip_latch = true,
                "reference" => args.allow_skip_reference = true,
                _ => return None,
            },
            _ => return None,
        }
    }
    Some(args)
}

fn out(text: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stdout(), "{text}");
}

fn now_ns() -> u64 {
    let read = CLOCK
        .get()
        .ok_or_else(|| "no clock installed".to_string())
        .and_then(|clock| clock.now_ns());
    match read {
        Ok(ns) => ns,
        Err(e) => {
            let mut first = CLOCK_ERROR
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            first.get_or_insert(e);
            0
        }
    }
}

fn percentile(sorted: &[u64], p: usize) -> u64 {
    sorted
        .get((sorted.len() * p / 100).min(sorted.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0)
}

/// Packed RAW10 (four pixels in five bytes, the low bits last) line `row`, unpacked to 10 bits.
fn raw10_line(raw: &[u8], row: usize) -> Vec<u16> {
    let line = raw
        .get(row * RAW10_STRIDE..(row + 1) * RAW10_STRIDE)
        .unwrap_or(&[]);
    let (groups, _) = line.as_chunks::<5>();
    groups
        .iter()
        .flat_map(|[a, b, c, d, low]| {
            [a, b, c, d]
                .into_iter()
                .enumerate()
                .map(move |(i, high)| u16::from(*high) << 2 | u16::from(low >> (2 * i) & 3))
        })
        .collect()
}

/// Output 0's line `row` as stored: 16-bit little-endian.
fn out_line(image: &[u8], stride: usize, row: usize) -> Vec<u16> {
    image
        .get(row * stride..row * stride + WIDTH as usize * 2)
        .unwrap_or(&[])
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u16::from_le_bytes(*p))
        .collect()
}

/// How many of the lines of `image` equal the reference's, value for value.
fn matched_lines(raw: &[u8], image: &[u8], stride: usize) -> usize {
    (0..HEIGHT as usize)
        .filter(|&row| {
            let want = raw10_line(raw, row);
            let got = out_line(image, stride, row);
            want.len() == WIDTH as usize && want.iter().zip(&got).all(|(w, g)| *g == w << 6)
        })
        .count()
}

/// A stream as the daemon declared it, in one line.
fn declared(stream: &FrameStream) -> String {
    let layout = stream.layout();
    format!(
        "{} buffers of {} bytes, {}, {}, {}",
        layout.buffers,
        layout.buffer_bytes,
        if stream.job_driven() {
            "job-driven"
        } else {
            "free-running"
        },
        if stream.buffers_are_dma_bufs() {
            "dma-buf per buffer"
        } else if stream.has_buffer_handles() {
            "memfd per buffer"
        } else {
            "shared memory"
        },
        if stream.is_writable() {
            "taken writable"
        } else {
            "taken read-only"
        }
    )
}

/// A job on the front end's buffer `buffer` (config, statistics and output 0), as
/// `lemnos-fe-proof` posts it.
/// What one stage's frames show from their sequence numbers and timestamps.
#[derive(Default)]
struct Track {
    frames: u64,
    seq_gaps: u64,
    ts_steps_bad: u64,
    last: Option<(u64, u64)>,
    stamps: Vec<u64>,
}

impl Track {
    fn see(&mut self, sequence: u64, stamp: u64) {
        if let Some((seq, ts)) = self.last {
            self.seq_gaps += u64::from(sequence != seq + 1);
            self.ts_steps_bad += u64::from(stamp <= ts);
        }
        self.last = Some((sequence, stamp));
        self.frames += 1;
        self.stamps.push(stamp);
    }

    /// The intervals between successive timestamps, sorted.
    fn intervals(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self
            .stamps
            .windows(2)
            .map(|w| w[1].saturating_sub(w[0]))
            .collect();
        v.sort_unstable();
        v
    }
}

/// The front end's streams and what they show.
struct FrontStage {
    cfg: FrameStream,
    stats: FrameStream,
    out: FrameStream,
    /// Output 0's line stride, the bytes the back end's input takes a line.
    stride: usize,
    track: Track,
    identity_bad: u64,
    stats_bad: u64,
    stats_missing: u64,
    /// Frames whose timestamp is not a recent frame start (see `age_bad`).
    age_bad: u64,
    /// Placeholder frames (see `placeholder`): counted, not checked for age.
    placeholders: u64,
}

/// One back-end job in flight, with the stamps the spans need. Every time is `CLOCK_MONOTONIC`
/// nanoseconds: the front-end frame's end (its EOF, as the module stamped it; 0 when not stamped)
/// and this client's time of kicking the job.
struct Pending {
    identity: u64,
    eof_ns: u64,
    kick_ns: u64,
    /// The front-end frame the job lent, held until the back end's output for it has arrived. `None`
    /// on the copy path: the frame was copied into the back end's input and given back at once.
    frame: Option<HeldFrame>,
}

/// Per-frame spans, in nanoseconds, and how many frames had no stamp to measure from. No queue depth
/// is reported: each span is one frame's own stamps.
#[derive(Default)]
struct Spans {
    /// Front-end EOF to this client's kick of the back-end job.
    eof_to_kick: Vec<u64>,
    /// The kick to the back end's finish (the block's job time, plus any wait in its ring).
    kick_to_finish: Vec<u64>,
    /// The back end's finish to this client taking the frame.
    finish_to_client: Vec<u64>,
    /// Front-end EOF to this client taking the frame: the product span.
    eof_to_client: Vec<u64>,
    /// Front-end frames the module did not stamp (never guessed from their start).
    eof_unstamped: u64,
    /// Back-end frames without a finish stamp.
    finish_unstamped: u64,
}

/// The back end's streams, the jobs in flight and what the frames show.
struct BackStage {
    cfg: FrameStream,
    /// The back end's `input` stream, when the board declares one. On the lent path this program never
    /// writes it (the front end's output 0 is lent to each job); on the copy path it is written.
    input: Option<FrameStream>,
    /// Whether the front end's buffers can be lent (dma-bufs): the lent path. Otherwise the copy path.
    lent: bool,
    out: FrameStream,
    /// Bytes of luma in one output 0 frame.
    luma_bytes: usize,
    track: Track,
    /// The jobs posted and not yet answered, oldest first.
    pending: VecDeque<Pending>,
    latency: Vec<u64>,
    spans: Spans,
    identity_bad: u64,
    zero_luma: u64,
    /// Frames whose timestamp is not a recent `CLOCK_MONOTONIC` frame start (see `age_bad`).
    age_bad: u64,
    placeholders: u64,
    /// Luma summed over every frame, and the bytes it is summed over: the mean.
    luma_total: u64,
    luma_pixels: u64,
}

impl BackStage {
    /// Takes the oldest back-end frame, checks its identity against its job, and releases it.
    /// `Ok(false)` when no frame came for 2 s, after which the jobs in flight are given up.
    fn take(&mut self, front_out: &mut FrameStream) -> Result<bool, String> {
        let Some(job) = self.pending.pop_front() else {
            return Ok(true);
        };
        let expected = job.identity;
        let lent = job.frame;
        let Some(frame) = self
            .out
            .next_frame(Duration::from_secs(2))
            .map_err(|e| format!("back end output0: {e:?}"))?
        else {
            out(format_args!(
                "no back-end frame for 2 s; job {expected} unanswered"
            ));
            self.pending.clear();
            return Ok(false);
        };
        if placeholder(&frame) {
            self.placeholders += 1;
            self.out
                .release(frame)
                .map_err(|e| format!("release: {e:?}"))?;
            return Ok(true);
        }
        let now = now_ns();
        let finish = frame.end_ns();
        if finish == 0 {
            self.spans.finish_unstamped += 1;
        } else {
            self.spans
                .kick_to_finish
                .push(finish.saturating_sub(job.kick_ns));
            self.spans.finish_to_client.push(now.saturating_sub(finish));
            if job.eof_ns != 0 {
                self.spans
                    .eof_to_client
                    .push(now.saturating_sub(job.eof_ns));
            }
        }
        self.track.see(frame.sequence(), frame.timestamp_ns());
        self.latency.push(now.saturating_sub(frame.timestamp_ns()));
        self.age_bad += u64::from(age_bad(frame.timestamp_ns(), now));
        self.identity_bad += u64::from(frame.identity() != expected);
        let luma = self.out.bytes(&frame).get(..self.luma_bytes).unwrap_or(&[]);
        self.zero_luma += u64::from(luma.iter().all(|&b| b == 0));
        self.luma_total += luma.iter().map(|&b| u64::from(b)).sum::<u64>();
        self.luma_pixels += luma.len() as u64;
        self.out
            .release(frame)
            .map_err(|e| format!("release: {e:?}"))?;
        // The front-end buffer the job lent is free again once its output has come back.
        if let Some(lent) = lent {
            front_out
                .release(lent)
                .map_err(|e| format!("front end release: {e:?}"))?;
        }
        Ok(true)
    }
}

/// Drives the chain for `frames` frames. Front-end frame `k` (job identity `k + 1`) is copied into
/// back-end input and config buffer `k % slots`, and its back-end job names its identity; a
/// back-end buffer is written only after the job that held it has been answered. Returns how many
/// of the first front-end frame's lines equal the reference, when one is given.
fn pump(
    args: &Args,
    front: &mut FrontStage,
    back: &mut BackStage,
    reference: Option<&[u8]>,
) -> Result<Option<usize>, String> {
    let fe_slots = u64::from(front.out.layout().buffers);
    // The back end's buffers that hold a job: its configurations, and on the copy path its input too.
    let be_slots = if back.lent {
        u64::from(back.cfg.layout().buffers)
    } else {
        let input = back.input.as_ref().map_or(0, |i| i.layout().buffers);
        u64::from(input.min(back.cfg.layout().buffers))
    };
    let in_bytes = front.stride * HEIGHT as usize;
    let be_out_slots = u64::from(back.out.layout().buffers);
    let queued = args.jobs_queued;
    let mut matched = None;
    let mut posted = 0u64;
    while posted < queued.min(args.frames) {
        post_front(&mut front.cfg, posted + 1, (posted % fe_slots) as u32)?;
        posted += 1;
    }
    for k in 0..args.frames {
        let Some(frame) = front
            .out
            .next_frame(Duration::from_secs(2))
            .map_err(|e| format!("front end output0: {e:?}"))?
        else {
            out(format_args!("no front-end frame for 2 s after {k} frames"));
            break;
        };
        // The next job goes in at once: the front end keeps its queue while the back end is busy.
        if posted < args.frames {
            post_front(&mut front.cfg, posted + 1, (posted % fe_slots) as u32)?;
            posted += 1;
        }
        if k == 0 {
            matched =
                reference.map(|raw| matched_lines(raw, front.out.bytes(&frame), front.stride));
        }
        if placeholder(&frame) {
            front.placeholders += 1;
        } else {
            front.track.see(frame.sequence(), frame.timestamp_ns());
            front.age_bad += u64::from(age_bad(frame.timestamp_ns(), now_ns()));
        }
        front.identity_bad += u64::from(frame.identity() != k + 1);
        match front
            .stats
            .next_frame(Duration::from_secs(2))
            .map_err(|e| format!("statistics: {e:?}"))?
        {
            Some(st) => {
                front.stats_bad += u64::from(st.identity() != frame.identity());
                front
                    .stats
                    .release(st)
                    .map_err(|e| format!("release: {e:?}"))?;
            }
            None => front.stats_missing += 1,
        }
        // The back end's buffer `slot` is free once the job that held it has been answered.
        let mut stalled = false;
        while back.pending.len() as u64 >= be_slots {
            if !back.take(&mut front.out)? {
                stalled = true;
                break;
            }
        }
        if stalled {
            break;
        }
        let slot = (k % be_slots) as u32;
        let identity = frame.identity();
        let eof_ns = frame.end_ns();
        let lent_buffer = frame.buffer();
        let kick_ns = now_ns();
        if eof_ns == 0 {
            back.spans.eof_unstamped += 1;
        } else {
            back.spans.eof_to_kick.push(kick_ns.saturating_sub(eof_ns));
        }
        let output = (k % be_out_slots) as u32;
        let held = if back.lent {
            post_back(&mut back.cfg, identity, slot, lent_buffer, output)?;
            Some(frame)
        } else {
            // The copy path: the frame's bytes go into the back end's input buffer `slot`, and the
            // front-end frame is given back at once, as the job reads the copy.
            let input = back
                .input
                .as_mut()
                .ok_or("the copy path needs the back end's input stream")?;
            let bytes = front.out.bytes(&frame);
            input
                .write(slot, 0, &bytes[..bytes.len().min(in_bytes)])
                .map_err(|e| format!("back end input write: {e:?}"))?;
            front
                .out
                .release(frame)
                .map_err(|e| format!("release: {e:?}"))?;
            post_back_copied(&mut back.cfg, identity, slot, output)?;
            None
        };
        back.pending.push_back(Pending {
            identity,
            eof_ns,
            kick_ns,
            frame: held,
        });
    }
    while !back.pending.is_empty() {
        if !back.take(&mut front.out)? {
            break;
        }
    }
    Ok(matched)
}

/// The refusal of a lent job on a memfd, asserted for real: one front-end frame is lent to one back-end
/// job, and the back end must end that job with `bad-config` (the kernel refused the lend of a file
/// that is not a dma-buf, and the block refuses the job as its consumer is told). No frame is made
/// from it. `Ok` is the line that says so.
fn probe_refusal(front: &mut FrontStage, back: &mut BackStage) -> Result<String, String> {
    // The front end makes a frame only for a job, so the probe queues one of its own, and lends its frame.
    const PROBE: u64 = 1 << 40;
    post_front(&mut front.cfg, PROBE, 0).map_err(|e| format!("lend probe: {e}"))?;
    let Some(frame) = front
        .out
        .next_frame(Duration::from_secs(2))
        .map_err(|e| format!("lend probe: front end output0: {e:?}"))?
    else {
        return Err("lend probe: the front end made no frame for the probe job".into());
    };
    let identity = frame.identity();
    if identity != PROBE {
        return Err(format!(
            "lend probe: the frame is identity {identity}, not the probe's"
        ));
    }
    let buffer = frame.buffer();
    let posted = post_back(&mut back.cfg, identity, 0, buffer, 0);
    let answer = match posted {
        Err(e) => Err(e),
        Ok(()) => (0..8)
            .find_map(|_| match back.out.next_or_end(Duration::from_secs(2)) {
                Ok(Next::JobEnded {
                    identity: got,
                    reason,
                }) if got == identity => Some(Ok(reason)),
                Ok(Next::Frame(_)) => None,
                Ok(_) => None,
                Err(e) => Some(Err(format!("lend probe: back end output0: {e:?}"))),
            })
            .unwrap_or_else(|| Err("lend probe: the back end did not end the job".into())),
    };
    front
        .out
        .release(frame)
        .map_err(|e| format!("lend probe: release: {e:?}"))?;
    match answer? {
        JobReason::BadConfig => Ok(format!(
            "lend probe: a lent job on a memfd frame (identity {identity}) was refused: job end bad-config, the kernel's refusal of a file that is not a dma-buf; no frame came from it"
        )),
        other => Err(format!("lend probe: refused as {other:?}, not bad-config")),
    }
}

#[allow(clippy::too_many_lines)]
fn run(args: &Args) -> Result<bool, String> {
    let clock: Box<dyn lemnos_sim_clock::Clock> = match &args.clock_file {
        None => Box::new(Monotonic),
        Some(path) => Box::new(lemnos_sim_clock::FileClock::open(path)?),
    };
    let clock_name = match &args.clock_file {
        None => "CLOCK_MONOTONIC".to_string(),
        Some(path) => format!("sim clock file {}", path.display()),
    };
    CLOCK
        .set(clock)
        .map_err(|_| "clock already installed".to_string())?;
    out(format_args!("clock: {clock_name}"));
    if !(1..=3).contains(&args.jobs_queued) {
        return Err("--jobs-queued takes 1 to 3".into());
    }
    if args.clock_file.is_some() && args.latch_trials.is_some() {
        return Err("--latch-trials and --clock-file: under the simulated clock the latch is checked in process (tests/chain_latch_sim.rs)".into());
    }
    let client = Client::connect(&args.socket, "lemnos-chain-proof")
        .map_err(|e| format!("{}: {e}", args.socket))?;
    client.set_timeout(Duration::from_secs(5));
    let list = client.list().map_err(|e| format!("list: {e}"))?;
    let names = resolve(
        &list,
        args.sensor.as_deref(),
        &[
            Stage {
                label: "receiver",
                flag: "--receiver",
                name: args.receiver.as_deref(),
            },
            Stage {
                label: "front end",
                flag: "--front-end",
                name: args.front_end.as_deref(),
            },
            Stage {
                label: "back end",
                flag: "--back-end",
                name: args.back_end.as_deref(),
            },
        ],
    )?;
    let (sensor_name, receiver_name, front_name, back_name) = match names.stages.as_slice() {
        [receiver, front, back] => (&names.sensor, receiver, front, back),
        _ => return Err("the chain resolved to the wrong number of stages".into()),
    };
    out(format_args!(
        "chain: {sensor_name} -> {receiver_name} -> {front_name} -> {back_name}"
    ));
    let mut chain = setup::Chain::open(
        &client,
        &list,
        [sensor_name, receiver_name, front_name, back_name],
        setup::Tuning {
            frame_length: args.frame_length,
            pattern: args.pattern,
        },
    )?;
    let mode = chain.mode;
    out(format_args!(
        "sensor mode 0: {}x{}, {} lanes at {} Mbit/s, {:?}; line length {}, frame length {}..={}",
        mode.link.width,
        mode.link.height,
        mode.link.lanes,
        mode.link.link_rate_mbps,
        mode.bayer,
        mode.line_length,
        mode.frame_length_min,
        mode.frame_length_max
    ));
    out(format_args!(
        "front end: output 0 stride {} size {}; back end: input stride {}, output 0 stride {} size {}, {} tiles",
        chain.fe_stride,
        chain.fe_size,
        chain.input_stride,
        chain.be_stride,
        chain.be_size,
        chain.be_tiles
    ));

    // The stages start before any stream is read: the back end, the front end, then the sensor.
    chain.start()?;
    let (fe_stride, be_stride) = (chain.fe_stride, chain.be_stride);
    let mut fe = chain.fe;
    let mut be = chain.be;
    let mut receiver = chain.receiver;
    // Kept for the run: the sensor is the daemon's, and its client is only dropped at the end.
    let _sensor = chain.sensor;
    let mut front = FrontStage {
        cfg: chain.front.cfg,
        stats: chain.front.stats,
        out: chain.front.out,
        stride: fe_stride as usize,
        track: Track::default(),
        identity_bad: 0,
        stats_bad: 0,
        stats_missing: 0,
        age_bad: 0,
        placeholders: 0,
    };
    // The path is the host's, by its buffers: the front end's output 0 is lent when it is a stream of
    // dma-bufs, and copied through the back end's input when it is memfds (a memfd is not a dma-buf,
    // and the kernel refuses to lend it). The proof says which ran.
    let lent = front.out.buffers_are_dma_bufs();
    if !lent && chain.back.input.is_none() {
        return Err("the front end's buffers are memfds, so the copy path needs the back end's input stream, and the board declares none".into());
    }
    out(format_args!(
        "front end output0: {}",
        if lent {
            "dma-bufs: the lent path (each frame is lent to its back-end job)"
        } else {
            "memfds: the copy path (the frame is copied into the back end's input; a memfd cannot be lent)"
        }
    ));
    let mut back = BackStage {
        cfg: chain.back.cfg,
        input: chain.back.input,
        lent,
        out: chain.back.out,
        luma_bytes: be_stride as usize * HEIGHT as usize,
        track: Track::default(),
        pending: VecDeque::new(),
        latency: Vec::new(),
        spans: Spans::default(),
        identity_bad: 0,
        zero_luma: 0,
        age_bad: 0,
        placeholders: 0,
        luma_total: 0,
        luma_pixels: 0,
    };
    // What the board run must show before it can claim the path: each stage bound, and each
    // stream as the daemon declared it (buffers, real buffer size, who makes its frames, what its
    // memory is, whether this program may fill it).
    for (label, name, streams) in [
        (
            "front end",
            front_name,
            [
                ("config", &front.cfg),
                ("statistics", &front.stats),
                ("output0", &front.out),
            ]
            .as_slice(),
        ),
        (
            "back end",
            back_name,
            [("config", &back.cfg), ("output0", &back.out)].as_slice(),
        ),
    ] {
        if let Some(d) = list.iter().find(|d| d.name == *name) {
            out(format_args!(
                "stage {label}: {name} ({}) {:?} {:?}, provides {}",
                d.driver,
                d.state,
                d.health,
                d.tags.join(", ")
            ));
        }
        for (stream, handle) in streams {
            out(format_args!("  {name} {stream}: {}", declared(handle)));
        }
        if label == "back end" {
            match (&back.input, lent) {
                (Some(input), true) => out(format_args!(
                    "  {name} input: {} (not written: the front end's output0 is lent to each job)",
                    declared(input)
                )),
                (Some(input), false) => out(format_args!(
                    "  {name} input: {} (written: the front end's frames are copied in, not lent)",
                    declared(input)
                )),
                (None, _) => out(format_args!(
                    "  {name} input: none declared; the front end's output0 is lent to each job"
                )),
            }
        }
    }
    let reference = match &args.reference {
        Some(path) => Some(std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?),
        None => None,
    };

    let latch = spawn_latch(args, sensor_name)?;
    let started = Instant::now();
    let pumped = pump(args, &mut front, &mut back, reference.as_deref());
    out(format_args!(
        "{} back-end frames in {:.1} s",
        back.track.frames,
        started.elapsed().as_secs_f64()
    ));

    // Each counter is read on its own, and every line prints whatever the others did.
    let fe_counters = fe
        .counters()
        .map_err(|s| format!("front end counters: {s:?}"));
    let be_counters = be
        .counters()
        .map_err(|s| format!("back end counters: {s:?}"));
    let rx = receiver.counters();
    // On the copy path, the refusal of a lent job on a memfd is asserted for real: one lent job, which the
    // back end must refuse. It is read after the counters, so it cannot change them.
    let probe = if lent {
        None
    } else {
        Some(probe_refusal(&mut front, &mut back))
    };
    // Back end first, then the front end. The sensor and the receiver are left running.
    let mut stop_errors = Vec::new();
    if let Err(s) = be.stop() {
        stop_errors.push(format!("back end stop: {s:?}"));
    }
    if let Err(s) = fe.stop() {
        stop_errors.push(format!("front end stop: {s:?}"));
    }
    let mut failed = Vec::new();
    match probe {
        None => {}
        Some(Ok(line)) => out(format_args!("{line}")),
        Some(Err(e)) => {
            out(format_args!("{e}"));
            failed.push(e);
        }
    }
    let matched = match pumped {
        Ok(m) => m,
        Err(e) => {
            out(format_args!("run stopped early: {e}"));
            failed.push(format!("run stopped early: {e}"));
            None
        }
    };
    let fe_counters = match fe_counters {
        Ok(c) => Some(c),
        Err(e) => {
            out(format_args!("{e}"));
            failed.push(e);
            None
        }
    };
    let be_counters = match be_counters {
        Ok(c) => Some(c),
        Err(e) => {
            out(format_args!("{e}"));
            failed.push(e);
            None
        }
    };
    let rx = match rx {
        Ok(c) => Some(c),
        Err(s) => {
            // The receiver's node is opened by the front end too, and the module allows one open:
            // while the chain is linked the receiver's counters are not readable (EBUSY). That is
            // not a receiver fault and not a pass: the receiver's checks are not made.
            out(format_args!(
                "receiver counters: not readable while the front end holds its node (EBUSY); receiver checks not checked ({s:?})"
            ));
            failed.push("receiver counters not checked (EBUSY while the chain is linked)".into());
            None
        }
    };
    for e in &stop_errors {
        out(format_args!("{e}"));
        failed.push(e.clone());
    }
    let latch = latch.map(Child::wait_with_output);
    if let Some(Ok(done)) = &latch {
        for line in String::from_utf8_lossy(&done.stdout).lines() {
            out(format_args!("sensor latch: {line}"));
        }
        for line in String::from_utf8_lossy(&done.stderr).lines() {
            out(format_args!("sensor latch (stderr): {line}"));
        }
    }

    let fe_track = &front.track;
    let be_track = &back.track;
    let fps = match fe_track.stamps.as_slice() {
        [first, .., last] if last > first => {
            (fe_track.frames - 1) as f64 * 1e9 / (last - first) as f64
        }
        _ => 0.0,
    };
    let intervals = be_track.intervals();
    let mut latency = back.latency.clone();
    latency.sort_unstable();
    out(format_args!("frame rate from timestamps: {fps:.3} fps"));
    if let Some(c) = fe_counters {
        out(format_args!(
            "frame starts with no job (front end dropped): {}",
            c.dropped
        ));
        out(format_args!(
            "front end: jobs {}, dropped {}, errors {}",
            c.jobs, c.dropped, c.errors
        ));
    }
    if let Some(c) = be_counters {
        out(format_args!(
            "back end: jobs {}, dropped {}, errors {}",
            c.jobs, c.dropped, c.errors
        ));
    }
    if let Some(c) = rx {
        out(format_args!(
            "receiver: frames {}, dropped {}, errors {}",
            c.frames, c.dropped, c.errors
        ));
    }
    out(format_args!(
        "front-end frames {}, sequence gaps {}, timestamp steps not increasing {}, identity mismatches {}",
        fe_track.frames, fe_track.seq_gaps, fe_track.ts_steps_bad, front.identity_bad
    ));
    out(format_args!(
        "statistics frames: missing {}, identity mismatches {}",
        front.stats_missing, front.stats_bad
    ));
    let mean_luma = back.luma_total.checked_div(back.luma_pixels).unwrap_or(0);
    out(format_args!(
        "back-end mean luma {mean_luma}; frames taken more than 1 s after their start: front end {} of {}, back end {} of {}; placeholder frames: front end {}, back end {}",
        front.age_bad,
        fe_track.frames,
        back.age_bad,
        be_track.frames,
        front.placeholders,
        back.placeholders
    ));
    out(format_args!(
        "back-end frames {}, sequence gaps {}, timestamp steps not increasing {}, identity mismatches {}, all-zero luma {}",
        be_track.frames,
        be_track.seq_gaps,
        be_track.ts_steps_bad,
        back.identity_bad,
        back.zero_luma
    ));
    out(format_args!(
        "back-end frame interval: median {} us, p99 {} us",
        percentile(&intervals, 50) / 1000,
        percentile(&intervals, 99) / 1000
    ));
    out(format_args!(
        "back-end frame timestamp to client: median {} us, max {} us",
        percentile(&latency, 50) / 1000,
        latency.last().copied().unwrap_or(0) / 1000
    ));
    let span = |name: &str, values: &[u64]| {
        let mut sorted = values.to_vec();
        sorted.sort_unstable();
        out(format_args!(
            "span {name}: median {} us, max {} us over {} frames",
            percentile(&sorted, 50) / 1000,
            sorted.last().copied().unwrap_or(0) / 1000,
            sorted.len()
        ));
    };
    span("EOF to kick", &back.spans.eof_to_kick);
    span("kick to back-end finish", &back.spans.kick_to_finish);
    span("back-end finish to client", &back.spans.finish_to_client);
    span(
        "EOF to client (the product span)",
        &back.spans.eof_to_client,
    );
    out(format_args!(
        "stamps missing, not guessed: front-end EOF {} frames, back-end finish {} frames",
        back.spans.eof_unstamped, back.spans.finish_unstamped
    ));
    if let Some(equal) = matched {
        out(format_args!(
            "first front-end frame: {equal} of {HEIGHT} lines equal the reference's"
        ));
    }
    out(format_args!(
        "frames delivered end to end: {}",
        be_track.frames
    ));

    let mut check = |ok: bool, what: String| {
        if !ok {
            failed.push(what);
        }
    };
    check(
        fe_track.frames == args.frames,
        format!("front-end frames {} of {}", fe_track.frames, args.frames),
    );
    check(
        be_track.frames == args.frames,
        format!("back-end frames {} of {}", be_track.frames, args.frames),
    );
    check(
        fe_track.seq_gaps == 0 && be_track.seq_gaps == 0,
        format!(
            "sequence gaps: front end {}, back end {}",
            fe_track.seq_gaps, be_track.seq_gaps
        ),
    );
    check(
        fe_track.ts_steps_bad == 0 && be_track.ts_steps_bad == 0,
        format!(
            "timestamp steps not increasing: front end {}, back end {}",
            fe_track.ts_steps_bad, be_track.ts_steps_bad
        ),
    );
    check(
        front.identity_bad == 0 && back.identity_bad == 0 && front.stats_bad == 0,
        format!(
            "identity mismatches: front end {}, statistics {}, back end {}",
            front.identity_bad, front.stats_bad, back.identity_bad
        ),
    );
    check(
        front.stats_missing == 0,
        format!("statistics frames missing {}", front.stats_missing),
    );
    check(
        back.zero_luma == 0,
        format!("back-end frames with all-zero luma {}", back.zero_luma),
    );
    check(
        fe_counters.is_some_and(|c| c.dropped == 0 && c.errors == 0),
        format!("front end counters not clean: {fe_counters:?}"),
    );
    check(
        be_counters.is_some_and(|c| c.dropped == 0 && c.errors == 0),
        format!("back end counters not clean: {be_counters:?}"),
    );
    check(
        rx.is_none_or(|c| c.dropped == 0 && c.errors == 0),
        format!("receiver dropped or errored: {rx:?}"),
    );
    // A check that is not made fails the run. Only the checks that need a lit scene may be skipped
    // (`--allow-skip lit-scene`).
    if matched.is_none() && args.allow_skip_reference {
        out(format_args!(
            "first front-end frame: reference lines not checked (--allow-skip reference)"
        ));
    }
    check(
        matched == Some(HEIGHT as usize) || (matched.is_none() && args.allow_skip_reference),
        match matched {
            None => "reference lines not checked: give --reference".into(),
            Some(n) => {
                format!("first front-end frame lines equal to the reference: {n} of {HEIGHT}")
            }
        },
    );
    check(
        args.fps.is_some_and(|e| rate_ok(fps, e)),
        match args.fps {
            None => "frame rate not checked: give --fps".into(),
            Some(e) => format!("frame rate {fps:.3} fps is not within 2% of {e}"),
        },
    );
    check(
        front.age_bad == 0 && back.age_bad == 0,
        format!(
            "frames taken more than 1 s after their start: front end {} of {}, back end {} of {}",
            front.age_bad, fe_track.frames, back.age_bad, be_track.frames
        ),
    );
    check(
        front.placeholders == 0 && back.placeholders == 0,
        format!(
            "placeholder frames delivered (no bytes, timestamp 0): front end {}, back end {}",
            front.placeholders, back.placeholders
        ),
    );
    let clock_failure = CLOCK_ERROR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    check(
        clock_failure.is_none(),
        format!(
            "clock read failed: {}",
            clock_failure.as_deref().unwrap_or("")
        ),
    );
    if args.allow_skip_lit {
        out(format_args!(
            "back-end mean luma {mean_luma}: lit-scene check skipped (--allow-skip lit-scene)"
        ));
    } else {
        check(
            mean_luma >= args.min_mean_luma,
            format!(
                "back-end mean luma {mean_luma} below {}: the scene is dark (--allow-skip lit-scene skips this check)",
                args.min_mean_luma
            ),
        );
    }
    let skip_latch = args.allow_skip_lit || args.allow_skip_latch;
    let why = if args.allow_skip_latch {
        "--allow-skip latch"
    } else {
        "lit-scene check skipped"
    };
    if args.clock_file.is_some() {
        // Under the simulated clock the latch is run in process by `tests/chain_latch_sim.rs`, the
        // same function the board's latch runs; the host-timed latch is not run beside it.
        out(format_args!("sensor latch: checked in-process (sim clock)"));
    } else {
        match latch {
            None if skip_latch => out(format_args!("sensor latch: not run ({why})")),
            None => check(
                false,
                "sensor latch not checked: give --latch-trials".into(),
            ),
            Some(Ok(done)) if done.status.success() => {}
            Some(Ok(done)) if skip_latch => {
                out(format_args!("sensor latch: exit {} ({why})", done.status))
            }
            Some(Ok(done)) => check(false, format!("sensor latch failed: exit {}", done.status)),
            Some(Err(e)) => check(false, format!("sensor latch did not finish: {e}")),
        }
    }
    if failed.is_empty() {
        out(format_args!(
            "{}",
            if lent {
                "chain proof: pass (lent path: the front end's frames are lent to the back end)"
            } else {
                "chain proof: pass (copy path: memfds, the lend refused; frames copied through the back end's input)"
            }
        ));
        Ok(true)
    } else {
        out(format_args!("chain proof: FAILED: {}", failed.join("; ")));
        Ok(false)
    }
}

/// Starts `lemnos-sensor-proof latch` beside this tool, with the same socket. Its output is read
/// when it finishes; only its own child is waited for.
fn spawn_latch(args: &Args, sensor: &str) -> Result<Option<Child>, String> {
    let Some(trials) = args.latch_trials else {
        return Ok(None);
    };
    let tool = std::env::current_exe()
        .map_err(|e| format!("current exe: {e}"))?
        .with_file_name("lemnos-sensor-proof");
    Command::new(&tool)
        .args([
            "latch",
            "--socket",
            &args.socket,
            "--sensor",
            sensor,
            "--trials",
        ])
        .arg(trials.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map(Some)
        .map_err(|e| format!("{}: {e}", tool.display()))
}

fn main() -> ExitCode {
    let Some(args) = parse() else {
        out(format_args!("{USAGE}"));
        return ExitCode::from(2);
    };
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            out(format_args!("lemnos-chain-proof: {e}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_must_be_within_two_percent_and_an_unset_rate_is_not_checked() {
        assert!(rate_ok(114.8, 115.0));
        assert!(!rate_ok(100.0, 115.0));
        assert!(!rate_ok(114.8, 0.0));
    }

    #[test]
    fn a_skip_is_named_and_only_the_three_exist() {
        for (flag, value, ok) in [
            ("--allow-skip", "lit-scene", true),
            ("--allow-skip", "latch", true),
            ("--allow-skip", "reference", true),
            ("--allow-skip", "fps", false),
            ("--allow-skip", "", false),
        ] {
            let mut args = vec!["lemnos-chain-proof".to_string()];
            args.extend([flag.into(), value.into()]);
            assert_eq!(parse_from(args).is_some(), ok, "{value}");
        }
        let all = parse_from(
            [
                "x",
                "--allow-skip",
                "latch",
                "--allow-skip",
                "reference",
                "--allow-skip",
                "lit-scene",
            ]
            .map(String::from)
            .to_vec(),
        )
        .unwrap();
        assert!(all.allow_skip_latch && all.allow_skip_reference && all.allow_skip_lit);
        let none = parse_from(vec!["x".to_string()]).unwrap();
        assert!(!none.allow_skip_latch && !none.allow_skip_reference && !none.allow_skip_lit);
    }

    #[test]
    fn a_timestamp_in_the_future_or_too_old_is_not_a_frame_start() {
        let now = 5_000_000_000;
        assert!(age_bad(now + 1, now));
        assert!(age_bad(now - MAX_AGE_NS - 1, now));
        assert!(!age_bad(now - 1_000_000, now));
        assert!(!age_bad(now, now));
    }
}
