//! The bench's 200 Hz stamping step. Reads `lemnos-ctl watch` lines on stdin, stamps each line with
//! the monotonic clock when it is read, and prints the IMU rows of the reference set as TSV
//! (key, status, value, note). It is the same arithmetic as `testing/bench/stamp.py`, which needs
//! python3, and which a board without python3 does not have. Nothing is made up: with no lines,
//! the rows say `not_measured`.
//!
//! Usage: `lemnos-bench-stamp --period-ms 5 --expect 9000 < watch-lines`
//!
//! A miss is a gap longer than 1.5 periods. A repeat is a line equal to the one before it.

use std::io::{self, BufRead, Write};
use std::time::Instant;

/// The rows for one run. `stamps` are monotonic nanoseconds, one per line, in read order;
/// `values` are the lines themselves.
pub fn rows(stamps: &[u64], values: &[String], period_ms: f64, expect: usize) -> Vec<String> {
    let note = format!("board, lemnos-ctl watch at {period_ms} ms, {expect} requested");
    if values.is_empty() {
        return [
            "imu.delivered",
            "imu.misses",
            "imu.largest_gap_ms",
            "imu.repeats",
        ]
        .iter()
        .map(|key| format!("{key}\tnot_measured\t\tno readings from the IMU"))
        .collect();
    }
    let gaps_ms: Vec<f64> = stamps
        .windows(2)
        .map(|pair| (pair[1] - pair[0]) as f64 / 1e6)
        .collect();
    let limit = 1.5 * period_ms;
    let misses = gaps_ms.iter().filter(|gap| **gap > limit).count();
    let repeats = values.windows(2).filter(|pair| pair[0] == pair[1]).count();
    let largest = gaps_ms.iter().cloned().fold(0.0_f64, f64::max);
    vec![
        format!(
            "imu.delivered\tmeasured\t{} of {expect}\t{note}",
            values.len()
        ),
        format!("imu.misses\tmeasured\t{misses}\tgaps over {limit} ms"),
        format!("imu.largest_gap_ms\tmeasured\t{largest:.2}\t{note}"),
        format!("imu.repeats\tmeasured\t{repeats}\treadings equal to the one before"),
    ]
}

fn parse_args(args: &[String]) -> Result<(f64, usize), String> {
    let mut period_ms = None;
    let mut expect = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--period-ms" => {
                let v = it.next().ok_or("--period-ms needs a value")?;
                period_ms = Some(v.parse::<f64>().map_err(|e| format!("--period-ms: {e}"))?);
            }
            "--expect" => {
                let v = it.next().ok_or("--expect needs a value")?;
                expect = Some(v.parse::<usize>().map_err(|e| format!("--expect: {e}"))?);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    match (period_ms, expect) {
        (Some(p), Some(e)) => Ok((p, e)),
        _ => Err("usage: lemnos-bench-stamp --period-ms N --expect N".to_string()),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (period_ms, expect) = match parse_args(&args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let base = Instant::now();
    let mut stamps = Vec::new();
    let mut values = Vec::new();
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        stamps.push(base.elapsed().as_nanos() as u64);
        values.push(line);
    }
    let out = io::stdout();
    let mut out = out.lock();
    for row in rows(&stamps, &values, period_ms, expect) {
        let _ = writeln!(out, "{row}");
    }
}

#[cfg(test)]
mod tests {
    use super::rows;

    fn lines(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("r{i}")).collect()
    }

    /// testing/crates/bench-stamp/src/main.rs::tests::counts_delivered_misses_and_repeats
    #[test]
    fn counts_delivered_misses_and_repeats() {
        // 5 ms period: gaps of 5, 5, 20 (a miss: over 7.5 ms), 5 ms.
        let stamps = [0u64, 5_000_000, 10_000_000, 30_000_000, 35_000_000];
        let mut values = lines(5);
        values[2] = values[1].clone(); // a repeat of the one before
        let out = rows(&stamps, &values, 5.0, 9000);
        assert_eq!(
            out[0],
            "imu.delivered\tmeasured\t5 of 9000\tboard, lemnos-ctl watch at 5 ms, 9000 requested"
        );
        assert_eq!(out[1], "imu.misses\tmeasured\t1\tgaps over 7.5 ms");
        assert_eq!(
            out[2],
            "imu.largest_gap_ms\tmeasured\t20.00\tboard, lemnos-ctl watch at 5 ms, 9000 requested"
        );
        assert_eq!(
            out[3],
            "imu.repeats\tmeasured\t1\treadings equal to the one before"
        );
    }

    /// testing/crates/bench-stamp/src/main.rs::tests::no_readings_is_not_measured
    #[test]
    fn no_readings_is_not_measured() {
        let out = rows(&[], &[], 5.0, 9000);
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|row| row.contains("\tnot_measured\t\t")));
    }
}
