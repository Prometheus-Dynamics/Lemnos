//! `lemnos-fe-proof`: the PiSP front end on a real board, through `lemnosd`'s socket.
//!
//! ```text
//! lemnos-fe-proof [--socket PATH] [--fe ID] [--sensor ID] [--pattern CODE] [--frames N]
//!                 [--frame-length N] [--reference FILE] [--save DIR] [--output1 1]
//!                 [--late N] [--bad 1] [--stop-cycles N]
//! ```
//!
//! The sensor streams into the front end, which writes output 0 (the sensor's 10-bit pixels
//! unpacked to 16 bits) and the AWB, AGC and focus statistics into the client's buffers. The tool
//! writes one configuration built with the configuration library into every `config` buffer and
//! keeps one job queued per buffer: job `n` names config, statistics and output buffer `n % 4`,
//! and when its frame and statistics have been read and released, the next job on that buffer is
//! posted. It prints:
//!
//! - the frames published on `output0` and on `statistics`, and whether each statistics frame
//!   carries the identity of the output frame it came with;
//! - the frame rate from output 0's timestamps, and the frame starts no job was queued for
//!   (intervals over one and a half periods);
//! - the AWB means and the AGC histogram's mean of the last frame;
//! - with `--reference` (one packed RAW10 frame the receiver captured of the same pattern), how
//!   many of output 0's lines in the first frame equal the reference's, value for value;
//! - stop with every job on the block: how long it takes, then a start again and 30 frames, to
//!   show nothing was left wedged;
//! - the front end's counters.
//!
//! With `--output1 1`, output 1 is on too, at half size through the Bayer downscaler, on the
//! `output1` stream: every job names its buffer, the run counts output 1 frames with the output
//! frame's identity, and the first one is compared with a software halving of output 0 (each
//! output pixel the mean of the four input pixels of its colour in its 4x4 block). The block's
//! filter is not exactly that mean (it reaches neighbouring pixels of the colour), so the check is
//! that 99 % of the pixels are within one 10-bit step of it.
//!
//! With `--late N`, after the main run, N frames with one job posted per two frames: the next job
//! goes in one frame period after a frame arrives. It prints the counters' `dropped` and the
//! block's `stalled` across that, and, with a pattern, how many frames differ from the main run's
//! first frame.
//!
//! With `--bad 1`, configurations the driver must refuse are posted, three with the block running
//! and three while it is stopped and before it starts again: an address field set, `ilines` set,
//! and output 0's stride larger than the one set. Each must be counted as dropped, publish
//! nothing, count no error, and the start after must stream.
//!
//! With `--stop-cycles N`, N times: four jobs posted, a stop while the block is writing, a start,
//! and three frames, with the stop's time kept.
//!
//! After everything, the bytes past each `output0` (and `output1`) buffer's frame, up to
//! [`GUARD`], must still be zero as the heap gave them: the board file gives the buffers that
//! room. The first frame of the main run prints `streaming` when it arrives, so a script can
//! kill the client, or `lemnosd`, after it.
//!
//! With `--save` and `--output1 1`, the first frame's output 0 and output 1 as they came, too.
//!
//! With `--frame-length`, the sensor's frame length (lines) is set before the start, and the
//! mode's line time and frame length limits are printed, so the period can be compared with them.
//! With `--save`, the first output 0 frame's top 8 bits as a PGM.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_client::{Client, FrameStream, HeldFrame};
use lemnos_contract_camera_isp::{BayerOrder, CameraIsp, IspInput, IspJob, IspOutput, NO_BUFFER};
use lemnos_contract_camera_sensor::{CameraSensor, Controls};
use lemnos_contract_isp_pisp::{IspPisp, LAYOUT_FRONT_END, OUTPUT_FRONT_END_0, OUTPUT_FRONT_END_1};
use lemnos_core::{Encode, Writer};
use lemnos_driver_pisp::fe::FrontEnd;
use lemnos_driver_pisp::format::formats;
use lemnos_driver_pisp::stats::Statistics;
use lemnos_driver_pisp::uapi::{
    BayerOrder as Order, Branch, FE_DOWNSCALE_BAYER, FeAgcStatsConfig, FeAwbStatsConfig, FeConfig,
    FeDownscaleConfig, FeRgbyConfig, ImageFormatConfig, Wire, fe_enable as en,
};
use lemnos_linux_stream::LinuxStreams;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
const RAW10_STRIDE: usize = 1600;
/// Buffers on every stream, and jobs kept queued.
const BUFFERS: u32 = 4;
/// Bytes past each output frame that must stay as the heap gave them (zero).
const GUARD: usize = 4096;

struct Args {
    socket: String,
    fe: String,
    sensor: String,
    pattern: Option<u16>,
    frames: u32,
    frame_length: Option<u32>,
    reference: Option<PathBuf>,
    save: Option<PathBuf>,
    output1: bool,
    late: u32,
    bad: bool,
    stop_cycles: u32,
}

fn parse() -> Option<Args> {
    let mut args = Args {
        socket: "/run/lemnos/lemnosd.sock".into(),
        fe: "cam0-fe".into(),
        sensor: "cam0-sensor".into(),
        pattern: None,
        frames: 3600,
        frame_length: None,
        reference: None,
        save: None,
        output1: false,
        late: 0,
        bad: false,
        stop_cycles: 0,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next()?;
        match flag.as_str() {
            "--socket" => args.socket = value,
            "--fe" => args.fe = value,
            "--sensor" => args.sensor = value,
            "--pattern" => args.pattern = Some(value.parse().ok()?),
            "--frames" => args.frames = value.parse().ok()?,
            "--frame-length" => args.frame_length = Some(value.parse().ok()?),
            "--reference" => args.reference = Some(PathBuf::from(value)),
            "--save" => args.save = Some(PathBuf::from(value)),
            "--output1" => args.output1 = value != "0",
            "--late" => args.late = value.parse().ok()?,
            "--bad" => args.bad = value != "0",
            "--stop-cycles" => args.stop_cycles = value.parse().ok()?,
            _ => return None,
        }
    }
    Some(args)
}

fn out(text: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stdout(), "{text}");
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

/// Output 0 on, 16-bit; AWB, AGC (through RGB to Y) and focus statistics on, every pixel counted.
/// With `output1`, output 1 too, 16-bit at half size through the Bayer downscaler.
fn configuration(bayer: Order, output1: bool) -> Result<Vec<u8>, String> {
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
        weights: [0x11; lemnos_driver_pisp::uapi::AGC_STATS_NUM_ZONES / 2],
        ..Default::default()
    });
    fe.enable(
        en::output(0) | en::AWB_STATS | en::RGBY | en::AGC_STATS | en::CDAF_STATS,
        true,
    );
    if output1 {
        fe.set_downscale(
            Branch::One,
            FeDownscaleConfig {
                xin: 2,
                xout: 1,
                yin: 2,
                yout: 1,
                flags: FE_DOWNSCALE_BAYER,
                ..Default::default()
            },
        );
        fe.set_output_format(
            Branch::One,
            ImageFormatConfig {
                format: formats::BAYER16,
                ..Default::default()
            },
        );
        fe.enable(en::output(1) | en::downscale(1), true);
    }
    let cfg = fe
        .prepare()
        .map_err(|e| format!("the configuration library refused: {e:?}"))?;
    let mut bytes = vec![0u8; lemnos_driver_pisp::uapi::FeConfig::SIZE];
    cfg.write_to(&mut bytes)
        .map_err(|_| "configuration size".to_string())?;
    Ok(bytes)
}

/// A job on config, statistics and output buffer `buffer`.
fn post_on(
    config: &mut FrameStream,
    identity: u64,
    buffer: u32,
    output1: bool,
) -> Result<(), String> {
    let job = IspJob {
        identity,
        layout: LAYOUT_FRONT_END,
        config_buffer: buffer,
        input_buffer: NO_BUFFER,
        outputs: [
            NO_BUFFER,
            NO_BUFFER,
            buffer,
            if output1 { buffer } else { NO_BUFFER },
        ],
    };
    let mut buf = [0u8; 128];
    let mut w = Writer::new(&mut buf);
    let n = job.encode(&mut w).map(|()| w.len()).unwrap_or(0);
    match config.post_job(buf.get(..n).unwrap_or(&[])) {
        Ok(true) => Ok(()),
        Ok(false) => Err("the job queue is full".into()),
        Err(e) => Err(format!("post: {e:?}")),
    }
}

/// One RAW10 line's pixels, unpacked to their 10-bit values.
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

/// One output 0 line's pixels, as stored: 16-bit little-endian.
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

/// Output 1's pixels against a software halving of output 0: each output pixel the mean of the
/// four input pixels of its colour in its 4x4 block. Returns (pixels, exactly equal, within one
/// 10-bit step (64), largest difference).
fn halving(out0: &[u8], stride0: usize, out1: &[u8], stride1: usize) -> (u64, u64, u64, u16) {
    let (w1, h1) = (WIDTH as usize / 2, HEIGHT as usize / 2);
    let at = |image: &[u8], stride: usize, x: usize, y: usize| {
        let i = y * stride + x * 2;
        image
            .get(i..i + 2)
            .map_or(0, |p| u32::from(u16::from_le_bytes([p[0], p[1]])))
    };
    let (mut n, mut equal, mut near, mut worst) = (0u64, 0u64, 0u64, 0u16);
    for y in 0..h1 {
        for x in 0..w1 {
            let (bx, by) = (4 * (x / 2) + x % 2, 4 * (y / 2) + y % 2);
            let sum = at(out0, stride0, bx, by)
                + at(out0, stride0, bx + 2, by)
                + at(out0, stride0, bx, by + 2)
                + at(out0, stride0, bx + 2, by + 2);
            let want = sum / 4;
            let got = at(out1, stride1, x, y);
            let diff = u16::try_from(want.abs_diff(got)).unwrap_or(u16::MAX);
            n += 1;
            equal += u64::from(diff == 0);
            near += u64::from(diff <= 64);
            worst = worst.max(diff);
        }
    }
    (n, equal, near, worst)
}

/// Whether the [`GUARD`] bytes past `size` in every buffer of `stream` are still zero. `None`
/// when the buffers have no such room.
fn guard_clean(stream: &FrameStream, size: usize) -> Option<bool> {
    let mut tail = vec![0u8; GUARD];
    let mut clean = true;
    for buffer in 0..BUFFERS {
        stream.read(buffer, size, &mut tail).ok()?;
        clean &= tail.iter().all(|&b| b == 0);
    }
    Some(clean)
}

struct Streams {
    cfg: FrameStream,
    output: FrameStream,
    stats: FrameStream,
    out1: Option<FrameStream>,
}

struct Run {
    frames: u32,
    stats: u32,
    matched: u32,
    out1: u32,
    out1_matched: u32,
    /// Frames whose output 0 differs from `expect`.
    differ: u32,
    unseen: u32,
    fps: f64,
    period_ns: u64,
    last: Statistics,
}

/// Reads `frames` frames with their statistics (and output 1), posting the next job on each
/// output buffer as its frame comes back. With `late`, one job at a time, posted that long after
/// the frame before arrives, and the sensor's frame period is `late` (frame starts with no job
/// are counted against it rather than against the shortest interval seen). `first` sees the first frame before it is released; `each` runs
/// after every frame with how many have come.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn stream(
    s: &mut Streams,
    identity: &mut u64,
    frames: u32,
    late: Option<Duration>,
    expect: Option<&[u8]>,
    mut first: impl FnMut(&Streams, &HeldFrame, Option<&HeldFrame>),
    mut each: impl FnMut(u32, &mut FrameStream) -> Result<(), String>,
) -> Result<Run, String> {
    let output1 = s.out1.is_some();
    let mut run = Run {
        frames: 0,
        stats: 0,
        matched: 0,
        out1: 0,
        out1_matched: 0,
        differ: 0,
        unseen: 0,
        fps: 0.0,
        period_ns: 0,
        last: Statistics::default(),
    };
    let mut posted = 0u32;
    let initial = if late.is_some() { 1 } else { BUFFERS };
    for buffer in 0..initial.min(frames) {
        post_on(&mut s.cfg, *identity, buffer, output1)?;
        *identity += 1;
        posted += 1;
    }
    let mut stamps = Vec::with_capacity(frames as usize);
    while run.frames < frames {
        let Some(frame) = s
            .output
            .next_frame(Duration::from_secs(2))
            .map_err(|e| format!("output0: {e:?}"))?
        else {
            out(format_args!(
                "no output frame for 2 s after {} frames",
                run.frames
            ));
            break;
        };
        let second = match s.out1.as_mut() {
            Some(o) => o
                .next_frame(Duration::from_millis(200))
                .map_err(|e| format!("output1: {e:?}"))?,
            None => None,
        };
        if run.frames == 0 {
            first(s, &frame, second.as_ref());
        }
        if let Some(want) = expect
            && s.output.bytes(&frame) != want
        {
            run.differ += 1;
        }
        stamps.push(frame.timestamp_ns());
        let id = frame.identity();
        let buffer = frame.buffer();
        if let (Some(o), Some(f)) = (s.out1.as_mut(), second) {
            run.out1 += 1;
            run.out1_matched += u32::from(f.identity() == id);
            o.release(f).map_err(|e| format!("release: {e:?}"))?;
        }
        s.output
            .release(frame)
            .map_err(|e| format!("release: {e:?}"))?;
        run.frames += 1;
        if let Some(st) = s
            .stats
            .next_frame(Duration::from_millis(200))
            .map_err(|e| format!("statistics: {e:?}"))?
        {
            run.stats += 1;
            if st.identity() == id {
                run.matched += 1;
            }
            if run.frames == frames {
                let _ = run.last.parse_into(s.stats.bytes(&st));
            }
            s.stats.release(st).map_err(|e| format!("release: {e:?}"))?;
        }
        each(run.frames, &mut s.cfg)?;
        if posted < frames {
            if let Some(wait) = late {
                std::thread::sleep(wait);
            }
            post_on(&mut s.cfg, *identity, buffer % BUFFERS, output1)?;
            *identity += 1;
            posted += 1;
        }
    }
    let intervals: Vec<u64> = stamps
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]))
        .collect();
    let mut sorted = intervals.clone();
    sorted.sort_unstable();
    let shortest = sorted.first().copied().unwrap_or(0);
    let period = late.map_or(shortest, |wait| {
        u64::try_from(wait.as_nanos()).unwrap_or(shortest)
    });
    run.period_ns = period;
    if period > 0 {
        run.unseen = intervals
            .iter()
            .map(|&i| {
                u32::try_from((i + period / 2) / period)
                    .unwrap_or(1)
                    .saturating_sub(1)
            })
            .sum();
        let span = stamps.last().copied().unwrap_or(0) - stamps.first().copied().unwrap_or(0);
        run.fps = f64::from(run.frames - 1) * 1e9 / span.max(1) as f64;
    }
    Ok(run)
}

/// `good` with one thing the driver must refuse: 0 an address field, 1 `ilines`, 2 output 0's
/// stride one alignment step larger than the one set.
fn spoiled(good: &[u8], which: u32) -> Result<Vec<u8>, String> {
    let mut cfg = FeConfig::default();
    cfg.read_from(good)
        .map_err(|_| "configuration size".to_string())?;
    match which {
        0 => cfg.stats_buffer.addr_lo = 16,
        1 => cfg.ch[0].output.ilines = 1,
        _ => cfg.ch[0].output.format.stride += 64,
    }
    let mut bytes = vec![0u8; FeConfig::SIZE];
    cfg.write_to(&mut bytes)
        .map_err(|_| "configuration size".to_string())?;
    Ok(bytes)
}

const SPOILED: [&str; 3] = ["an address field", "ilines", "a larger output 0 stride"];
/// Config (and statistics) buffers the refused jobs name: none of the good jobs' buffers.
const BAD_BUFFER: u32 = BUFFERS;

/// Posts the three refused configurations, on config buffers [`BAD_BUFFER`] and up.
fn post_spoiled(cfg: &mut FrameStream, good: &[u8], identity: &mut u64) -> Result<(), String> {
    for which in 0..3 {
        let buffer = BAD_BUFFER + which;
        cfg.write(buffer, 0, &spoiled(good, which)?)
            .map_err(|e| format!("config write {buffer}: {e:?}"))?;
        let job = IspJob {
            identity: *identity,
            layout: LAYOUT_FRONT_END,
            config_buffer: buffer,
            input_buffer: NO_BUFFER,
            outputs: [NO_BUFFER, NO_BUFFER, 0, NO_BUFFER],
        };
        *identity += 1;
        let mut buf = [0u8; 128];
        let mut w = Writer::new(&mut buf);
        let n = job.encode(&mut w).map(|()| w.len()).unwrap_or(0);
        match cfg.post_job(buf.get(..n).unwrap_or(&[])) {
            Ok(true) => {}
            Ok(false) => return Err("the job queue is full".into()),
            Err(e) => return Err(format!("post: {e:?}")),
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn run(args: &Args) -> Result<bool, String> {
    let client = Client::connect(&args.socket, "lemnos-fe-proof")
        .map_err(|e| format!("{}: {e}", args.socket))?;
    client.set_timeout(Duration::from_secs(5));
    let list = client.list().map_err(|e| format!("list: {e}"))?;
    let fe_index = list
        .iter()
        .find(|d| d.name == args.fe)
        .map(|d| d.index)
        .ok_or(format!("no device {}", args.fe))?;
    let mut sensor = client
        .open::<dyn CameraSensor>(&args.sensor)
        .map_err(|e| format!("{}: {e}", args.sensor))?;
    let mut fe = client
        .open::<dyn CameraIsp>(&args.fe)
        .map_err(|e| format!("{}: {e}", args.fe))?;
    let mut pisp = client
        .open::<dyn IspPisp>(&args.fe)
        .map_err(|e| format!("{}: {e}", args.fe))?;
    let info = pisp.info().map_err(|s| format!("info: {s:?}"))?;
    out(format_args!("front end: {info:?}"));

    let _ = sensor.stop();
    let _ = fe.stop();
    sensor
        .select_mode(0)
        .map_err(|s| format!("select_mode: {s:?}"))?;
    let mode = sensor.mode(0).map_err(|s| format!("mode: {s:?}"))?;
    out(format_args!(
        "sensor mode 0: {}x{}, data type 0x{:02x}, {} lanes at {} Mbit/s, {:?}",
        mode.link.width,
        mode.link.height,
        mode.link.data_type,
        mode.link.lanes,
        mode.link.link_rate_mbps,
        mode.bayer
    ));
    out(format_args!(
        "mode 0 timing: line length {}, line time {} ps, frame length {}..={}, max {} mfps",
        mode.line_length,
        mode.line_time_ps,
        mode.frame_length_min,
        mode.frame_length_max,
        mode.max_fps_milli
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
    fe.set_input(IspInput {
        width: WIDTH,
        height: HEIGHT,
        bit_depth: 16,
        bayer: mode.bayer,
    })
    .map_err(|s| format!("set_input: {s:?}"))?;
    let raw16 = |width, height| IspOutput {
        width,
        height,
        fourcc: u32::from_le_bytes(*b"BYR2"),
        pad_bytes: 0,
    };
    let layout = fe
        .set_output(OUTPUT_FRONT_END_0, raw16(WIDTH, HEIGHT))
        .map_err(|s| format!("set_output: {s:?}"))?;
    let stride = layout.stride as usize;
    out(format_args!(
        "output 0: stride {} size {}",
        layout.stride, layout.size
    ));
    let layout1 = if args.output1 {
        let l = fe
            .set_output(OUTPUT_FRONT_END_1, raw16(WIDTH / 2, HEIGHT / 2))
            .map_err(|s| format!("set_output 1: {s:?}"))?;
        out(format_args!(
            "output 1: stride {} size {}",
            l.stride, l.size
        ));
        Some(l)
    } else {
        None
    };
    let config = configuration(order(mode.bayer), args.output1)?;

    let platform = Arc::new(LinuxStreams);
    let mut s = Streams {
        cfg: client
            .open_stream_writable(fe_index, "config", platform.clone())
            .map_err(|e| format!("config stream: {e}"))?,
        stats: client
            .open_stream(fe_index, "statistics", platform.clone())
            .map_err(|e| format!("statistics stream: {e}"))?,
        output: client
            .open_stream(fe_index, "output0", platform.clone())
            .map_err(|e| format!("output0 stream: {e}"))?,
        out1: if args.output1 {
            Some(
                client
                    .open_stream(fe_index, "output1", platform)
                    .map_err(|e| format!("output1 stream: {e}"))?,
            )
        } else {
            None
        },
    };
    for buffer in 0..BUFFERS {
        s.cfg
            .write(buffer, 0, &config)
            .map_err(|e| format!("config write: {e:?}"))?;
    }
    fe.start().map_err(|s| format!("start: {s:?}"))?;
    sensor.start().map_err(|s| format!("sensor start: {s:?}"))?;

    let reference = match &args.reference {
        Some(path) => Some(std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?),
        None => None,
    };
    let mut equal_lines = None;
    let mut zero_frame = false;
    let mut halved = None;
    let mut first_image = Vec::new();
    let mut identity = 1u64;
    let started = Instant::now();
    let main = stream(
        &mut s,
        &mut identity,
        args.frames,
        None,
        None,
        |streams, frame, second| {
            out(format_args!(
                "streaming: first frame after {} ms",
                started.elapsed().as_millis()
            ));
            let image = streams.output.bytes(frame);
            zero_frame = image.iter().all(|&b| b == 0);
            first_image = image.to_vec();
            if let (Some(o), Some(f), Some(l)) = (streams.out1.as_ref(), second, layout1) {
                halved = Some(halving(image, stride, o.bytes(f), l.stride as usize));
                if let Some(dir) = &args.save {
                    for (name, bytes) in [("fe-output0.raw", image), ("fe-output1.raw", o.bytes(f))]
                    {
                        let path = dir.join(name);
                        match std::fs::write(&path, bytes) {
                            Ok(()) => out(format_args!("saved {}", path.display())),
                            Err(e) => out(format_args!("{}: {e}", path.display())),
                        }
                    }
                }
            }
            if let Some(raw) = &reference {
                let equal = (0..HEIGHT as usize)
                    .filter(|&row| {
                        let want = raw10_line(raw, row);
                        let got = out_line(image, stride, row);
                        want.len() == WIDTH as usize
                            && want.iter().zip(&got).all(|(w, g)| *g == w << 6)
                    })
                    .count();
                let row = HEIGHT as usize / 2;
                let sample: Vec<(u16, u16)> = raw10_line(raw, row)
                    .iter()
                    .zip(out_line(image, stride, row))
                    .step_by(160)
                    .map(|(w, g)| (*w, g))
                    .collect();
                out(format_args!(
                    "first frame, identity {}: (reference, output) every 160th pixel of the middle line {sample:?}",
                    frame.identity()
                ));
                equal_lines = Some(equal);
            }
            if let Some(dir) = &args.save {
                let mut pgm = format!("P5\n{WIDTH} {HEIGHT}\n255\n").into_bytes();
                for row in 0..HEIGHT as usize {
                    pgm.extend(out_line(image, stride, row).iter().map(|v| (v >> 8) as u8));
                }
                let path = dir.join("fe-output0.pgm");
                match std::fs::write(&path, pgm) {
                    Ok(()) => out(format_args!("saved {}", path.display())),
                    Err(e) => out(format_args!("{}: {e}", path.display())),
                }
            }
        },
        |_, _| Ok(()),
    )?;
    out(format_args!(
        "{} frames in {:.1} s: {:.3} fps from timestamps, {} frame starts with no job, {} statistics frames, {} with the output frame's identity, all-zero first frame: {zero_frame}",
        main.frames,
        started.elapsed().as_secs_f64(),
        main.fps,
        main.unseen,
        main.stats,
        main.matched
    ));
    let mut ok = main.frames == args.frames
        && main.stats == main.frames
        && main.matched == main.frames
        && !zero_frame;
    if args.output1 {
        out(format_args!(
            "output 1: {} frames, {} with the output frame's identity",
            main.out1, main.out1_matched
        ));
        ok &= main.out1 == main.frames && main.out1_matched == main.frames;
        match halved {
            Some((n, equal, near, worst)) => {
                out(format_args!(
                    "output 1 against output 0 halved in software: {n} pixels, {equal} equal, {near} within 64 (one 10-bit step), largest difference {worst}"
                ));
                // Measured on the board: the block's downscaler is not the plain mean. On the
                // colour bars it differs only at the bars' edges (it reaches a neighbouring pixel
                // of the colour), and on a scene 99.85 % of pixels are within one step.
                ok &= near * 100 >= n * 99;
            }
            None => {
                out(format_args!("output 1: no first frame to compare"));
                ok = false;
            }
        }
    }
    if let Some(equal) = equal_lines {
        out(format_args!(
            "first frame: {equal} of {HEIGHT} lines equal the reference's, each value moved up 6 bits"
        ));
        ok &= equal == HEIGHT as usize;
    }
    let awb = main.last.awb_total();
    out(format_args!(
        "last statistics: AWB {} pixels counted, means {:?}; AGC histogram {} pixels, mean bin {:?}",
        awb.counted,
        awb.mean(),
        main.last.histogram_count(),
        main.last.histogram_mean()
    ));
    ok &= awb.counted > 0;
    let counters = fe.counters().map_err(|s| format!("counters: {s:?}"))?;
    let info = pisp.info().map_err(|s| format!("info: {s:?}"))?;
    out(format_args!(
        "front end counters: {counters:?}, stalled {}",
        info.stalled
    ));
    ok &= counters.errors == 0;

    if args.late > 0 {
        // One job per two frames: each posted a frame period after the frame before arrived.
        let before = fe.counters().map_err(|s| format!("counters: {s:?}"))?;
        let stalled = pisp.info().map_err(|s| format!("info: {s:?}"))?.stalled;
        let wait = Duration::from_nanos(main.period_ns);
        let expect = args.pattern.map(|_| first_image.as_slice());
        let late = stream(
            &mut s,
            &mut identity,
            args.late,
            Some(wait),
            expect,
            |_, _, _| {},
            |_, _| Ok(()),
        )?;
        let after = fe.counters().map_err(|s| format!("counters: {s:?}"))?;
        let stalled_after = pisp.info().map_err(|s| format!("info: {s:?}"))?.stalled;
        out(format_args!(
            "late posting, each job {} us after the frame before: {} frames, {} statistics, {} frame starts with no job from the timestamps, {:.3} fps, {} frames differ from the first; dropped +{}, errors +{}, stalled +{}",
            wait.as_micros(),
            late.frames,
            late.stats,
            late.unseen,
            late.fps,
            late.differ,
            after.dropped - before.dropped,
            after.errors - before.errors,
            stalled_after - stalled
        ));
        ok &= late.frames == args.late
            && late.stats == late.frames
            && late.differ == 0
            && after.errors == before.errors
            && after.dropped - before.dropped >= u64::from(late.unseen);
    }

    if args.bad {
        // Running: the refused jobs go in between good ones, which keep the block busy.
        let mut window = (None, None);
        // Refused jobs never come back, so their identities only need to be their own.
        let mut bad_identity = 1u64 << 40;
        let bad = stream(
            &mut s,
            &mut identity,
            60,
            None,
            None,
            |_, _, _| {},
            |n, cfg| {
                match n {
                    10 => window.0 = Some(fe.counters().map_err(|s| format!("{s:?}"))?),
                    11 => post_spoiled(cfg, &config, &mut bad_identity)?,
                    50 => window.1 = Some(fe.counters().map_err(|s| format!("{s:?}"))?),
                    _ => {}
                }
                Ok(())
            },
        )?;
        let (Some(a), Some(b)) = window else {
            return Err("the running run ended early".into());
        };
        out(format_args!(
            "refused configurations ({}) while running: dropped +{}, errors +{}, jobs +{}; {} frames, {} frame starts with no job in the whole run",
            SPOILED.join(", "),
            b.dropped - a.dropped,
            b.errors - a.errors,
            b.jobs - a.jobs,
            bad.frames,
            bad.unseen
        ));
        ok &= bad.frames == 60 && b.dropped - a.dropped == 3 && b.errors == a.errors;
        // Stopped: posted before the start, read when the driver takes them after it.
        fe.stop().map_err(|s| format!("stop: {s:?}"))?;
        drain(&mut s)?;
        post_spoiled(&mut s.cfg, &config, &mut identity)?;
        fe.start().map_err(|s| format!("start after: {s:?}"))?;
        let mut seen = None;
        let idle = stream(
            &mut s,
            &mut identity,
            30,
            None,
            None,
            |_, _, _| {},
            |n, _| {
                if n == 20 {
                    seen = Some(fe.counters().map_err(|s| format!("{s:?}"))?);
                }
                Ok(())
            },
        )?;
        let seen = seen.ok_or("the start after the refused jobs streamed less than 20 frames")?;
        out(format_args!(
            "refused configurations posted while stopped, then a start: {} frames after it, counters at frame 20 {seen:?}, {} frame starts with no job",
            idle.frames, idle.unseen
        ));
        ok &= idle.frames == 30 && seen.dropped == 3 + u64::from(idle.unseen) && seen.errors == 0;
    }

    // Stop with jobs on the block, then start again.
    let cycles = args.stop_cycles.max(1);
    let mut stops = Vec::with_capacity(cycles as usize);
    let mut restarted = 0u32;
    for cycle in 0..cycles {
        for buffer in 0..BUFFERS {
            post_on(&mut s.cfg, identity, buffer, args.output1)?;
            identity += 1;
        }
        // Somewhere within a frame or two, so the stop lands at different points of one.
        std::thread::sleep(Duration::from_millis(u64::from(5 + cycle * 7 % 31)));
        let t = Instant::now();
        fe.stop().map_err(|s| format!("stop: {s:?}"))?;
        stops.push(t.elapsed().as_micros());
        drain(&mut s)?;
        fe.start().map_err(|s| format!("start again: {s:?}"))?;
        let again = stream(
            &mut s,
            &mut identity,
            3,
            None,
            None,
            |_, _, _| {},
            |_, _| Ok(()),
        )?;
        restarted += u32::from(again.frames == 3);
    }
    stops.sort_unstable();
    out(format_args!(
        "stop with {BUFFERS} jobs queued, {cycles} times: median {} us, max {} us; started again with 3 frames {restarted} times",
        stops.get(stops.len() / 2).copied().unwrap_or(0),
        stops.last().copied().unwrap_or(0)
    ));
    ok &= restarted == cycles;
    let counters = fe.counters().map_err(|s| format!("counters: {s:?}"))?;
    out(format_args!("front end counters: {counters:?}"));
    ok &= counters.errors == 0;
    sensor.stop().map_err(|s| format!("sensor stop: {s:?}"))?;
    fe.stop().map_err(|s| format!("stop: {s:?}"))?;
    drain(&mut s)?;
    let guard0 = guard_clean(&s.output, layout.size as usize);
    let guard1 = match (&s.out1, layout1) {
        (Some(o), Some(l)) => guard_clean(o, l.size as usize),
        _ => None,
    };
    out(format_args!(
        "guard: {GUARD} bytes past each frame still zero: output0 {guard0:?}, output1 {guard1:?}"
    ));
    ok &= guard0.is_none_or(|g| g) && guard1.is_none_or(|g| g);
    Ok(ok)
}

/// Releases every frame waiting on the output and statistics streams.
fn drain(s: &mut Streams) -> Result<(), String> {
    for stream in [Some(&mut s.output), Some(&mut s.stats), s.out1.as_mut()]
        .into_iter()
        .flatten()
    {
        while let Some(frame) = stream.try_next().map_err(|e| format!("drain: {e:?}"))? {
            stream
                .release(frame)
                .map_err(|e| format!("release: {e:?}"))?;
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let Some(args) = parse() else {
        out(format_args!(
            "usage: lemnos-fe-proof [--socket PATH] [--fe ID] [--sensor ID] [--pattern CODE] [--frames N] [--frame-length N] [--reference FILE] [--save DIR] [--output1 1] [--late N] [--bad 1] [--stop-cycles N]"
        ));
        return ExitCode::from(2);
    };
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            out(format_args!("lemnos-fe-proof: {e}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_driver_pisp::fe_job::{Fixed, check};

    fn fixed() -> Fixed {
        Fixed {
            width: WIDTH as u16,
            height: HEIGHT as u16,
            output_width: WIDTH as u16,
            output_height: HEIGHT as u16,
            output_stride: WIDTH * 2,
            output_format: formats::BAYER16,
        }
    }

    fn parsed(bytes: &[u8]) -> FeConfig {
        let mut cfg = FeConfig::default();
        cfg.read_from(bytes).unwrap();
        cfg
    }

    #[test]
    fn the_good_configurations_pass_the_drivers_check_and_the_spoiled_ones_do_not() {
        for output1 in [false, true] {
            let good = configuration(Order::Rggb, output1).unwrap();
            let shape = check(&parsed(&good), &fixed()).unwrap();
            assert_eq!(shape.output1, output1);
            for which in 0..3 {
                let bad = spoiled(&good, which).unwrap();
                assert!(
                    check(&parsed(&bad), &fixed()).is_err(),
                    "{}",
                    SPOILED[which as usize]
                );
            }
        }
    }

    #[test]
    fn output_1_is_half_size() {
        let cfg = parsed(&configuration(Order::Rggb, true).unwrap());
        let f = cfg.ch[1].output.format;
        assert_eq!((f.width, f.height, f.stride), (640, 400, 1280));
    }

    #[test]
    fn a_halving_compares_each_pixel_with_the_mean_of_its_colour() {
        let (w0, h0) = (WIDTH as usize, HEIGHT as usize);
        let mut out0 = vec![0u8; w0 * h0 * 2];
        for y in 0..h0 {
            for x in 0..w0 {
                // Each colour its own level, rising across the image.
                let v = (((x % 2) * 2 + y % 2) * 4000 + x * 8 + y) as u16;
                out0[(y * w0 + x) * 2..][..2].copy_from_slice(&v.to_le_bytes());
            }
        }
        let (w1, h1) = (w0 / 2, h0 / 2);
        let mut out1 = vec![0u8; w1 * h1 * 2];
        for y in 0..h1 {
            for x in 0..w1 {
                let (bx, by) = (4 * (x / 2) + x % 2, 4 * (y / 2) + y % 2);
                let v = |x: usize, y: usize| {
                    u32::from((((x % 2) * 2 + y % 2) * 4000 + x * 8 + y) as u16)
                };
                let mean = (v(bx, by) + v(bx + 2, by) + v(bx, by + 2) + v(bx + 2, by + 2)) / 4;
                out1[(y * w1 + x) * 2..][..2].copy_from_slice(&(mean as u16).to_le_bytes());
            }
        }
        let n = (w1 * h1) as u64;
        assert_eq!(halving(&out0, w0 * 2, &out1, w1 * 2), (n, n, n, 0));
        out1[0] = out1[0].wrapping_add(100);
        let (_, equal, near, worst) = halving(&out0, w0 * 2, &out1, w1 * 2);
        assert_eq!((equal, near, worst), (n - 1, n - 1, 100));
    }
}
