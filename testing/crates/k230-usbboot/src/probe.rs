//! The return-path probes of the board plan (`testing/board-runs/K230-USB.md`, step R). The host loads
//! the one stage image in a probe mode, starts it, and listens. While the board is up the image sends
//! telemetry (time CSR, both watchdogs' CR and CCVR, and a "taking <path> now" record before a path the
//! image's own code triggers). From two samples the host computes each watchdog's real clock and its
//! predicted expiry, against the host's own clock. The board's return is timed on the host.
//!
//! The verdict, in order: a "taking" record names the path; else the return is matched to the nearest
//! predicted expiry of an armed path, within 5% or 0.25 s; else, with no telemetry, the expected
//! windows classify it, and that is "ambiguous (telemetry missing)" when more than one window fits.
//! A return that matches no prediction is EARLY or LATE, with the path and by how much. A run with no
//! host still returns: the verdict then rests on the return time alone.

use std::time::{Duration, Instant};

use crate::bootrom::{DOWNLOAD_PID, DOWNLOAD_VID};
use lemnos_k230_usbboot_stage::format::{LOADER_VERSION, PACKET};
use lemnos_k230_usbboot_stage::image::LOAD_ADDRESS;
use lemnos_k230_usbboot_stage::telemetry::{self, PATH_DEADLINE, PATH_SOC, Record};
use lemnos_k230_usbboot_stage::wdt::{self, CR_ENABLE};

use crate::bootrom::{PAGE, PROG_START, SET_DATA_ADDRESS, TEXT_BOOT_ROM};
use crate::transport::{UsbError, UsbIo};

/// The WDT clock range the design accepts (UNVERIFIED until run 1 measures it).
pub const WDT_HZ_RANGE: (u64, u64) = (1_000_000, 100_000_000);
/// The core's timebase assumed for the deadline's window before telemetry measures it.
pub const TIMEBASE_HZ: u64 = 1_000_000;
/// The SoC reset's return window, in seconds, with no telemetry (UNVERIFIED: no source gives the ROM's
/// boot time).
pub const SOC_WINDOW_S: (f64, f64) = (0.0, 5.0);

/// A return path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    Wdt1,
    Wdt0,
    Deadline,
    Soc,
}

impl Path {
    pub fn name(self) -> &'static str {
        match self {
            Self::Wdt1 => "wdt1 (TOP 10)",
            Self::Wdt0 => "wdt0 (TOP 11)",
            Self::Deadline => "deadline (spin, then soc reset)",
            Self::Soc => "soc reset at once",
        }
    }

    fn from_taking(code: u8) -> Option<Self> {
        match code {
            PATH_DEADLINE => Some(Self::Deadline),
            PATH_SOC => Some(Self::Soc),
            _ => None,
        }
    }
}

/// The expected window of `path` with no telemetry, in seconds, at the given clocks.
pub fn window_at(path: Path, wdt_hz: u64, timebase_hz: u64) -> (f64, f64) {
    let centre = match path {
        Path::Wdt1 => (1u64 << 26) as f64 / wdt_hz as f64,
        Path::Wdt0 => (1u64 << 27) as f64 / wdt_hz as f64,
        Path::Deadline => (1u64 << 28) as f64 / timebase_hz as f64 + SOC_WINDOW_S.1,
        Path::Soc => return SOC_WINDOW_S,
    };
    (centre * 0.9, centre * 1.1)
}

/// The window over every clock the design accepts: the widest a window without telemetry can be.
pub fn window_plausible(path: Path) -> (f64, f64) {
    let (lo, _) = window_at(path, WDT_HZ_RANGE.1, TIMEBASE_HZ);
    let (_, hi) = window_at(path, WDT_HZ_RANGE.0, TIMEBASE_HZ);
    (lo, hi)
}

/// One run of the plan's table.
#[derive(Clone, Copy, Debug)]
pub struct Run {
    pub id: u8,
    pub image: &'static str,
    /// The mode name the host gives the image (`lemnos_k230_usbboot_stage::image::mode_word`).
    pub mode: &'static str,
    pub armed: &'static str,
    /// The path expected to return the board, named before the run.
    pub expected: Path,
    /// The paths this run armed: the only ones a return can be attributed to.
    pub candidates: &'static [Path],
}

/// The five runs, in the order the board plan takes them: all paths armed first, one removed per run.
pub const RUNS: [Run; 5] = [
    Run {
        id: 1,
        image: "stage image",
        mode: "all",
        armed: "wdt0 (TOP 11) + wdt1 (TOP 10) + deadline",
        expected: Path::Wdt1,
        candidates: &[Path::Wdt1, Path::Wdt0, Path::Deadline],
    },
    Run {
        id: 2,
        image: "stage image",
        mode: "wdt0",
        armed: "wdt0 (TOP 11) + deadline",
        expected: Path::Wdt0,
        candidates: &[Path::Wdt0, Path::Deadline],
    },
    Run {
        id: 3,
        image: "stage image",
        mode: "wdt1",
        armed: "wdt1 (TOP 10) + deadline",
        expected: Path::Wdt1,
        candidates: &[Path::Wdt1, Path::Deadline],
    },
    Run {
        id: 4,
        image: "stage image",
        mode: "spin",
        armed: "deadline only",
        expected: Path::Deadline,
        candidates: &[Path::Deadline],
    },
    Run {
        id: 5,
        image: "stage image",
        mode: "soc",
        armed: "soc reset only",
        expected: Path::Soc,
        candidates: &[Path::Soc],
    },
];

/// One telemetry record with the host's time of receipt, in seconds from the start request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timed {
    pub at_s: f64,
    pub record: Record,
}

/// What one probe run measured on the host.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// Telemetry in arrival order (empty when no host read any).
    pub telemetry: Vec<Timed>,
    /// The board's return, in seconds from the start request, when it came back as the boot ROM.
    pub returned_s: Option<f64>,
}

/// A watchdog's measured clock and predicted expiry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WdtEstimate {
    /// The clock, in hertz, from the change of CCVR over the device's own time.
    pub hz: f64,
    /// The predicted expiry, in host seconds from the start request.
    pub expiry_s: f64,
    /// Whether the counter agrees with its arming: at the first sample, the cycles it has lost since
    /// the arming are no more than the time that has passed.
    pub agrees_with_arming: bool,
}

/// What the telemetry says about the clocks and the predictions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Analysis {
    /// The device's time CSR rate, in hertz, from the host's clock.
    pub timebase_hz: Option<f64>,
    /// Each watchdog, when its counter was seen to run.
    pub wdt: [Option<WdtEstimate>; 2],
    /// The deadline's predicted expiry, in host seconds, when the timebase is known.
    pub deadline_s: Option<f64>,
    /// The first and last sample times, in host seconds.
    pub first_s: Option<f64>,
}

/// Decides one run's verdict.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// The return is the expected path.
    Matched(Path),
    /// The telemetry or the return names another path than the one expected.
    Unexpected(Path),
    /// The board came back before the path it matches was predicted to.
    Early { path: Path, by_s: f64 },
    /// The board came back after the path it matches was predicted to.
    Late { path: Path, by_s: f64 },
    /// No telemetry, and more than one window fits the return.
    Ambiguous { paths: Vec<Path>, telemetry: bool },
    /// The return fits no window of the paths the run armed.
    Outside,
    /// The board did not come back within the wait.
    NoReturn,
}

/// Runs one probe: loads `image` at the stage's load address, starts it, and listens until the board
/// returns or `wait` has passed. The telemetry is read while the image is up; a host that reads none
/// still gets the return. The start's result is not used.
pub fn run_probe(io: &mut dyn UsbIo, image: &[u8], wait: Duration) -> Result<Outcome, UsbError> {
    let (high, low) = ((LOAD_ADDRESS >> 16) as u16, (LOAD_ADDRESS & 0xffff) as u16);
    io.control_out(SET_DATA_ADDRESS, high, low, &[])?;
    for page in image.chunks(PAGE) {
        io.bulk_out(page)?;
    }
    let start = Instant::now();
    let _ = io.control_out(PROG_START, high, low, &[]);
    let mut outcome = Outcome::default();
    while start.elapsed() < wait {
        match io.wait_for_device(200) {
            Ok((vid, pid)) if (vid, pid) == (DOWNLOAD_VID, DOWNLOAD_PID) => {}
            Ok(_) | Err(UsbError::Timeout(_)) => continue,
            Err(error) => return Err(error),
        }
        match identity(io)? {
            Some(Identity::Image) => {
                read_telemetry(io, start, &mut outcome.telemetry);
            }
            Some(Identity::Rom) => {
                outcome.returned_s = Some(start.elapsed().as_secs_f64());
                break;
            }
            None => {}
        }
    }
    Ok(outcome)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Identity {
    /// The image itself (the stage's identity text).
    Image,
    /// The boot ROM, after the image's reset.
    Rom,
}

/// Identifies what answers now. A device that is enumerated but deaf is [`UsbError::Deaf`], named, and is
/// never retried blindly: a deaf ROM is not waited for.
fn identity(io: &mut dyn UsbIo) -> Result<Option<Identity>, UsbError> {
    let reply = match io.control_in(0, 0, 0, 32) {
        Ok(reply) => reply,
        Err(UsbError::Deaf) => return Err(UsbError::Deaf),
        Err(_) => return Ok(None),
    };
    let text = String::from_utf8_lossy(&reply)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string();
    Ok(if text == TEXT_BOOT_ROM {
        Some(Identity::Rom)
    } else if text == lemnos_k230_usbboot_stage::format::STAGE_IDENT {
        let _ = LOADER_VERSION;
        Some(Identity::Image)
    } else {
        None
    })
}

/// Reads telemetry until the host stops getting records (a timeout, or the device left the bus).
fn read_telemetry(io: &mut dyn UsbIo, start: Instant, out: &mut Vec<Timed>) {
    while let Ok(bytes) = io.bulk_in(PACKET) {
        if let Some(record) = telemetry::decode(&bytes) {
            out.push(Timed {
                at_s: start.elapsed().as_secs_f64(),
                record,
            });
        }
    }
}

/// The samples in `records`, as `(host seconds, time CSR ticks, cr, ccvr)`.
fn samples(records: &[Timed]) -> Vec<(f64, u64, [u32; 2], [u32; 2])> {
    records
        .iter()
        .filter_map(|t| match t.record {
            Record::Sample { ticks, cr, ccvr } => Some((t.at_s, ticks, cr, ccvr)),
            _ => None,
        })
        .collect()
}

/// Computes the clocks and the predictions from the telemetry. Needs two samples at least half a second
/// apart in host time; without them the analysis is empty and the verdict falls back to the windows.
pub fn analyse(records: &[Timed]) -> Analysis {
    let s = samples(records);
    let mut analysis = Analysis::default();
    let (Some(first), Some(last)) = (s.first().copied(), s.last().copied()) else {
        return analysis;
    };
    analysis.first_s = Some(first.0);
    if last.0 - first.0 < 0.5 || last.1 <= first.1 {
        return analysis;
    }
    let timebase = (last.1 - first.1) as f64 / (last.0 - first.0);
    analysis.timebase_hz = Some(timebase);
    analysis.deadline_s = Some(first.0 + wdt::DEADLINE_US as f64 / 1e6);
    for i in 0..2 {
        let enabled = |sample: &(f64, u64, [u32; 2], [u32; 2])| sample.2[i] & CR_ENABLE != 0;
        if !(enabled(&first) && enabled(&last)) {
            continue;
        }
        let consumed = first.3[i].saturating_sub(last.3[i]);
        if consumed == 0 || last.1 <= first.1 {
            continue;
        }
        let device_s = (last.1 - first.1) as f64 / timebase;
        let hz = f64::from(consumed) / device_s;
        let remaining_s = f64::from(last.3[i]) / hz;
        // The arming of each watchdog in a probe: wdt0 at TOP 11, wdt1 at TOP 10 (image.rs).
        let period_cycles = if i == 0 { (1u64 << 27) as f64 } else { (1u64 << 26) as f64 };
        // Cycles lost since the arming, at the first sample: at most the host time that has passed.
        let lost = period_cycles - f64::from(first.3[i]);
        let agrees = lost >= 0.0 && lost <= first.0 * hz * 1.05 + 1.0;
        analysis.wdt[i] = Some(WdtEstimate {
            hz,
            expiry_s: last.0 + remaining_s,
            agrees_with_arming: agrees,
        });
    }
    analysis
}

/// The predicted expiry of `path` from an analysis, in host seconds.
pub fn predicted(analysis: &Analysis, path: Path) -> Option<f64> {
    match path {
        Path::Wdt1 => analysis.wdt[1].map(|e| e.expiry_s),
        Path::Wdt0 => analysis.wdt[0].map(|e| e.expiry_s),
        Path::Deadline => analysis.deadline_s,
        Path::Soc => None,
    }
}

/// The verdict on one run, from the expected path, the armed candidates, the telemetry and the return.
pub fn verdict(run: &Run, outcome: &Outcome) -> (Analysis, Verdict) {
    let analysis = analyse(&outcome.telemetry);
    let Some(actual) = outcome.returned_s else {
        return (analysis, Verdict::NoReturn);
    };
    // A "taking" record names the path the image triggered itself.
    if let Some(path) = outcome.telemetry.iter().rev().find_map(|t| match t.record {
        Record::Taking { path, .. } => Path::from_taking(path),
        _ => None,
    }) {
        let verdict = if path == run.expected {
            Verdict::Matched(path)
        } else {
            Verdict::Unexpected(path)
        };
        return (analysis, verdict);
    }
    if analysis.timebase_hz.is_some() {
        // The nearest predicted expiry among the paths the run armed.
        let best = run
            .candidates
            .iter()
            .filter_map(|&p| predicted(&analysis, p).map(|t| (p, t)))
            .min_by(|a, b| (a.1 - actual).abs().total_cmp(&(b.1 - actual).abs()));
        if let Some((path, expiry)) = best {
            let tolerance = (expiry * 0.05).max(0.25);
            let diff = actual - expiry;
            let verdict = if diff.abs() <= tolerance {
                if path == run.expected {
                    Verdict::Matched(path)
                } else {
                    Verdict::Unexpected(path)
                }
            } else if diff < 0.0 {
                Verdict::Early {
                    path,
                    by_s: -diff,
                }
            } else {
                Verdict::Late { path, by_s: diff }
            };
            return (analysis, verdict);
        }
    }
    // No telemetry (or none that predicts): the windows decide, and two that fit are ambiguous.
    let (wdt_hz, timebase) = (WDT_HZ_RANGE.1, TIMEBASE_HZ);
    let _ = (wdt_hz, timebase);
    let fits: Vec<Path> = run
        .candidates
        .iter()
        .copied()
        .filter(|&p| {
            let (lo, hi) = window_plausible(p);
            (lo..=hi).contains(&actual)
        })
        .collect();
    let verdict = match fits.as_slice() {
        [] => Verdict::Outside,
        [one] => {
            if *one == run.expected {
                Verdict::Matched(*one)
            } else {
                Verdict::Unexpected(*one)
            }
        }
        many => Verdict::Ambiguous {
            paths: many.to_vec(),
            telemetry: !outcome.telemetry.is_empty(),
        },
    };
    (analysis, verdict)
}

/// The printed row of one run: expected path, predicted time, the last telemetry record, the actual
/// return time, and the verdict.
pub fn report(run: &Run, outcome: &Outcome) -> String {
    let (analysis, verdict) = verdict(run, outcome);
    let predicted_text = {
        let mut parts = Vec::new();
        for &p in run.candidates {
            if let Some(t) = predicted(&analysis, p) {
                parts.push(format!("{} {:.3} s", short(p), t));
            }
        }
        if parts.is_empty() {
            "none (telemetry missing)".to_string()
        } else {
            parts.join(", ")
        }
    };
    let last = match outcome.telemetry.last() {
        Some(Timed {
            record: Record::Sample { ticks, ccvr, .. },
            ..
        }) => format!("sample at {ticks} ticks, ccvr {} / {}", ccvr[0], ccvr[1]),
        Some(Timed {
            record: Record::Taking { path, .. },
            ..
        }) => format!("taking path {path} now"),
        Some(Timed {
            record: Record::Boot { ctl },
            ..
        }) => format!("boot record ctl {ctl:#010x}"),
        Some(Timed {
            record: Record::Card { blocks, .. },
            ..
        }) => format!("card record, {blocks} blocks"),
        None => "none (telemetry missing)".to_string(),
    };
    let actual = match outcome.returned_s {
        Some(s) => format!("{s:.3} s"),
        None => "no return".to_string(),
    };
    let verdict_text = match &verdict {
        Verdict::Matched(p) => format!("MATCHED {}", p.name()),
        Verdict::Unexpected(p) => format!("UNEXPECTED: returned by {}", p.name()),
        Verdict::Early { path, by_s } => format!("EARLY: {} fired {by_s:.3} s before predicted", short(*path)),
        Verdict::Late { path, by_s } => format!("LATE: {} fired {by_s:.3} s after predicted", short(*path)),
        Verdict::Ambiguous { paths, telemetry } => format!(
            "ambiguous: {} paths fit{}",
            paths.len(),
            if *telemetry { "" } else { " (telemetry missing)" }
        ),
        Verdict::Outside => "outside every armed window".to_string(),
        Verdict::NoReturn => "FAILED TO RETURN: one power cycle, nothing changed on storage".to_string(),
    };
    format!(
        "run {} | mode {} | armed: {} | expected: {} | predicted: {} | last telemetry: {} | actual: {} | verdict: {}",
        run.id,
        run.mode,
        run.armed,
        run.expected.name(),
        predicted_text,
        last,
        actual,
        verdict_text
    )
}

fn short(path: Path) -> &'static str {
    match path {
        Path::Wdt1 => "wdt1",
        Path::Wdt0 => "wdt0",
        Path::Deadline => "deadline",
        Path::Soc => "soc",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Samples for a watchdog counting at `hz`, armed `period` cycles at time 0, one per `step_s`.
    fn samples_for(hz: f64, period: u64, step_s: f64, count: usize, tb: u64, which: usize) -> Vec<Timed> {
        (0..count)
            .map(|k| {
                let at = k as f64 * step_s;
                let consumed = (at * hz) as u64;
                let mut ccvr = [0u32; 2];
                let mut cr = [0u32; 2];
                ccvr[which] = (period - consumed) as u32;
                cr[which] = CR_ENABLE;
                Timed {
                    at_s: at,
                    record: Record::Sample {
                        ticks: (at * tb as f64) as u64,
                        cr,
                        ccvr,
                    },
                }
            })
            .collect()
    }

    #[test]
    fn telemetry_present_measures_the_clock_and_predicts_the_return() {
        // wdt1 at 10 MHz: its period is 2^26 cycles, 6.7109 s. The telemetry is 0.25 s apart.
        let records = samples_for(10_000_000.0, 1 << 26, 0.25, 20, 1_000_000, 1);
        let outcome = Outcome {
            telemetry: records,
            returned_s: Some(6.72),
        };
        let run = &RUNS[2];
        let (analysis, verdict) = verdict(run, &outcome);
        let est = analysis.wdt[1].expect("wdt1's clock is measured");
        assert!((est.hz - 10_000_000.0).abs() < 1_000.0, "{}", est.hz);
        assert!((est.expiry_s - 6.7109).abs() < 0.01, "{}", est.expiry_s);
        assert!(est.agrees_with_arming);
        assert_eq!(verdict, Verdict::Matched(Path::Wdt1));
        let row = report(run, &outcome);
        for field in ["expected:", "predicted:", "last telemetry:", "actual:", "verdict:"] {
            assert!(row.contains(field), "{row}");
        }
    }

    #[test]
    fn telemetry_absent_is_ambiguous_when_two_windows_fit_and_says_so() {
        // 1.3 s fits wdt1's window (0.6 to 74 s over the clocks) and wdt0's (1.2 to 148 s).
        let outcome = Outcome {
            telemetry: Vec::new(),
            returned_s: Some(1.3),
        };
        let run = &RUNS[0];
        let (analysis, verdict) = verdict(run, &outcome);
        assert!(analysis.timebase_hz.is_none());
        assert!(matches!(verdict, Verdict::Ambiguous { telemetry: false, .. }), "{verdict:?}");
        let row = report(run, &outcome);
        assert!(row.contains("telemetry missing"), "{row}");
    }

    #[test]
    fn a_return_before_the_predicted_expiry_is_early_and_names_the_path() {
        // The counter says 6.7 s; the board came back at 5.0 s.
        let outcome = Outcome {
            telemetry: samples_for(10_000_000.0, 1 << 26, 0.25, 20, 1_000_000, 1),
            returned_s: Some(5.0),
        };
        let (_, verdict) = verdict(&RUNS[2], &outcome);
        match verdict {
            Verdict::Early { path, by_s } => {
                assert_eq!(path, Path::Wdt1);
                assert!((by_s - 1.71).abs() < 0.05, "{by_s}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_counter_that_disagrees_with_its_arming_is_flagged_by_the_telemetry() {
        // The counter reads 3/4 of the period at the first sample: it fires at 3/4 (the early fault).
        let mut records = samples_for(10_000_000.0, 1 << 26, 0.25, 20, 1_000_000, 1);
        for t in records.iter_mut() {
            if let Record::Sample { ccvr, .. } = &mut t.record {
                ccvr[1] = (ccvr[1] as u64 * 3 / 4) as u32;
            }
        }
        let outcome = Outcome {
            telemetry: records,
            returned_s: Some(5.03),
        };
        let analysis = analyse(&outcome.telemetry);
        assert!(!analysis.wdt[1].unwrap().agrees_with_arming);
        let (_, verdict) = verdict(&RUNS[2], &outcome);
        assert!(matches!(verdict, Verdict::Early { path: Path::Wdt1, .. }), "{verdict:?}");
    }

    #[test]
    fn a_taking_record_names_its_path_and_the_run_matches_or_not() {
        let mut records = samples_for(1_000_000.0, 1 << 28, 0.5, 4, 1_000_000, 0);
        records.push(Timed {
            at_s: 2.5,
            record: Record::Taking {
                path: PATH_DEADLINE,
                ticks: 2_500_000,
            },
        });
        let outcome = Outcome {
            telemetry: records,
            returned_s: Some(2.7),
        };
        assert_eq!(verdict(&RUNS[3], &outcome).1, Verdict::Matched(Path::Deadline));
        assert_eq!(verdict(&RUNS[1], &outcome).1, Verdict::Unexpected(Path::Deadline));
    }

    #[test]
    fn no_return_is_a_named_failure_with_its_cost() {
        let outcome = Outcome::default();
        let row = report(&RUNS[0], &outcome);
        assert!(row.contains("FAILED TO RETURN"), "{row}");
        assert!(row.contains("nothing changed on storage"), "{row}");
    }

    #[test]
    fn the_plan_order_arms_all_paths_first_and_removes_one_per_run() {
        assert_eq!(RUNS[0].candidates.len(), 3);
        assert_eq!(RUNS[1].candidates, &[Path::Wdt0, Path::Deadline]);
        assert_eq!(RUNS[2].candidates, &[Path::Wdt1, Path::Deadline]);
        assert_eq!(RUNS[3].candidates, &[Path::Deadline]);
        assert_eq!(RUNS[4].candidates, &[Path::Soc]);
        assert_eq!(RUNS[4].mode, "soc");
    }

    #[test]
    fn the_windows_follow_the_formulas_without_telemetry() {
        let (lo, hi) = window_at(Path::Wdt1, 1_000_000, 1_000_000);
        assert!(lo < 67.108864 && 67.108864 < hi);
        assert!(window_plausible(Path::Wdt1).0 < window_plausible(Path::Wdt1).1);
    }
}
