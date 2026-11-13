//! `lemnos-rate-proof`: the sensor's nominal frame rate against the frame timestamps, and the shape of
//! the single intervals.
//!
//! ```text
//! lemnos-rate-proof [--socket PATH] [--sensor ID] [--mode N] --asked-milli N [--lines N] [--frames N]
//! ```
//!
//! The frame length is the whole-line count nearest the asked rate, on the nominal line time
//! ([`lemnos_contract_camera_sensor::frame_length_for_fps_milli`]). `--lines N` sets the frame length
//! instead, to N lines (within the mode's range), so one mode can be run at VTS, VTS ± 1 and VTS ± 10
//! and the slope and the intercept of the period read from the means (testing/board-runs/F2h.md). The camera runs `--frames` frames
//! (at least 5000). The measured rate is `(frames - 1) / (last - first)` from the timestamps, which does
//! not depend on how the single intervals spread. The computed rate is `1e15 / (line time × lines)`.
//! The check fails beyond 200 parts per million (0.02 %), on a dropped frame or a sequence gap, or on
//! fewer than 5000 frames. Nothing is fitted: a difference is printed, not corrected.
//!
//! It also prints the shape of the single intervals, which the summaries of a run do not give: the
//! shortest and longest, a histogram in 100 ns buckets, the run lengths of the intervals on each side
//! of the midpoint, the autocorrelation of that sequence at lags 1 to 8, and the first 200 intervals.
//! Testing/board-runs/F2h.md says what each of these would show for each hypothesis.

use std::collections::BTreeMap;
use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use lemnos_camera_proof::{Error, FrameSink, Options, SocketBus, assemble, capture};
use lemnos_client::Client;
use lemnos_contract_camera_sensor::{Controls, fps_milli_of, frame_length_for_fps_milli};
use lemnos_wire::ring::FrameSlot;

/// The check's tolerance: 0.02 %.
const TOLERANCE_PPM: u128 = 200;
/// The fewest frames the mean is taken over.
const MIN_FRAMES: u64 = 5000;

const USAGE: &str = "usage: lemnos-rate-proof [--socket PATH] [--sensor ID] [--mode N] --asked-milli N [--frames N]";

fn out(text: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stdout(), "{text}");
}

/// Records each frame's timestamp; the bytes are not kept.
#[derive(Default)]
struct Stamps(Vec<u64>);

impl FrameSink for Stamps {
    fn save(&mut self, frame: &FrameSlot, _bytes: &[u8]) -> std::io::Result<()> {
        self.0.push(frame.timestamp_ns);
        Ok(())
    }
}

struct Args {
    socket: String,
    sensor: Option<String>,
    mode: u16,
    asked_milli: u32,
    lines: Option<u32>,
    frames: u64,
}

fn parse() -> Option<Args> {
    let mut args = Args {
        socket: "/run/lemnos/lemnosd.sock".into(),
        sensor: None,
        mode: 0,
        asked_milli: 0,
        lines: None,
        frames: 5000,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next()?;
        match flag.as_str() {
            "--socket" => args.socket = value,
            "--sensor" => args.sensor = Some(value),
            "--mode" => args.mode = value.parse().ok()?,
            "--asked-milli" => args.asked_milli = value.parse().ok()?,
            "--lines" => args.lines = Some(value.parse().ok()?),
            "--frames" => args.frames = value.parse().ok()?,
            _ => return None,
        }
    }
    (args.asked_milli > 0).then_some(args)
}

/// The intervals between successive timestamps, in ns.
fn intervals(stamps: &[u64]) -> Vec<u64> {
    stamps
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]))
        .collect()
}

/// Prints the shape of the intervals.
fn shape(gaps: &[u64]) {
    let (Some(&min), Some(&max)) = (gaps.iter().min(), gaps.iter().max()) else {
        return;
    };
    out(format_args!(
        "intervals: shortest {min} ns, longest {max} ns, longest minus shortest {} ns",
        max - min
    ));
    let mut hist: BTreeMap<u64, u64> = BTreeMap::new();
    for &g in gaps {
        *hist.entry(g / 100 * 100).or_default() += 1;
    }
    out(format_args!(
        "histogram of intervals (100 ns buckets: bucket ns, count):"
    ));
    for (bucket, count) in &hist {
        out(format_args!("  {bucket:>10} {count:>6}"));
    }
    // The short and the long side of the midpoint, as -1 and +1.
    let mid = min / 2 + max / 2;
    let sign: Vec<f64> = gaps
        .iter()
        .map(|&g| if g > mid { 1.0 } else { -1.0 })
        .collect();
    let mut runs: BTreeMap<usize, u64> = BTreeMap::new();
    let mut run = 1usize;
    for pair in sign.windows(2) {
        if pair[0] == pair[1] {
            run += 1;
        } else {
            *runs.entry(run).or_default() += 1;
            run = 1;
        }
    }
    *runs.entry(run).or_default() += 1;
    out(format_args!(
        "run lengths of the short/long sequence (length: count):"
    ));
    for (len, count) in &runs {
        out(format_args!("  {len}: {count}"));
    }
    let n = sign.len() as f64;
    let mean = sign.iter().sum::<f64>() / n;
    let var = sign.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n;
    let mut line = String::from("autocorrelation of the short/long sequence, lags 1 to 8:");
    for lag in 1..=8usize {
        let c = sign
            .iter()
            .zip(sign.iter().skip(lag))
            .map(|(a, b)| (a - mean) * (b - mean))
            .sum::<f64>()
            / (n - lag as f64);
        line.push_str(&format!(" {lag}:{:+.3}", c / var.max(1e-12)));
    }
    out(format_args!("{line}"));
}

fn run(args: &Args) -> Result<bool, String> {
    let client = Client::connect(&args.socket, "lemnos-rate-proof")
        .map_err(|e| format!("{}: {e}", args.socket))?;
    client.set_timeout(Duration::from_secs(5));
    let bus = SocketBus::new(client);
    let mut camera = assemble(
        &bus,
        &Options {
            sensor: args.sensor.clone(),
            mode: args.mode,
        },
    )
    .map_err(|e| format!("assemble: {e:?}"))?;
    let mode = *camera.mode();
    let (lines, nominal) = match args.lines {
        Some(lines) if (mode.frame_length_min..=mode.frame_length_max).contains(&lines) => (
            lines,
            fps_milli_of(mode.line_time_ps, lines).ok_or("the line time is zero")?,
        ),
        Some(lines) => {
            return Err(format!(
                "--lines {lines} is outside the mode's {}..={}",
                mode.frame_length_min, mode.frame_length_max
            ));
        }
        None => frame_length_for_fps_milli(
            mode.line_time_ps,
            mode.frame_length_min,
            mode.frame_length_max,
            args.asked_milli,
        )
        .ok_or("no frame length gives the asked rate")?,
    };
    let step = fps_milli_of(mode.line_time_ps, lines)
        .unwrap_or(0)
        .abs_diff(fps_milli_of(mode.line_time_ps, lines + 1).unwrap_or(0));
    out(format_args!(
        "mode {}: line time {} ps; asked {} milli-fps: frame length {lines} lines (allowed {}..={}), nominal rate {nominal} milli-fps",
        args.mode,
        mode.line_time_ps,
        args.asked_milli,
        mode.frame_length_min,
        mode.frame_length_max
    ));
    out(format_args!(
        "residual against the asked rate: {} milli-fps; one line changes the nominal rate by {step} milli-fps",
        i64::from(nominal) - i64::from(args.asked_milli)
    ));
    camera
        .set_controls(Controls {
            frame_length: Some(lines),
            ..Controls::default()
        })
        .map_err(|e| format!("set frame length: {e:?}"))?;
    camera.start().map_err(|e| format!("start: {e:?}"))?;
    let mut stamps = Stamps::default();
    let taken = capture(&mut camera, args.frames, &mut stamps);
    let stopped = camera.stop();
    let report = taken.map_err(|e: Error| format!("capture: {e:?}"))?;
    stopped.map_err(|e| format!("stop: {e:?}"))?;

    let measured = report.fps_milli().ok_or("no frame interval to measure")?;
    let period_ps = u128::from(mode.line_time_ps) * u128::from(lines);
    let computed = 1_000_000_000_000_000u128 / period_ps;
    let error_ppm = u128::from(measured.abs_diff(computed as u64)) * 1_000_000 / computed.max(1);
    out(format_args!(
        "frames {}, dropped {}, gaps {}; rate from timestamps {measured} milli-fps; nominal {computed} milli-fps; difference {error_ppm} ppm (limit {TOLERANCE_PPM})",
        report.frames, report.dropped, report.gaps
    ));
    let gaps = intervals(&stamps.0);
    shape(&gaps);
    if let (Some(&first), Some(&last)) = (stamps.0.first(), stamps.0.last()) {
        // The mean interval is (last - first) / (N - 1): the figure testing/board-runs/F2h-judge.py reads.
        let spans = (stamps.0.len() as u64).saturating_sub(1).max(1);
        out(format_args!(
            "mean interval {} ns over {spans} intervals at {lines} lines, line time {} ps",
            last.saturating_sub(first) / spans,
            mode.line_time_ps
        ));
    }
    out(format_args!(
        "first 200 intervals (ns): {:?}",
        gaps.iter().take(200).collect::<Vec<_>>()
    ));
    let mut failed = Vec::new();
    if report.frames < MIN_FRAMES {
        failed.push(format!("{} frames, need {MIN_FRAMES}", report.frames));
    }
    if report.dropped != 0 || report.gaps != 0 {
        failed.push(format!("dropped {} gaps {}", report.dropped, report.gaps));
    }
    if error_ppm > TOLERANCE_PPM {
        failed.push(format!("rate off by {error_ppm} ppm"));
    }
    if failed.is_empty() {
        out(format_args!("rate proof: pass"));
        Ok(true)
    } else {
        out(format_args!("rate proof: FAILED: {}", failed.join("; ")));
        Ok(false)
    }
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
            out(format_args!("lemnos-rate-proof: {e}"));
            ExitCode::FAILURE
        }
    }
}
