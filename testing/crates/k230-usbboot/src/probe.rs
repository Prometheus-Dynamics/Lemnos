//! The return-path probes of the board plan (`testing/board-runs/K230-USB.md`, the "Return paths"
//! steps). Each run names, before it starts, which path is expected to return the board and the
//! window its elapsed time must fall in. The host measures the elapsed time from the start request to
//! the device's re-enumeration, classifies it, and prints the row. Nothing is written to storage.
//!
//! The four paths and their clocks (the windows are formulas; the clocks are UNVERIFIED until the
//! first run measures them):
//! - `wdt1`: the watchdog of the big core, TOP 10, `2^26` cycles of the WDT clock.
//! - `wdt0`: TOP 11, `2^27` cycles of the WDT clock.
//! - `deadline`: the entry's spin, `2^28` ticks of the core's `time` CSR (1 MHz assumed).
//! - `soc`: the SoC software reset, `soc_glb_rst`, taken at once; the window is the ROM's return time,
//!   which no source gives (UNVERIFIED; the window here is 0 to 5 s).
//!
//! The windows are ±10% of the formula. Where two windows overlap the verdict is ambiguous, and the
//! run is not decided by the host: the lead decides from the row.

use std::time::{Duration, Instant};

use crate::bootrom::{DOWNLOAD_PID, DOWNLOAD_VID, PROG_START, SET_DATA_ADDRESS};
use crate::transport::{UsbError, UsbIo};

/// The WDT clock range the design accepts (`lemnos_k230_usbboot_stage::wdt`, UNVERIFIED).
pub const WDT_HZ_RANGE: (u64, u64) = (1_000_000, 100_000_000);
/// The core's timebase assumed for the deadline (UNVERIFIED).
pub const TIMEBASE_HZ: u64 = 1_000_000;
/// The SoC reset's return window, in microseconds (UNVERIFIED: no source gives the ROM's boot time).
pub const SOC_WINDOW_US: (u64, u64) = (0, 5_000_000);

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
            Self::Deadline => "deadline (spin, soc reset)",
            Self::Soc => "soc reset at once",
        }
    }
}

/// The expected window of `path`, in microseconds, at the given clocks.
///
/// The deadline's reset is the SoC reset, so its window is the spin's time plus the ROM's return.
pub fn window_at(path: Path, wdt_hz: u64, timebase_hz: u64) -> (u64, u64) {
    let us = |cycles: u64, hz: u64| (u128::from(cycles) * 1_000_000 / u128::from(hz)) as u64;
    let (centre, slack) = match path {
        Path::Wdt1 => (us(1 << 26, wdt_hz), 0),
        Path::Wdt0 => (us(1 << 27, wdt_hz), 0),
        Path::Deadline => (us(1 << 28, timebase_hz), SOC_WINDOW_US.1),
        Path::Soc => return SOC_WINDOW_US,
    };
    let tenth = centre / 10;
    (centre - tenth, centre + tenth + slack)
}

/// The window of `path` over every clock the design accepts: the widest the run could see.
pub fn window_plausible(path: Path) -> (u64, u64) {
    let (lo, _) = window_at(path, WDT_HZ_RANGE.1, TIMEBASE_HZ);
    let (_, hi) = window_at(path, WDT_HZ_RANGE.0, TIMEBASE_HZ);
    (lo, hi)
}

/// One run of the plan's table.
#[derive(Clone, Copy, Debug)]
pub struct Run {
    pub id: u8,
    pub image: &'static str,
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
        image: "lemnos_probe_all",
        armed: "wdt0 + wdt1 + deadline",
        expected: Path::Wdt1,
        candidates: &[Path::Wdt1, Path::Wdt0, Path::Deadline],
    },
    Run {
        id: 2,
        image: "lemnos_probe_wdt0",
        armed: "wdt0 + deadline",
        expected: Path::Wdt0,
        candidates: &[Path::Wdt0, Path::Deadline],
    },
    Run {
        id: 3,
        image: "lemnos_probe_wdt1",
        armed: "wdt1 + deadline",
        expected: Path::Wdt1,
        candidates: &[Path::Wdt1, Path::Deadline],
    },
    Run {
        id: 4,
        image: "lemnos_probe_spin",
        armed: "deadline",
        expected: Path::Deadline,
        candidates: &[Path::Deadline],
    },
    Run {
        id: 5,
        image: "lemnos_probe_soc",
        armed: "soc reset only",
        expected: Path::Soc,
        candidates: &[Path::Soc],
    },
];

/// The verdict on one run's elapsed time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The board came back, and its time is in exactly one path's window.
    Matched(Path),
    /// The board came back in the window of more than one path: not decided by the host.
    Ambiguous(Vec<Path>),
    /// The board came back, in no path's window.
    Outside,
    /// The board did not come back within the wait: the run's failure to return.
    NoReturn,
}

/// Classifies an elapsed time against the windows of the paths the run armed, at the given clocks.
pub fn classify(
    elapsed: Option<Duration>,
    candidates: &[Path],
    wdt_hz: u64,
    timebase_hz: u64,
) -> Verdict {
    let Some(elapsed) = elapsed else {
        return Verdict::NoReturn;
    };
    let us = elapsed.as_micros() as u64;
    let hits: Vec<Path> = candidates
        .iter()
        .copied()
        .filter(|&path| {
            let (lo, hi) = window_at(path, wdt_hz, timebase_hz);
            (lo..=hi).contains(&us)
        })
        .collect();
    match hits.as_slice() {
        [] => Verdict::Outside,
        [one] => Verdict::Matched(*one),
        _ => Verdict::Ambiguous(hits),
    }
}

/// The printed row of one run: the expected path and window, and the actual field.
pub fn report(run: &Run, elapsed: Option<Duration>, wdt_hz: u64, timebase_hz: u64) -> String {
    let (lo, hi) = window_at(run.expected, wdt_hz, timebase_hz);
    let actual = match elapsed {
        Some(d) => format!("{:.3} s", d.as_secs_f64()),
        None => "no return".to_string(),
    };
    let verdict = match classify(elapsed, run.candidates, wdt_hz, timebase_hz) {
        Verdict::Matched(p) if p == run.expected => format!("as expected ({})", p.name()),
        Verdict::Matched(p) => format!("UNEXPECTED: returned by {}", p.name()),
        Verdict::Ambiguous(ps) => format!("ambiguous: {} paths fit", ps.len()),
        Verdict::Outside => "outside every window".to_string(),
        Verdict::NoReturn => {
            "FAILED TO RETURN: one power cycle, nothing changed on storage".to_string()
        }
    };
    format!(
        "run {} | image {} | armed: {} | expected: {} | window {:.3}-{:.3} s | actual: {} | {}",
        run.id,
        run.image,
        run.armed,
        run.expected.name(),
        lo as f64 / 1e6,
        hi as f64 / 1e6,
        actual,
        verdict
    )
}

/// Loads `image` at `address` and starts it, with no stage check: the probe's image is not the stage,
/// and the device's return is the measurement. Returns the elapsed time from the start request to
/// the device's re-enumeration, or `None` when it did not come back within `wait_ms`.
///
/// The start's own result is not used. The device is identified after the return: a probe comes back
/// as the boot ROM (`K230`), and a return with the download IDs is the only one counted.
pub fn run_probe(
    io: &mut dyn UsbIo,
    image: &[u8],
    address: u32,
    wait_ms: u32,
) -> Result<Option<Duration>, UsbError> {
    let (high, low) = ((address >> 16) as u16, (address & 0xffff) as u16);
    io.control_out(SET_DATA_ADDRESS, high, low, &[])?;
    for page in image.chunks(crate::bootrom::PAGE) {
        io.bulk_out(page)?;
    }
    let started = Instant::now();
    let _ = io.control_out(PROG_START, high, low, &[]);
    match io.wait_for_device(wait_ms) {
        Ok((vid, pid)) if (vid, pid) == (DOWNLOAD_VID, DOWNLOAD_PID) => Ok(Some(started.elapsed())),
        Ok(_) => Ok(None),
        Err(UsbError::Timeout(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_follow_the_formulas_at_the_clocks() {
        // At 1 MHz the wdt1 period is 67.1 s and wdt0's is 134.2 s; the deadline at 1 MHz is 268.4 s.
        let (lo, hi) = window_at(Path::Wdt1, 1_000_000, 1_000_000);
        assert!(lo < 67_108_864 && 67_108_864 < hi);
        let (lo, hi) = window_at(Path::Deadline, 1_000_000, 1_000_000);
        assert!(lo < 268_435_456 && 268_435_456 < hi);
    }

    #[test]
    fn a_return_is_matched_to_the_path_whose_window_it_is_in() {
        let all = &[Path::Wdt1, Path::Wdt0, Path::Deadline];
        // At 100 MHz: wdt1 is 0.671 s, wdt0 1.342 s; the deadline at 1 MHz is 268 s.
        assert_eq!(
            classify(
                Some(Duration::from_millis(700)),
                all,
                100_000_000,
                TIMEBASE_HZ
            ),
            Verdict::Matched(Path::Wdt1)
        );
        assert_eq!(
            classify(
                Some(Duration::from_millis(1_350)),
                all,
                100_000_000,
                TIMEBASE_HZ
            ),
            Verdict::Matched(Path::Wdt0)
        );
        assert_eq!(
            classify(
                Some(Duration::from_secs(268)),
                all,
                100_000_000,
                TIMEBASE_HZ
            ),
            Verdict::Matched(Path::Deadline)
        );
    }

    #[test]
    fn a_return_is_attributed_only_among_the_paths_the_run_armed() {
        // Run 3 armed wdt1 and the deadline only: a 1.3 s return cannot be wdt0, so it is outside.
        let armed = &[Path::Wdt1, Path::Deadline];
        assert_eq!(
            classify(
                Some(Duration::from_millis(1_350)),
                armed,
                100_000_000,
                TIMEBASE_HZ
            ),
            Verdict::Outside
        );
        // The soc window overlaps wdt1 at 100 MHz; run 1 never armed the soc reset alone, so it is not a candidate.
        let run1 = &[Path::Wdt1, Path::Wdt0, Path::Deadline];
        assert!(!matches!(
            classify(
                Some(Duration::from_millis(700)),
                run1,
                100_000_000,
                TIMEBASE_HZ
            ),
            Verdict::Ambiguous(_)
        ));
    }

    #[test]
    fn two_paths_that_fit_one_return_are_ambiguous_and_not_decided() {
        // The soc reset alone and the deadline are both candidates at the same time: the verdict is
        // ambiguous, and the host does not pick one.
        let both = &[Path::Deadline, Path::Soc];
        // At a 100 MHz timebase the deadline is 2.68 s plus the ROM's return, so 3 s fits both.
        let verdict = classify(Some(Duration::from_secs(3)), both, 1_000_000, 100_000_000);
        assert!(matches!(verdict, Verdict::Ambiguous(_)), "{verdict:?}");
    }

    #[test]
    fn no_return_is_named_as_a_failure_with_its_cost() {
        assert_eq!(
            classify(None, &[Path::Wdt0], 1_000_000, TIMEBASE_HZ),
            Verdict::NoReturn
        );
        let row = report(&RUNS[0], None, 24_000_000, TIMEBASE_HZ);
        assert!(row.contains("FAILED TO RETURN"), "{row}");
        assert!(row.contains("nothing changed on storage"), "{row}");
    }

    #[test]
    fn every_run_prints_its_expected_path_window_and_actual_field() {
        for run in &RUNS {
            let row = report(run, Some(Duration::from_secs(1)), 24_000_000, TIMEBASE_HZ);
            assert!(row.contains("expected:"), "{row}");
            assert!(row.contains("window"), "{row}");
            assert!(row.contains("actual:"), "{row}");
        }
        assert_eq!(RUNS.len(), 5);
        assert_eq!(RUNS[0].expected, Path::Wdt1);
    }

    #[test]
    fn the_plan_order_arms_all_paths_first_and_removes_one_per_run() {
        assert_eq!(RUNS[0].armed, "wdt0 + wdt1 + deadline");
        assert_eq!(RUNS[1].armed, "wdt0 + deadline");
        assert_eq!(RUNS[2].armed, "wdt1 + deadline");
        assert_eq!(RUNS[3].armed, "deadline");
        assert_eq!(RUNS[4].armed, "soc reset only");
    }
}
