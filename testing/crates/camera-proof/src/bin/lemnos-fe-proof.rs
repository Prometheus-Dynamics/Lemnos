//! `lemnos-fe-proof`: the PiSP front end on a real board, through `lemnosd`'s socket.
//!
//! ```text
//! lemnos-fe-proof [--socket PATH] [--fe ID] [--sensor ID] [--pattern CODE] [--frames N]
//!                 [--frame-length N] [--reference FILE] [--save DIR]
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
use lemnos_contract_isp_pisp::{IspPisp, LAYOUT_FRONT_END, OUTPUT_FRONT_END_0};
use lemnos_core::{Encode, Writer};
use lemnos_driver_pisp::fe::FrontEnd;
use lemnos_driver_pisp::format::formats;
use lemnos_driver_pisp::stats::Statistics;
use lemnos_driver_pisp::uapi::{
    BayerOrder as Order, Branch, FeAgcStatsConfig, FeAwbStatsConfig, FeRgbyConfig,
    ImageFormatConfig, Wire, fe_enable as en,
};
use lemnos_linux_stream::LinuxStreams;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
const RAW10_STRIDE: usize = 1600;
/// Buffers on every stream, and jobs kept queued.
const BUFFERS: u32 = 4;

struct Args {
    socket: String,
    fe: String,
    sensor: String,
    pattern: Option<u16>,
    frames: u32,
    frame_length: Option<u32>,
    reference: Option<PathBuf>,
    save: Option<PathBuf>,
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
fn configuration(bayer: Order) -> Result<Vec<u8>, String> {
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
    let cfg = fe
        .prepare()
        .map_err(|e| format!("the configuration library refused: {e:?}"))?;
    let mut bytes = vec![0u8; lemnos_driver_pisp::uapi::FeConfig::SIZE];
    cfg.write_to(&mut bytes)
        .map_err(|_| "configuration size".to_string())?;
    Ok(bytes)
}

fn post(config: &mut FrameStream, identity: u64) -> Result<(), String> {
    let buffer = u32::try_from((identity - 1) % u64::from(BUFFERS)).unwrap_or(0);
    let job = IspJob {
        identity,
        layout: LAYOUT_FRONT_END,
        config_buffer: buffer,
        input_buffer: NO_BUFFER,
        outputs: [NO_BUFFER, NO_BUFFER, buffer, NO_BUFFER],
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

struct Run {
    frames: u32,
    stats: u32,
    matched: u32,
    unseen: u32,
    fps: f64,
    last: Statistics,
}

/// Reads `frames` frames with their statistics, posting the next job on each buffer as it comes
/// back. `first` sees the first frame before it is released.
fn stream(
    cfg: &mut FrameStream,
    output: &mut FrameStream,
    stats: &mut FrameStream,
    identity: &mut u64,
    frames: u32,
    mut first: impl FnMut(&FrameStream, &HeldFrame),
) -> Result<Run, String> {
    let mut run = Run {
        frames: 0,
        stats: 0,
        matched: 0,
        unseen: 0,
        fps: 0.0,
        last: Statistics::default(),
    };
    for _ in 0..BUFFERS {
        post(cfg, *identity)?;
        *identity += 1;
    }
    let mut stamps = Vec::with_capacity(frames as usize);
    while run.frames < frames {
        let Some(frame) = output
            .next_frame(Duration::from_secs(2))
            .map_err(|e| format!("output0: {e:?}"))?
        else {
            out(format_args!(
                "no output frame for 2 s after {} frames",
                run.frames
            ));
            break;
        };
        if run.frames == 0 {
            first(output, &frame);
        }
        stamps.push(frame.timestamp_ns());
        let id = frame.identity();
        output
            .release(frame)
            .map_err(|e| format!("release: {e:?}"))?;
        run.frames += 1;
        if let Some(s) = stats
            .next_frame(Duration::from_millis(200))
            .map_err(|e| format!("statistics: {e:?}"))?
        {
            run.stats += 1;
            if s.identity() == id {
                run.matched += 1;
            }
            if run.frames == frames {
                let _ = run.last.parse_into(stats.bytes(&s));
            }
            stats.release(s).map_err(|e| format!("release: {e:?}"))?;
        }
        if run.frames + BUFFERS <= frames {
            post(cfg, *identity)?;
            *identity += 1;
        }
    }
    let intervals: Vec<u64> = stamps
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]))
        .collect();
    let mut sorted = intervals.clone();
    sorted.sort_unstable();
    let period = sorted.get(sorted.len() / 2).copied().unwrap_or(0);
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
    let layout = fe
        .set_output(
            OUTPUT_FRONT_END_0,
            IspOutput {
                width: WIDTH,
                height: HEIGHT,
                fourcc: u32::from_le_bytes(*b"BYR2"),
                pad_bytes: 0,
            },
        )
        .map_err(|s| format!("set_output: {s:?}"))?;
    let stride = layout.stride as usize;
    out(format_args!(
        "output 0: stride {} size {}",
        layout.stride, layout.size
    ));
    let config = configuration(order(mode.bayer))?;

    let platform = Arc::new(LinuxStreams);
    let mut cfg = client
        .open_stream_writable(fe_index, "config", platform.clone())
        .map_err(|e| format!("config stream: {e}"))?;
    let mut stats = client
        .open_stream(fe_index, "statistics", platform.clone())
        .map_err(|e| format!("statistics stream: {e}"))?;
    let mut output = client
        .open_stream(fe_index, "output0", platform)
        .map_err(|e| format!("output0 stream: {e}"))?;
    for buffer in 0..BUFFERS {
        cfg.write(buffer, 0, &config)
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
    let mut identity = 1u64;
    let started = Instant::now();
    let main = stream(
        &mut cfg,
        &mut output,
        &mut stats,
        &mut identity,
        args.frames,
        |stream, frame| {
            let image = stream.bytes(frame);
            zero_frame = image.iter().all(|&b| b == 0);
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
    if let Some(equal) = equal_lines {
        out(format_args!(
            "first frame: {equal} of {HEIGHT} lines equal the reference's, each value moved up 6 bits"
        ));
    }
    let awb = main.last.awb_total();
    out(format_args!(
        "last statistics: AWB {} pixels counted, means {:?}; AGC histogram {} pixels, mean bin {:?}",
        awb.counted,
        awb.mean(),
        main.last.histogram_count(),
        main.last.histogram_mean()
    ));
    let counters = fe.counters().map_err(|s| format!("counters: {s:?}"))?;
    out(format_args!("front end counters: {counters:?}"));

    // Stop with jobs on the block, then start again.
    for _ in 0..BUFFERS {
        post(&mut cfg, identity)?;
        identity += 1;
    }
    let t = Instant::now();
    fe.stop().map_err(|s| format!("stop: {s:?}"))?;
    let stop_us = t.elapsed().as_micros();
    while let Some(frame) = output.try_next().map_err(|e| format!("output0: {e:?}"))? {
        output
            .release(frame)
            .map_err(|e| format!("release: {e:?}"))?;
    }
    while let Some(frame) = stats.try_next().map_err(|e| format!("statistics: {e:?}"))? {
        stats
            .release(frame)
            .map_err(|e| format!("release: {e:?}"))?;
    }
    fe.start().map_err(|s| format!("start again: {s:?}"))?;
    let again = stream(
        &mut cfg,
        &mut output,
        &mut stats,
        &mut identity,
        30,
        |_, _| {},
    )?;
    out(format_args!(
        "stop with {BUFFERS} jobs queued: {stop_us} us; started again: {} frames, {} statistics",
        again.frames, again.stats
    ));
    let counters = fe.counters().map_err(|s| format!("counters: {s:?}"))?;
    out(format_args!("front end counters: {counters:?}"));
    sensor.stop().map_err(|s| format!("sensor stop: {s:?}"))?;
    fe.stop().map_err(|s| format!("stop: {s:?}"))?;
    Ok(main.frames == args.frames
        && main.stats == main.frames
        && main.matched == main.frames
        && !zero_frame
        && awb.counted > 0
        && equal_lines.is_none_or(|e| e == HEIGHT as usize)
        && again.frames == 30
        && counters.errors == 0)
}

fn main() -> ExitCode {
    let Some(args) = parse() else {
        out(format_args!(
            "usage: lemnos-fe-proof [--socket PATH] [--fe ID] [--sensor ID] [--pattern CODE] [--frames N] [--frame-length N] [--reference FILE] [--save DIR]"
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
