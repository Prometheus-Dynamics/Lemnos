//! `lemnos-rate-proof`: the sensor's nominal frame rate against the frame timestamps, and the shape of
//! the single intervals.
//!
//! ```text
//! lemnos-rate-proof [--socket PATH] [--sensor ID] [--mode N] --asked-milli N [--lines N] [--frames N]
//!                   [--measure-only]
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
//!
//! Two different things can end the run, and the output and the exit status say which. The
//! `measurement:` line (the mean interval, the expected value, the difference in ns and in ppm) is
//! printed whenever frames were taken, and the `rate measurement:` line says `completed` (at least
//! 5000 frames, none dropped, no sequence gap) or `NOT MADE` with the reasons. By default the exit
//! status is as it always was: 0 only if the measurement completed AND is within the tolerance, 1
//! otherwise. With `--measure-only` the tolerance is not judged: a completed measurement exits 0 and
//! one that could not be made exits 3 (a usage error is 2; any error before the measurement, such
//! as a refused call or no camera, exits 1).

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

const USAGE: &str = "usage: lemnos-rate-proof [--socket PATH] [--sensor ID] [--mode N] --asked-milli N [--lines N] [--frames N] [--measure-only]";

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
    /// Report the measurement without judging it against the tolerance.
    measure_only: bool,
}

fn parse() -> Option<Args> {
    let mut args = Args {
        socket: "/run/lemnos/lemnosd.sock".into(),
        sensor: None,
        mode: 0,
        asked_milli: 0,
        lines: None,
        frames: 5000,
        measure_only: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        if flag == "--measure-only" {
            args.measure_only = true;
            continue;
        }
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

/// What a completed run measured: the mean interval against the one expected from the line time.
#[derive(Debug, PartialEq)]
struct Mean {
    /// `(last - first) / (N - 1)`, in ns.
    measured_ns: u64,
    /// The lines times the line time, in ns (the model the tool checks, with no extra term).
    expected_ns: u64,
    /// Measured minus expected, in ns.
    difference_ns: i64,
    /// The difference as parts per million of the expected value.
    difference_ppm: i64,
}

impl Mean {
    fn of(first: u64, last: u64, stamps: usize, period_ps: u128) -> Option<Self> {
        let spans = u64::try_from(stamps)
            .ok()?
            .checked_sub(1)
            .filter(|&n| n > 0)?;
        let measured_ns = last.saturating_sub(first) / spans;
        let expected_ns = u64::try_from(period_ps / 1000).ok().filter(|&n| n > 0)?;
        let difference_ns = i64::try_from(measured_ns).ok()? - i64::try_from(expected_ns).ok()?;
        let difference_ppm =
            i64::try_from(i128::from(difference_ns) * 1_000_000 / i128::from(expected_ns)).ok()?;
        Some(Self {
            measured_ns,
            expected_ns,
            difference_ns,
            difference_ppm,
        })
    }
}

/// How a run ended.
#[derive(Debug, PartialEq)]
enum Verdict {
    /// Measured, and within the tolerance.
    Pass,
    /// Measured, and beyond the tolerance (the measurement itself is sound).
    Differs(String),
    /// The measurement could not be made: too few frames, dropped frames, a sequence gap, no
    /// interval.
    NotMade(Vec<String>),
}

fn verdict(frames: u64, dropped: u64, gaps: u64, have_mean: bool, error_ppm: u128) -> Verdict {
    let mut unsound = Vec::new();
    if !have_mean {
        unsound.push("no frame interval to measure".to_string());
    }
    if frames < MIN_FRAMES {
        unsound.push(format!("{frames} frames, need {MIN_FRAMES}"));
    }
    if dropped != 0 || gaps != 0 {
        unsound.push(format!("dropped {dropped} gaps {gaps}"));
    }
    if !unsound.is_empty() {
        return Verdict::NotMade(unsound);
    }
    if error_ppm > TOLERANCE_PPM {
        return Verdict::Differs(format!("rate off by {error_ppm} ppm"));
    }
    Verdict::Pass
}

/// The exit status: 0 for a pass, and, with `measure_only`, for a measurement that completed;
/// 3 for a measurement that could not be made under `measure_only`; 1 otherwise.
fn exit_status(verdict: &Verdict, measure_only: bool) -> u8 {
    match (verdict, measure_only) {
        (Verdict::Pass, _) | (Verdict::Differs(_), true) => 0,
        (Verdict::NotMade(_), true) => 3,
        _ => 1,
    }
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

fn run(args: &Args) -> Result<u8, String> {
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

    let measured = report.fps_milli();
    let computed = 1_000_000_000_000_000u128 / mode.period_ps(lines);
    let error_ppm = measured.map_or(u128::MAX, |m| {
        u128::from(m.abs_diff(computed as u64)) * 1_000_000 / computed.max(1)
    });
    match measured {
        Some(measured) => out(format_args!(
            "frames {}, dropped {}, gaps {}; rate from timestamps {measured} milli-fps; nominal {computed} milli-fps; difference {error_ppm} ppm (limit {TOLERANCE_PPM})",
            report.frames, report.dropped, report.gaps
        )),
        None => out(format_args!(
            "frames {}, dropped {}, gaps {}; no frame interval to measure",
            report.frames, report.dropped, report.gaps
        )),
    }
    let gaps = intervals(&stamps.0);
    shape(&gaps);
    let mean = match (stamps.0.first(), stamps.0.last()) {
        (Some(&first), Some(&last)) => Mean::of(first, last, stamps.0.len(), mode.period_ps(lines)),
        _ => None,
    };
    if let Some(m) = &mean {
        // The mean interval is (last - first) / (N - 1): the figure testing/board-runs/F2h-judge.py reads.
        out(format_args!(
            "mean interval {} ns over {} intervals at {lines} lines, line time {} ps",
            m.measured_ns,
            stamps.0.len().saturating_sub(1),
            mode.line_time_ps
        ));
        out(format_args!(
            "measurement: mean interval {} ns, expected {} ns (lines x line time), difference {:+} ns ({:+} ppm)",
            m.measured_ns, m.expected_ns, m.difference_ns, m.difference_ppm
        ));
    }
    out(format_args!(
        "first 200 intervals (ns): {:?}",
        gaps.iter().take(200).collect::<Vec<_>>()
    ));
    let verdict = verdict(
        report.frames,
        report.dropped,
        report.gaps,
        mean.is_some() && measured.is_some(),
        error_ppm,
    );
    match &verdict {
        Verdict::NotMade(why) => {
            out(format_args!(
                "rate measurement: NOT MADE: {}",
                why.join("; ")
            ));
        }
        _ => out(format_args!("rate measurement: completed")),
    }
    match &verdict {
        Verdict::Pass => out(format_args!("rate proof: pass")),
        Verdict::Differs(why) if args.measure_only => {
            out(format_args!(
                "rate proof: not judged (--measure-only): {why}"
            ));
        }
        Verdict::Differs(why) => out(format_args!("rate proof: FAILED: {why}")),
        Verdict::NotMade(why) => out(format_args!("rate proof: FAILED: {}", why.join("; "))),
    }
    Ok(exit_status(&verdict, args.measure_only))
}

fn main() -> ExitCode {
    let Some(args) = parse() else {
        out(format_args!("{USAGE}"));
        return ExitCode::from(2);
    };
    match run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            out(format_args!("lemnos-rate-proof: {e}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mean_is_set_against_lines_times_line_time_in_ns_and_ppm() {
        // 5001 stamps 16_680_000 ns apart, expected 16_675_000 ns (2092 lines x 7.97 us is not
        // used here: the period is given directly).
        let last = 5000 * 16_680_000;
        let m = Mean::of(0, last, 5001, 16_675_000_000).unwrap();
        assert_eq!(m.measured_ns, 16_680_000);
        assert_eq!(m.expected_ns, 16_675_000);
        assert_eq!(m.difference_ns, 5_000);
        assert_eq!(m.difference_ppm, 299);
        let short = Mean::of(0, last, 5001, 16_685_000_000).unwrap();
        assert_eq!(short.difference_ns, -5_000);
        assert_eq!(short.difference_ppm, -299);
        assert_eq!(
            Mean::of(0, 0, 1, 1_000_000),
            None,
            "one stamp has no interval"
        );
        assert_eq!(Mean::of(0, 10, 2, 0), None, "no expected period");
    }

    #[test]
    fn a_sound_measurement_beyond_the_tolerance_differs_and_is_not_unmade() {
        let v = verdict(5000, 0, 0, true, 440);
        assert_eq!(v, Verdict::Differs("rate off by 440 ppm".into()));
        assert_eq!(exit_status(&v, false), 1, "the default exit is unchanged");
        assert_eq!(exit_status(&v, true), 0, "--measure-only does not judge");
        assert_eq!(verdict(5000, 0, 0, true, 200), Verdict::Pass);
        assert_eq!(exit_status(&Verdict::Pass, false), 0);
        assert_eq!(exit_status(&Verdict::Pass, true), 0);
    }

    #[test]
    fn a_measurement_that_could_not_be_made_exits_non_zero_in_both_modes() {
        for (frames, dropped, gaps, have) in [
            (4999, 0, 0, true),
            (5000, 1, 0, true),
            (5000, 0, 2, true),
            (0, 0, 0, false),
        ] {
            let v = verdict(frames, dropped, gaps, have, 0);
            assert!(
                matches!(v, Verdict::NotMade(_)),
                "{frames} {dropped} {gaps} {have}"
            );
            assert_eq!(exit_status(&v, false), 1);
            assert_eq!(exit_status(&v, true), 3);
        }
        let Verdict::NotMade(why) = verdict(10, 1, 0, true, 0) else {
            panic!("not unmade");
        };
        assert_eq!(why, ["10 frames, need 5000", "dropped 1 gaps 0"]);
    }
}
