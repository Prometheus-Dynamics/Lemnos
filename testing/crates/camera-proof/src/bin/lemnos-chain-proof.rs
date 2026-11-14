//! `lemnos-chain-proof`: the whole PiSP camera chain on a real board, through `lemnosd`'s socket.
//!
//! ```text
//! lemnos-chain-proof [--socket PATH] [--sensor NAME] [--receiver NAME] [--front-end NAME]
//!                    [--back-end NAME] [--frames N] [--frame-length N] [--pattern CODE]
//!                    [--reference FILE] [--jobs-queued N] [--fps RATE] [--latch-trials N]
//!                    [--min-mean-luma N] [--allow-skip lit-scene]
//! ```
//!
//! A check that is not made is a failure, named in the output: `--fps` (the sensor mode's frame
//! rate, within 2 %), `--reference` (the first frame's lines), `--latch-trials` (the sensor's latch,
//! run as `lemnos-sensor-proof latch` beside the chain, on the same socket). `--allow-skip lit-scene`
//! is the only skip: it lets the lit-scene checks (the back end's mean luma reaching
//! `--min-mean-luma`, and the latch) be skipped on a dark scene; the others are never skipped.
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
//! cannot resolve is an error naming what it looked for and what the board has. The front end's `output0` (16-bit Bayer, 2560 bytes a line) is copied, frame by
//! frame, into the back end's `input` buffers, and one back-end job per frame carries the front
//! end frame's identity, so the back end's frame names the same job. Each front-end frame is
//! released once its bytes are copied. Two front-end jobs are kept queued (`--jobs-queued`,
//! 1 to 3), and the back end takes two input buffers in turn.
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
use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_camera_proof::chain::{Stage, resolve};
use lemnos_client::{Client, FrameStream};
use lemnos_contract_camera_isp::{BayerOrder, CameraIsp, IspInput, IspJob, IspOutput, NO_BUFFER};
use lemnos_contract_camera_receiver::CameraReceiver;
use lemnos_contract_camera_sensor::{CameraSensor, Controls};
use lemnos_contract_isp_pisp::{LAYOUT_BACK_END, LAYOUT_FRONT_END, OUTPUT_FRONT_END_0};
use lemnos_core::{Encode, Writer};
use lemnos_driver_pisp::be::BackEnd;
use lemnos_driver_pisp::fe::FrontEnd;
use lemnos_driver_pisp::format::{compute_stride_align, formats};
use lemnos_driver_pisp::uapi::{
    AGC_STATS_NUM_ZONES, BayerOrder as Order, BeCcmConfig, BeTilesConfig, Branch, FeAgcStatsConfig,
    FeAwbStatsConfig, FeConfig, FeRgbyConfig, ImageFormatConfig, Wire, fe_enable as en,
};
use lemnos_linux_stream::LinuxStreams;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
const RAW10_STRIDE: usize = 1600;
/// RAW10's black level, moved to 16 bits.
const BLACK: u16 = 64 << 6;
/// Full-range BT.601, as the isp proof uses.
const CSC: BeCcmConfig = BeCcmConfig {
    coeffs: [306, 601, 117, -173, -339, 512, 512, -429, -83],
    pad: [0; 2],
    offsets: [0, 33_554_432, 33_554_432],
};
const USAGE: &str = "usage: lemnos-chain-proof [--socket PATH] [--sensor NAME] [--receiver NAME] [--front-end NAME] [--back-end NAME] [--frames N] [--frame-length N] [--pattern CODE] [--reference FILE] [--jobs-queued N] [--fps RATE] [--latch-trials N] [--min-mean-luma N] [--allow-skip lit-scene]";
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
    stamp > now || now - stamp > MAX_AGE_NS
}

struct Args {
    socket: String,
    /// The sensor the chain starts from. Not given: the board's one camera sensor.
    sensor: Option<String>,
    /// Each stage named on the command line instead of followed from the sensor's links.
    receiver: Option<String>,
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
}

fn parse() -> Option<Args> {
    let mut args = Args {
        socket: "/run/lemnos/lemnosd.sock".into(),
        sensor: None,
        receiver: None,
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
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next()?;
        match flag.as_str() {
            "--socket" => args.socket = value,
            "--sensor" => args.sensor = Some(value),
            "--receiver" => args.receiver = Some(value),
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
            "--allow-skip" if value == "lit-scene" => args.allow_skip_lit = true,
            _ => return None,
        }
    }
    Some(args)
}

fn out(text: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stdout(), "{text}");
}

fn now_ns() -> u64 {
    lemnos_linux_sys::time::monotonic_now()
        .map(|t| u64::try_from(t.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn percentile(sorted: &[u64], p: usize) -> u64 {
    sorted
        .get((sorted.len() * p / 100).min(sorted.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0)
}

fn order(bayer: BayerOrder) -> Order {
    match bayer {
        BayerOrder::Rggb => Order::Rggb,
        BayerOrder::Grbg => Order::Grbg,
        BayerOrder::Gbrg => Order::Gbrg,
        BayerOrder::Bggr => Order::Bggr,
        BayerOrder::Mono => Order::Greyscale,
    }
}

/// The front end's configuration: output 0 on, 16-bit; AWB, AGC and focus statistics on, every
/// pixel counted, as `lemnos-fe-proof` builds it.
fn front_config(bayer: Order) -> Result<Vec<u8>, String> {
    let mut fe = FrontEnd::new(WIDTH as u16, HEIGHT as u16, bayer);
    fe.set_output_format(
        Branch::Zero,
        ImageFormatConfig {
            format: formats::BAYER16,
            ..Default::default()
        },
    );
    fe.set_rgby(FeRgbyConfig {
        gain_r: 306,
        gain_g: 601,
        gain_b: 117,
        ..Default::default()
    });
    fe.set_awb_stats(FeAwbStatsConfig {
        r_hi: u16::MAX,
        g_hi: u16::MAX,
        b_hi: u16::MAX,
        ..Default::default()
    });
    fe.set_agc_stats(FeAgcStatsConfig {
        weights: [0x11; AGC_STATS_NUM_ZONES / 2],
        ..Default::default()
    });
    fe.enable(
        en::output(0) | en::AWB_STATS | en::RGBY | en::AGC_STATS | en::CDAF_STATS,
        true,
    );
    let cfg = fe
        .prepare()
        .map_err(|e| format!("the configuration library refused: {e:?}"))?;
    let mut bytes = vec![0u8; FeConfig::SIZE];
    cfg.write_to(&mut bytes)
        .map_err(|_| "configuration size".to_string())?;
    Ok(bytes)
}

/// The back end's configuration: Bayer to NV12 at the input size, as `lemnos-isp-proof` builds it.
fn back_config(input: ImageFormatConfig, bayer: BayerOrder) -> Result<BeTilesConfig, String> {
    BackEnd::simple_bayer(
        input,
        order(bayer),
        BLACK,
        (1.6, 1.0, 1.6),
        None,
        formats::NV12,
        Some(CSC),
    )
    .prepare()
    .map_err(|e| format!("the configuration library refused: {e:?}"))
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
        if stream.has_buffer_handles() {
            "dma-buf per buffer"
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
fn post_front(cfg: &mut FrameStream, identity: u64, buffer: u32) -> Result<(), String> {
    let job = IspJob {
        identity,
        layout: LAYOUT_FRONT_END,
        config_buffer: buffer,
        input_buffer: NO_BUFFER,
        outputs: [NO_BUFFER, NO_BUFFER, buffer, NO_BUFFER],
    };
    post_job(cfg, &job)
}

/// A back-end job reading input and config buffer `slot` and writing output 0 buffer `output`.
fn post_back(cfg: &mut FrameStream, identity: u64, slot: u32, output: u32) -> Result<(), String> {
    let job = IspJob {
        identity,
        layout: LAYOUT_BACK_END,
        config_buffer: slot,
        input_buffer: slot,
        outputs: [output, NO_BUFFER, NO_BUFFER, NO_BUFFER],
    };
    post_job(cfg, &job)
}

fn post_job(cfg: &mut FrameStream, job: &IspJob) -> Result<(), String> {
    let mut buf = [0u8; 128];
    let mut w = Writer::new(&mut buf);
    let n = job.encode(&mut w).map(|()| w.len()).unwrap_or(0);
    match cfg.post_job(buf.get(..n).unwrap_or(&[])) {
        Ok(true) => Ok(()),
        Ok(false) => Err("the job queue is full".into()),
        Err(e) => Err(format!("post: {e:?}")),
    }
}

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
    /// Frames whose timestamp is not a recent `CLOCK_MONOTONIC` frame start (see `age_bad`).
    age_bad: u64,
}

/// The back end's streams, the jobs in flight and what the frames show.
struct BackStage {
    cfg: FrameStream,
    input: FrameStream,
    out: FrameStream,
    /// Bytes of luma in one output 0 frame.
    luma_bytes: usize,
    track: Track,
    /// The identities of the jobs posted and not yet answered, oldest first.
    pending: VecDeque<u64>,
    latency: Vec<u64>,
    identity_bad: u64,
    zero_luma: u64,
    age_bad: u64,
    /// Luma summed over every frame, and the bytes it is summed over: the mean.
    luma_total: u64,
    luma_pixels: u64,
}

impl BackStage {
    /// Takes the oldest back-end frame, checks its identity against its job, and releases it.
    /// `Ok(false)` when no frame came for 2 s, after which the jobs in flight are given up.
    fn take(&mut self) -> Result<bool, String> {
        let Some(expected) = self.pending.pop_front() else {
            return Ok(true);
        };
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
        let now = now_ns();
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
    let be_slots = u64::from(back.input.layout().buffers.min(back.cfg.layout().buffers));
    let be_out_slots = u64::from(back.out.layout().buffers);
    let in_bytes = front.stride * HEIGHT as usize;
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
        front.track.see(frame.sequence(), frame.timestamp_ns());
        front.age_bad += u64::from(age_bad(frame.timestamp_ns(), now_ns()));
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
            if !back.take()? {
                stalled = true;
                break;
            }
        }
        if stalled {
            break;
        }
        let slot = (k % be_slots) as u32;
        let identity = frame.identity();
        let bytes = front.out.bytes(&frame);
        back.input
            .write(slot, 0, &bytes[..bytes.len().min(in_bytes)])
            .map_err(|e| format!("back end input write: {e:?}"))?;
        front
            .out
            .release(frame)
            .map_err(|e| format!("release: {e:?}"))?;
        post_back(&mut back.cfg, identity, slot, (k % be_out_slots) as u32)?;
        back.pending.push_back(identity);
    }
    while !back.pending.is_empty() {
        if !back.take()? {
            break;
        }
    }
    Ok(matched)
}

#[allow(clippy::too_many_lines)]
fn run(args: &Args) -> Result<bool, String> {
    if !(1..=3).contains(&args.jobs_queued) {
        return Err("--jobs-queued takes 1 to 3".into());
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
    let index = |name: &str| {
        list.iter()
            .find(|d| d.name == name)
            .map(|d| d.index)
            .ok_or(format!("no device {name}"))
    };
    let fe_index = index(front_name)?;
    let be_index = index(back_name)?;
    let mut sensor = client
        .open::<dyn CameraSensor>(sensor_name)
        .map_err(|e| format!("{sensor_name}: {e}"))?;
    let mut receiver = client
        .open::<dyn CameraReceiver>(receiver_name)
        .map_err(|e| format!("{receiver_name}: {e}"))?;
    let mut fe = client
        .open::<dyn CameraIsp>(front_name)
        .map_err(|e| format!("{front_name}: {e}"))?;
    let mut be = client
        .open::<dyn CameraIsp>(back_name)
        .map_err(|e| format!("{back_name}: {e}"))?;

    let _ = sensor.stop();
    let _ = fe.stop();
    let _ = be.stop();
    sensor
        .select_mode(0)
        .map_err(|s| format!("select_mode: {s:?}"))?;
    let mode = sensor.mode(0).map_err(|s| format!("mode: {s:?}"))?;
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
    if let Some(lines) = args.frame_length {
        sensor
            .set_controls(Controls {
                frame_length: Some(lines),
                ..Controls::default()
            })
            .map_err(|s| format!("set_controls: {s:?}"))?;
    }
    if let Some(code) = args.pattern {
        sensor
            .set_test_pattern(code)
            .map_err(|s| format!("set_test_pattern: {s:?}"))?;
    }

    let mut input = ImageFormatConfig {
        width: WIDTH as u16,
        height: HEIGHT as u16,
        format: formats::BAYER16,
        ..Default::default()
    };
    compute_stride_align(&mut input, 64);
    fe.set_input(IspInput {
        width: WIDTH,
        height: HEIGHT,
        bit_depth: 16,
        bayer: mode.bayer,
    })
    .map_err(|s| format!("front end set_input: {s:?}"))?;
    let fe_layout = fe
        .set_output(
            OUTPUT_FRONT_END_0,
            IspOutput {
                width: WIDTH,
                height: HEIGHT,
                fourcc: u32::from_le_bytes(*b"BYR2"),
                pad_bytes: 0,
            },
        )
        .map_err(|s| format!("front end set_output: {s:?}"))?;
    if i64::from(fe_layout.stride) != i64::from(input.stride) {
        return Err(format!(
            "front-end output stride {} is not the back end's input stride {}",
            fe_layout.stride, input.stride
        ));
    }
    be.set_input(IspInput {
        width: WIDTH,
        height: HEIGHT,
        bit_depth: 16,
        bayer: mode.bayer,
    })
    .map_err(|s| format!("back end set_input: {s:?}"))?;
    let be_layout = be
        .set_output(
            0,
            IspOutput {
                width: WIDTH,
                height: HEIGHT,
                fourcc: u32::from_le_bytes(*b"NV12"),
                pad_bytes: 0,
            },
        )
        .map_err(|s| format!("back end set_output: {s:?}"))?;
    let fe_bytes = front_config(order(mode.bayer))?;
    let be_config = back_config(input, mode.bayer)?;
    if i64::from(be_config.config.output_format[0].image.stride) != i64::from(be_layout.stride) {
        return Err(format!(
            "back-end configuration's output stride {} is not its layout's {}",
            be_config.config.output_format[0].image.stride, be_layout.stride
        ));
    }
    let mut be_bytes = vec![0u8; BeTilesConfig::SIZE];
    be_config
        .write_to(&mut be_bytes)
        .map_err(|_| "configuration size".to_string())?;
    out(format_args!(
        "front end: output 0 stride {} size {}; back end: input stride {}, output 0 stride {} size {}, {} tiles",
        fe_layout.stride,
        fe_layout.size,
        input.stride,
        be_layout.stride,
        be_layout.size,
        be_config.num_tiles
    ));

    let platform = Arc::new(LinuxStreams);
    let mut front = FrontStage {
        cfg: client
            .open_stream_writable(fe_index, "config", platform.clone())
            .map_err(|e| format!("front end config stream: {e}"))?,
        stats: client
            .open_stream(fe_index, "statistics", platform.clone())
            .map_err(|e| format!("front end statistics stream: {e}"))?,
        out: client
            .open_stream(fe_index, "output0", platform.clone())
            .map_err(|e| format!("front end output0 stream: {e}"))?,
        stride: fe_layout.stride as usize,
        track: Track::default(),
        identity_bad: 0,
        stats_bad: 0,
        stats_missing: 0,
        age_bad: 0,
    };
    let mut back = BackStage {
        cfg: client
            .open_stream_writable(be_index, "config", platform.clone())
            .map_err(|e| format!("back end config stream: {e}"))?,
        input: client
            .open_stream_writable(be_index, "input", platform.clone())
            .map_err(|e| format!("back end input stream: {e}"))?,
        out: client
            .open_stream(be_index, "output0", platform)
            .map_err(|e| format!("back end output0 stream: {e}"))?,
        luma_bytes: be_layout.stride as usize * HEIGHT as usize,
        track: Track::default(),
        pending: VecDeque::new(),
        latency: Vec::new(),
        identity_bad: 0,
        zero_luma: 0,
        age_bad: 0,
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
            [
                ("config", &back.cfg),
                ("input", &back.input),
                ("output0", &back.out),
            ]
            .as_slice(),
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
    }
    // Every buffer of each configuration stream holds the configuration: a job names one of them.
    for buffer in 0..front.out.layout().buffers {
        front
            .cfg
            .write(buffer, 0, &fe_bytes)
            .map_err(|e| format!("front end config write: {e:?}"))?;
    }
    for buffer in 0..back.cfg.layout().buffers {
        back.cfg
            .write(buffer, 0, &be_bytes)
            .map_err(|e| format!("back end config write: {e:?}"))?;
    }
    let reference = match &args.reference {
        Some(path) => Some(std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?),
        None => None,
    };

    be.start().map_err(|s| format!("back end start: {s:?}"))?;
    fe.start().map_err(|s| format!("front end start: {s:?}"))?;
    sensor.start().map_err(|s| format!("sensor start: {s:?}"))?;
    // The sensor's latch (`lemnos-sensor-proof latch`) runs beside the chain, as its own process.
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
    // Back end first, then the front end. The sensor and the receiver are left running.
    let mut stop_errors = Vec::new();
    if let Err(s) = be.stop() {
        stop_errors.push(format!("back end stop: {s:?}"));
    }
    if let Err(s) = fe.stop() {
        stop_errors.push(format!("front end stop: {s:?}"));
    }
    let mut failed = Vec::new();
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
        "back-end mean luma {mean_luma}; timestamps not a recent frame start: front end {}, back end {}",
        front.age_bad, back.age_bad
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
    check(
        matched == Some(HEIGHT as usize),
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
            "timestamps not a recent CLOCK_MONOTONIC frame start: front end {}, back end {}",
            front.age_bad, back.age_bad
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
    match latch {
        None if args.allow_skip_lit => out(format_args!(
            "sensor latch: not run (lit-scene check skipped)"
        )),
        None => check(
            false,
            "sensor latch not checked: give --latch-trials".into(),
        ),
        Some(Ok(done)) if done.status.success() => {}
        Some(Ok(done)) if args.allow_skip_lit => out(format_args!(
            "sensor latch: exit {} (lit-scene check skipped)",
            done.status
        )),
        Some(Ok(done)) => check(false, format!("sensor latch failed: exit {}", done.status)),
        Some(Err(e)) => check(false, format!("sensor latch did not finish: {e}")),
    }
    if failed.is_empty() {
        out(format_args!("chain proof: pass"));
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
    fn a_timestamp_in_the_future_or_too_old_is_not_a_frame_start() {
        let now = 5_000_000_000;
        assert!(age_bad(now + 1, now));
        assert!(age_bad(now - MAX_AGE_NS - 1, now));
        assert!(!age_bad(now - 1_000_000, now));
        assert!(!age_bad(now, now));
    }
}
