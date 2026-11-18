//! The arithmetic of the B8 capture: decode the records of each batch, and check what the chip
//! delivered against the stamps' bounds. The binary (`main.rs`) only reads batches through the daemon
//! and hands them here with the host's times, so everything the record checks is measured by code that
//! is tested on its own.
//!
//! A batch is read with `read_batch_stamped`: each record is its mode (tick time or the model) and the
//! sample, `STAMPED_RECORD_LEN` bytes. A batch that is not a whole number of records is refused, not
//! decoded: a batch is whole records only, so a partial one is a driver or transport fault and the
//! capture says so. The stamp is `at_ns` and is always a plain time on `CLOCK_MONOTONIC`, the clock the
//! host reads its own times on.
//!
//! The checks (B8.md, check 3 and 4; check 7 for the cross-die spread):
//!
//! - (a) Per batch. The newest record of a batch was taken before the FIFO was read, and not more than
//!   one sample period before it. The read started at `start` and returned at `end`, so the sample's
//!   stamp lies in `[start - P - TOL, end + TOL]`. A stamp later than `end + TOL` is a stamp later than the
//!   read, and a stamp more than `(end - start) + P + TOL` before `end` is one the chip could not have
//!   held. `P` is the sample period, `TOL` the model's own phase budget (1 ms, the driver's
//!   `PHASE_WIDTH_NS`). Every batch's newest record is checked, in either mode: a tick-time stamp
//!   from before the read fails this by its lag, and that is the fault it exists to catch.
//! - (b) Over the run, for the model's records. Each model batch's newest record gives one point: its
//!   record number, its stamp, and the end of its read. The least-squares slope of the stamps against the
//!   record number is compared with the slope of the read ends (the host's clock for the same samples). The
//!   difference is a rate, in ppm. Its bound is the most the difference can be given that each point's
//!   `end - stamp` lies in a band as wide as the widest point's `(end - start) + P + 2 TOL`: the largest
//!   least-squares slope such a band allows, from the points' own record numbers. A rate error beyond that
//!   bound is a fault, and a band-wide error at one batch cannot move the fit past it.
//! - (c) Over every record. No stamp earlier than the one before it, and no gap longer than 1.5 sample
//!   periods.
//! - Check 7 (gyroscope runs only). Each model gyroscope record's stamp against the nearest model
//!   accelerometer record's stamp: the spread (max minus min) of that offset over the capture, in ms.
//!   Reported, not judged: check 7 stays open until a clock-based alignment is in `next`.

use lemnos_contract_accelerometer::{STAMPED_RECORD_LEN, Stamp, Stamped};
use lemnos_core::{Decode, Reader};

/// The model's phase budget, in nanoseconds: the driver's `PHASE_WIDTH_NS` (1 ms). A stamp can be this
/// far from the bracket its reads give it, because the phase is only known to within that much.
pub const TOL_NS: u64 = 1_000_000;

/// When one batch read started and returned, on the host's monotonic clock, in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReadSpan {
    pub start_ns: u64,
    pub end_ns: u64,
}

/// One non-empty batch's newest record, with the read that fetched it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Point {
    /// The record's number in the run, counted from 0.
    index: u64,
    stamp: u64,
    mode: Stamp,
    read: ReadSpan,
}

/// What one capture measured.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    /// Batch reads that were made.
    pub reads: u64,
    /// Batch reads that returned no records.
    pub empty_reads: u64,
    /// The largest number of records one read returned.
    pub max_per_read: usize,
    /// Records decoded.
    pub records: u64,
    /// Records stamped from the host's tick time.
    pub tick_records: u64,
    /// Records stamped from the device's sample clock's model.
    pub model_records: u64,
    /// Where the stamp's mode changed: the non-empty batch (counted from 0) whose first record has a
    /// different mode from the record before it, and the mode it changed to.
    pub mode_changes: Vec<(u64, Stamp)>,
    /// Records whose stamp is earlier than the record before (across batches too).
    pub backwards: u64,
    /// Bytes that did not make a whole record, in batches that were refused.
    pub refused_bytes: u64,
    /// The stamps of the records, in order.
    pub stamps: Vec<u64>,
    /// Each record's mode, in order.
    pub modes: Vec<Stamp>,
    /// Records whose value equals the one before it.
    pub repeats: u64,
    /// The previous record's value, to count repeats across batches.
    last: Option<[f32; 3]>,
    /// Non-empty batches so far.
    batches: u64,
    /// The mode of the last record.
    last_mode: Option<Stamp>,
    /// Each non-empty batch's newest record, for checks (a) and (b).
    points: Vec<Point>,
}

/// One batch read that could not be decoded.
#[derive(Debug, PartialEq)]
pub enum BatchError {
    /// `bytes` is not a whole number of records: the chip or the transport handed over part of one.
    Partial { bytes: usize },
    /// A record did not decode.
    Undecodable { index: usize },
}

/// Check (a)'s figures and verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckA {
    pub batches: u64,
    /// Batches whose newest stamp is outside its bracket.
    pub violations: u64,
    /// The smallest `end - stamp`, in ns: how long before its read's end the newest stamp is. Negative
    /// is a stamp after the read.
    pub min_lag_ns: i64,
    /// The largest `end - stamp`, in ns.
    pub max_lag_ns: i64,
    /// The widest allowed `end - stamp` over the batches, in ns: `(end - start) + P + TOL`.
    pub bound_ns: u64,
}

/// Check (b)'s figures and verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckB {
    /// Model batches the fit used.
    pub points: u64,
    /// The stamps' fitted rate, in ns per record.
    pub stamp_ns_per_record: f64,
    /// The host's fitted rate for the same records, in ns per record.
    pub host_ns_per_record: f64,
    /// The stamps' rate against the host's, in ppm (positive: the stamps run fast).
    pub rate_ppm: f64,
    /// The bound on that rate, in ppm, from the points' record numbers and the widest band.
    pub bound_ppm: f64,
}

/// Check (c)'s figures and verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckC {
    pub backwards: u64,
    /// Gaps longer than 1.5 sample periods.
    pub misses: u64,
    pub largest_gap_ns: u64,
}

/// Check 7's figures. The spread is of the offset from each gyroscope stamp to the nearest accelerometer
/// stamp, so it is at most about one accelerometer period: it cannot show a drift between the two dies,
/// only the phase between them. The drift is the rate: the two dies' stamp slopes against their own
/// record numbers, as a ratio.
#[derive(Debug, Clone, PartialEq)]
pub struct Check7 {
    pub pairs: u64,
    pub min_offset_ns: i64,
    pub max_offset_ns: i64,
    /// The gyroscope's stamp slope over the accelerometer's, minus one, in ppm.
    pub rate_ppm: f64,
}

impl Report {
    /// Records one batch read of stamped records (`read_batch_stamped`) into `batch`, which the host
    /// read between `read.start_ns` and `read.end_ns`.
    pub fn add_batch(&mut self, batch: &[u8], read: ReadSpan) -> Result<(), BatchError> {
        self.reads += 1;
        if batch.is_empty() {
            self.empty_reads += 1;
            return Ok(());
        }
        if !batch.len().is_multiple_of(STAMPED_RECORD_LEN) {
            self.refused_bytes += batch.len() as u64;
            return Err(BatchError::Partial { bytes: batch.len() });
        }
        let count = batch.len() / STAMPED_RECORD_LEN;
        self.max_per_read = self.max_per_read.max(count);
        let batch_no = self.batches;
        self.batches += 1;
        let mut newest = None;
        for (index, record) in batch.as_chunks::<STAMPED_RECORD_LEN>().0.iter().enumerate() {
            let stamped = Stamped::decode(&mut Reader::new(record))
                .map_err(|_| BatchError::Undecodable { index })?;
            let sample = stamped.sample;
            let value = [sample.value.x, sample.value.y, sample.value.z];
            if self.last == Some(value) {
                self.repeats += 1;
            }
            self.last = Some(value);
            if self.last_mode != Some(stamped.stamp) {
                // The first record of this batch that differs from the one before it.
                self.mode_changes.push((batch_no, stamped.stamp));
                self.last_mode = Some(stamped.stamp);
            }
            match stamped.stamp {
                Stamp::TickTime => self.tick_records += 1,
                Stamp::Model => self.model_records += 1,
            }
            if self
                .stamps
                .last()
                .is_some_and(|&previous| sample.at_ns < previous)
            {
                self.backwards += 1;
            }
            self.stamps.push(sample.at_ns);
            self.modes.push(stamped.stamp);
            self.records += 1;
            newest = Some((self.records - 1, sample.at_ns, stamped.stamp));
        }
        if let Some((index, stamp, mode)) = newest {
            self.points.push(Point {
                index,
                stamp,
                mode,
                read,
            });
        }
        Ok(())
    }

    /// The average time between consecutive stamps, in nanoseconds (`None` with fewer than two).
    pub fn mean_period_ns(&self) -> Option<f64> {
        let (first, last) = (self.stamps.first()?, self.stamps.last()?);
        (self.stamps.len() >= 2).then(|| (last - first) as f64 / (self.stamps.len() - 1) as f64)
    }

    /// The largest gap between consecutive stamps, in nanoseconds.
    pub fn largest_gap_ns(&self) -> u64 {
        self.stamps
            .windows(2)
            .map(|pair| pair[1].saturating_sub(pair[0]))
            .max()
            .unwrap_or(0)
    }

    /// Check (a): each batch's newest stamp against its read.
    pub fn check_a(&self, period_ns: u64) -> CheckA {
        let mut out = CheckA {
            batches: self.points.len() as u64,
            violations: 0,
            min_lag_ns: i64::MAX,
            max_lag_ns: i64::MIN,
            bound_ns: 0,
        };
        for point in &self.points {
            let read_ns = point.read.end_ns - point.read.start_ns;
            let bound = read_ns + period_ns + TOL_NS;
            let lag = point.read.end_ns as i64 - point.stamp as i64;
            out.min_lag_ns = out.min_lag_ns.min(lag);
            out.max_lag_ns = out.max_lag_ns.max(lag);
            out.bound_ns = out.bound_ns.max(bound);
            if lag < -(TOL_NS as i64) || lag > bound as i64 {
                out.violations += 1;
            }
        }
        if self.points.is_empty() {
            out.min_lag_ns = 0;
            out.max_lag_ns = 0;
        }
        out
    }

    /// Check (b): the model's stamps' rate against the host's, over the run. `None` with fewer than two
    /// model batches, which is not a measurement.
    pub fn check_b(&self, period_ns: u64) -> Option<CheckB> {
        let model: Vec<&Point> = self
            .points
            .iter()
            .filter(|p| p.mode == Stamp::Model)
            .collect();
        if model.len() < 2 {
            return None;
        }
        let xs: Vec<f64> = model.iter().map(|p| p.index as f64).collect();
        let base_stamp = model[0].stamp as f64;
        let base_host = model[0].read.end_ns as f64;
        let stamps: Vec<f64> = model.iter().map(|p| p.stamp as f64 - base_stamp).collect();
        let hosts: Vec<f64> = model
            .iter()
            .map(|p| p.read.end_ns as f64 - base_host)
            .collect();
        let stamp_rate = slope(&xs, &stamps);
        let host_rate = slope(&xs, &hosts);
        // Each point's `end - stamp` lies in a band of width `(end - start) + P + 2 TOL`. The widest of
        // those over the points is the band; the fit's slope can move by at most half of it times the
        // least-squares weight of the record numbers.
        let width = model
            .iter()
            .map(|p| (p.read.end_ns - p.read.start_ns + period_ns + 2 * TOL_NS) as f64)
            .fold(0.0_f64, f64::max);
        let mean_x = xs.iter().sum::<f64>() / xs.len() as f64;
        let spread: f64 = xs.iter().map(|x| (x - mean_x).abs()).sum();
        let sum_sq: f64 = xs.iter().map(|x| (x - mean_x).powi(2)).sum();
        let bound_ns_per_record = if sum_sq > 0.0 {
            (width / 2.0) * spread / sum_sq
        } else {
            f64::INFINITY
        };
        Some(CheckB {
            points: model.len() as u64,
            stamp_ns_per_record: stamp_rate,
            host_ns_per_record: host_rate,
            rate_ppm: (stamp_rate - host_rate) / host_rate * 1e6,
            bound_ppm: bound_ns_per_record / host_rate * 1e6,
        })
    }

    /// Check (c): no stamp goes back, and no gap is over 1.5 sample periods.
    pub fn check_c(&self, period_ns: u64) -> CheckC {
        CheckC {
            backwards: self.backwards,
            misses: self.misses(period_ns),
            largest_gap_ns: self.largest_gap_ns(),
        }
    }

    /// Gaps longer than `1.5` periods at `period_ns`: the records the chip did not deliver.
    pub fn misses(&self, period_ns: u64) -> u64 {
        let limit = period_ns + period_ns / 2;
        self.stamps
            .windows(2)
            .filter(|pair| pair[1].saturating_sub(pair[0]) > limit)
            .count() as u64
    }

    /// The span from the first stamp to the last, in nanoseconds. Not a check: the window it is measured
    /// against includes the time before the first read and after the last, so it is only shown.
    pub fn span_ns(&self) -> u64 {
        match (self.stamps.first(), self.stamps.last()) {
            (Some(first), Some(last)) => last - first,
            _ => 0,
        }
    }

    /// The model's stamps, in order.
    pub fn model_stamps(&self) -> Vec<u64> {
        self.stamps
            .iter()
            .zip(&self.modes)
            .filter(|(_, mode)| **mode == Stamp::Model)
            .map(|(stamp, _)| *stamp)
            .collect()
    }
}

/// The least-squares slope of `ys` against `xs`.
fn slope(xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len() as f64;
    let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
    let cov: f64 = xs.iter().zip(ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    let var: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
    cov / var
}

/// Check 7: the spread of the offset between each gyroscope model stamp and the nearest accelerometer
/// model stamp. `None` when either side has no model stamps. Both lists are in order.
pub fn check_7(gyro: &[u64], accel: &[u64]) -> Option<Check7> {
    if gyro.is_empty() || accel.is_empty() {
        return None;
    }
    let mut min = i64::MAX;
    let mut max = i64::MIN;
    for &g in gyro {
        let at = accel.partition_point(|&a| a < g);
        let near = [at.checked_sub(1), (at < accel.len()).then_some(at)]
            .into_iter()
            .flatten()
            .map(|i| accel[i])
            .min_by_key(|&a| a.abs_diff(g))?;
        let offset = g as i64 - near as i64;
        min = min.min(offset);
        max = max.max(offset);
    }
    let index = |n: usize| (0..n).map(|i| i as f64).collect::<Vec<f64>>();
    let stamps = |s: &[u64]| {
        let base = s[0] as f64;
        s.iter().map(|&v| v as f64 - base).collect::<Vec<f64>>()
    };
    let gyro_rate = slope(&index(gyro.len()), &stamps(gyro));
    let accel_rate = slope(&index(accel.len()), &stamps(accel));
    Some(Check7 {
        pairs: gyro.len() as u64,
        min_offset_ns: min,
        max_offset_ns: max,
        rate_ppm: (gyro_rate / accel_rate - 1.0) * 1e6,
    })
}

/// The mode changes as `batch:mode` pairs, separated by spaces, or `none` when no record came.
pub fn mode_changes(report: &Report) -> String {
    if report.mode_changes.is_empty() {
        return "none".to_string();
    }
    report
        .mode_changes
        .iter()
        .map(|(batch, mode)| {
            let mode = match mode {
                Stamp::TickTime => "tick",
                Stamp::Model => "model",
            };
            format!("{batch}:{mode}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn verdict(pass: bool) -> &'static str {
    if pass { "pass" } else { "fail" }
}

/// Whether every check passes on this capture. A capture with no batches, or a model fit that could not
/// be made, does not pass.
pub fn all_pass(report: &Report, period_ns: u64) -> bool {
    let a = report.check_a(period_ns);
    let b = report.check_b(period_ns);
    let c = report.check_c(period_ns);
    a.batches > 0
        && a.violations == 0
        && b.as_ref().is_some_and(|b| b.rate_ppm.abs() <= b.bound_ppm)
        && c.backwards == 0
        && c.misses == 0
        && report.records > 0
}

/// The TSV rows of the report, as the record's checks read them (key, status, value, note).
pub fn rows(report: &Report, period_ns: u64, elapsed_ns: u64) -> String {
    let mut out = String::new();
    let mut row = |key: &str, value: String, note: &str| {
        out.push_str(&format!("{key}\tmeasured\t{value}\t{note}\n"));
    };
    row(
        "b8.records",
        report.records.to_string(),
        "records decoded from the FIFO",
    );
    row(
        "b8.tick_records",
        report.tick_records.to_string(),
        "records stamped from the host's tick time (the sample clock had not converged, or re-anchored)",
    );
    row(
        "b8.model_records",
        report.model_records.to_string(),
        "records stamped from the sample clock's model",
    );
    row(
        "b8.mode_changes",
        mode_changes(report),
        "batch:mode at which each stamping mode began (tick or model)",
    );
    row("b8.reads", report.reads.to_string(), "batch reads made");
    row(
        "b8.empty_reads",
        report.empty_reads.to_string(),
        "batch reads that returned no records",
    );
    row(
        "b8.max_per_read",
        report.max_per_read.to_string(),
        "the most records one read returned",
    );
    row(
        "b8.refused_bytes",
        report.refused_bytes.to_string(),
        "bytes in batches that were not whole records",
    );
    row(
        "b8.repeats",
        report.repeats.to_string(),
        "records equal to the one before",
    );
    if let Some(period) = report.mean_period_ns() {
        row(
            "b8.mean_period_us",
            format!("{:.2}", period / 1e3),
            "average between consecutive stamps",
        );
    }
    row(
        "b8.span_vs_elapsed_ms",
        format!("{:.3}", (report.span_ns() as f64 - elapsed_ns as f64) / 1e6),
        "information only, not a check: first to last stamp, minus the capture's elapsed time",
    );

    let a = report.check_a(period_ns);
    row(
        "b8.a.batches",
        a.batches.to_string(),
        "non-empty batches, each checked on its newest record",
    );
    row(
        "b8.a.min_lag_ms",
        format!("{:.3}", a.min_lag_ns as f64 / 1e6),
        "smallest (read end - newest stamp): negative is a stamp after its read",
    );
    row(
        "b8.a.max_lag_ms",
        format!("{:.3}", a.max_lag_ns as f64 / 1e6),
        "largest (read end - newest stamp)",
    );
    row(
        "b8.a.bound_ms",
        format!("{:.3}", a.bound_ns as f64 / 1e6),
        "allowed (read end - newest stamp): read latency + one sample period + 1 ms",
    );
    row(
        "b8.a.violations",
        a.violations.to_string(),
        "batches whose newest stamp is outside [read start - P - 1 ms, read end + 1 ms]",
    );
    row(
        "b8.a.verdict",
        verdict(a.batches > 0 && a.violations == 0).to_string(),
        "check (a)",
    );

    match report.check_b(period_ns) {
        Some(b) => {
            row(
                "b8.b.points",
                b.points.to_string(),
                "model batches in the rate fit",
            );
            row(
                "b8.b.rate_ppm",
                format!("{:.1}", b.rate_ppm),
                "stamps' fitted rate against the host's, over the same records (positive: stamps fast)",
            );
            row(
                "b8.b.bound_ppm",
                format!("{:.1}", b.bound_ppm),
                "bound on that rate from the record numbers and the widest read band",
            );
            row(
                "b8.b.verdict",
                verdict(b.rate_ppm.abs() <= b.bound_ppm).to_string(),
                "check (b)",
            );
        }
        None => row(
            "b8.b.verdict",
            "fail".to_string(),
            "check (b): fewer than two model batches, so no rate was measured",
        ),
    }

    let c = report.check_c(period_ns);
    row(
        "b8.c.backwards",
        c.backwards.to_string(),
        "records stamped earlier than the record before them",
    );
    row(
        "b8.c.misses",
        c.misses.to_string(),
        "gaps between consecutive stamps over 1.5 sample periods",
    );
    row(
        "b8.c.largest_gap_ms",
        format!("{:.3}", c.largest_gap_ns as f64 / 1e6),
        "largest gap between two stamps",
    );
    row(
        "b8.c.verdict",
        verdict(c.backwards == 0 && c.misses == 0 && report.records > 0).to_string(),
        "check (c)",
    );
    row(
        "b8.verdict",
        verdict(all_pass(report, period_ns)).to_string(),
        "all of checks (a), (b) and (c)",
    );
    out
}

/// The check-7 rows, for a gyroscope capture: the spread of the offset to the accelerometer's model stamps.
pub fn check7_rows(gyro: &Report, accel: &Report) -> String {
    match check_7(&gyro.model_stamps(), &accel.model_stamps()) {
        Some(c) => format!(
            "b8.check7.pairs\tmeasured\t{}\tgyroscope model records paired with their nearest accelerometer model record\n\
             b8.check7.spread_ms\tmeasured\t{:.3}\tmax minus min of the offset (gyroscope stamp - nearest accelerometer stamp); reported, not judged (check 7 stays open)\n\
             b8.check7.min_offset_ms\tmeasured\t{:.3}\tsmallest offset\n\
             b8.check7.max_offset_ms\tmeasured\t{:.3}\tlargest offset\n\
             b8.check7.rate_ppm\tmeasured\t{:.1}\tthe gyroscope's stamp slope over the accelerometer's, minus one, per record (the dies' clocks; reported, not judged)\n",
            c.pairs,
            (c.max_offset_ns - c.min_offset_ns) as f64 / 1e6,
            c.min_offset_ns as f64 / 1e6,
            c.max_offset_ns as f64 / 1e6,
            c.rate_ppm,
        ),
        None => "b8.check7\tnot_measured\t\tno model records on one of the dies\n".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_contract_accelerometer::{Sample, Stamp};
    use lemnos_core::{Encode, Vector3, Writer};

    const P: u64 = 5_000_000;

    /// A stamped record as the driver writes it: the mode, then the stamp and the value, in the
    /// contract's encoding.
    fn stamped(stamp: Stamp, at_ns: u64, x: f32, y: f32, z: f32) -> [u8; STAMPED_RECORD_LEN] {
        let sample = Sample {
            at_ns,
            value: Vector3 { x, y, z },
        };
        let mut buf = [0u8; STAMPED_RECORD_LEN];
        buf[0] = match stamp {
            Stamp::Model => 0,
            Stamp::TickTime => 1,
        };
        let mut writer = Writer::new(&mut buf[1..]);
        sample.encode(&mut writer).unwrap();
        buf
    }

    /// A model-stamped record.
    fn record(at_ns: u64, x: f32, y: f32, z: f32) -> [u8; STAMPED_RECORD_LEN] {
        stamped(Stamp::Model, at_ns, x, y, z)
    }

    fn batch(records: &[[u8; STAMPED_RECORD_LEN]]) -> Vec<u8> {
        records.iter().flat_map(|r| r.iter().copied()).collect()
    }

    /// A read from `start` to `end`.
    fn read(start: u64, end: u64) -> ReadSpan {
        ReadSpan {
            start_ns: start,
            end_ns: end,
        }
    }

    /// A clean stream: `n` model records one sample period apart from `t0`, read in batches of `per`
    /// records. Each batch is read at the time its newest sample was taken plus one period, with a
    /// read latency of `latency` ns: the stamps are exact, so every check passes.
    fn clean_report(n: u64, per: u64, latency: u64) -> Report {
        let mut report = Report::default();
        let mut k = 0;
        while k < n {
            let end_of = (k + per).min(n);
            let records: Vec<[u8; STAMPED_RECORD_LEN]> = (k..end_of)
                .map(|i| record(i * P, 0.0, 0.0, 9.8 + i as f32 * 1e-3))
                .collect();
            let newest = (end_of - 1) * P;
            // The read is at newest + up to one period, and it takes `latency`.
            let end = newest + P / 2 + latency;
            report
                .add_batch(&batch(&records), read(end - latency, end))
                .unwrap();
            k = end_of;
        }
        report
    }

    /// testing/crates/bmi088-batch/src/lib.rs::tests::a_whole_batch_decodes_in_order_with_its_stamps
    #[test]
    fn a_whole_batch_decodes_in_order_with_its_stamps() {
        let mut report = Report::default();
        let bytes = batch(&[record(1_000, 0.1, 0.2, 9.8), record(6_000, 0.1, 0.2, 9.7)]);
        report.add_batch(&bytes, read(0, 10)).unwrap();
        assert_eq!(report.records, 2);
        assert_eq!(report.stamps, vec![1_000, 6_000]);
        assert_eq!(report.max_per_read, 2);
        assert_eq!(report.mean_period_ns(), Some(5_000.0));
    }

    /// testing/crates/bmi088-batch/src/lib.rs::tests::a_partial_batch_is_refused_and_counted_not_decoded
    /// Fails on a tool that decoded the leading whole records of a partial batch: this one refuses it.
    #[test]
    fn a_partial_batch_is_refused_and_counted_not_decoded() {
        let mut report = Report::default();
        let mut bytes = batch(&[record(1_000, 0.0, 0.0, 9.8)]);
        bytes.push(0xab); // a torn record
        assert_eq!(
            report.add_batch(&bytes, read(0, 10)),
            Err(BatchError::Partial {
                bytes: STAMPED_RECORD_LEN + 1
            })
        );
        assert_eq!(report.records, 0, "nothing from a refused batch is decoded");
        assert_eq!(report.refused_bytes, (STAMPED_RECORD_LEN + 1) as u64);
    }

    /// testing/crates/bmi088-batch/src/lib.rs::tests::an_empty_read_is_counted_and_is_not_a_record
    #[test]
    fn an_empty_read_is_counted_and_is_not_a_record() {
        let mut report = Report::default();
        report.add_batch(&[], read(0, 10)).unwrap();
        report.add_batch(&[], read(10, 20)).unwrap();
        assert_eq!(
            (report.reads, report.empty_reads, report.records),
            (2, 2, 0)
        );
    }

    /// testing/crates/bmi088-batch/src/lib.rs::tests::misses_are_gaps_over_one_and_a_half_periods
    #[test]
    fn misses_are_gaps_over_one_and_a_half_periods() {
        let mut report = Report::default();
        // 5 ms period: gaps of 5, 5, 20 (a miss), 5.
        let bytes = batch(&[
            record(0, 1.0, 0.0, 0.0),
            record(5_000_000, 2.0, 0.0, 0.0),
            record(10_000_000, 3.0, 0.0, 0.0),
            record(30_000_000, 4.0, 0.0, 0.0),
            record(35_000_000, 5.0, 0.0, 0.0),
        ]);
        report
            .add_batch(&bytes, read(35_000_000, 35_500_000))
            .unwrap();
        assert_eq!(report.misses(5_000_000), 1);
        assert_eq!(report.largest_gap_ns(), 20_000_000);
    }

    /// testing/crates/bmi088-batch/src/lib.rs::tests::repeats_count_across_batches
    #[test]
    fn repeats_count_across_batches() {
        let mut report = Report::default();
        report
            .add_batch(&batch(&[record(0, 1.0, 1.0, 1.0)]), read(0, 10))
            .unwrap();
        report
            .add_batch(
                &batch(&[record(5, 1.0, 1.0, 1.0), record(10, 2.0, 1.0, 1.0)]),
                read(10, 20),
            )
            .unwrap();
        assert_eq!(report.repeats, 1);
    }

    /// The mode is its own byte, and the stamp stays a plain time in both modes.
    #[test]
    fn a_stamped_batch_counts_each_records_mode_and_keeps_stamps_plain() {
        let mut report = Report::default();
        report
            .add_batch(
                &batch(&[
                    stamped(Stamp::TickTime, 1_000, 0.0, 0.0, 9.8),
                    stamped(Stamp::TickTime, 6_000, 0.0, 0.0, 9.7),
                ]),
                read(0, 10),
            )
            .unwrap();
        report
            .add_batch(
                &batch(&[
                    stamped(Stamp::Model, 11_000, 0.1, 0.0, 9.8),
                    stamped(Stamp::Model, 16_000, 0.2, 0.0, 9.8),
                ]),
                read(10, 20),
            )
            .unwrap();
        assert_eq!(report.records, 4);
        assert_eq!((report.tick_records, report.model_records), (2, 2));
        assert_eq!(report.stamps, vec![1_000, 6_000, 11_000, 16_000]);
        assert_eq!(report.backwards, 0);
        assert_eq!(mode_changes(&report), "0:tick 1:model");
    }

    /// A batch from a run that switches at the second batch names that batch, and a re-anchor that goes
    /// back to tick time names the batch it happens in.
    #[test]
    fn a_switch_and_a_return_to_tick_time_are_named_by_batch() {
        let mut report = Report::default();
        report
            .add_batch(
                &batch(&[stamped(Stamp::TickTime, 0, 0.0, 0.0, 1.0)]),
                read(0, 1),
            )
            .unwrap();
        report
            .add_batch(
                &batch(&[stamped(Stamp::TickTime, 5, 0.0, 0.0, 2.0)]),
                read(1, 2),
            )
            .unwrap();
        report.add_batch(&[], read(2, 3)).unwrap(); // an empty read is not a batch
        report
            .add_batch(
                &batch(&[stamped(Stamp::Model, 10, 0.0, 0.0, 3.0)]),
                read(3, 4),
            )
            .unwrap();
        report
            .add_batch(
                &batch(&[stamped(Stamp::Model, 15, 0.0, 0.0, 4.0)]),
                read(4, 5),
            )
            .unwrap();
        report
            .add_batch(
                &batch(&[stamped(Stamp::TickTime, 20, 0.0, 0.0, 5.0)]),
                read(5, 6),
            )
            .unwrap();
        assert_eq!(
            report.mode_changes,
            vec![
                (0, Stamp::TickTime),
                (2, Stamp::Model),
                (4, Stamp::TickTime)
            ]
        );
        assert_eq!(mode_changes(&report), "0:tick 2:model 4:tick");
    }

    /// A stamp earlier than the one before it is counted, across batches as well as within one.
    #[test]
    fn a_stamp_that_goes_back_is_counted() {
        let mut report = Report::default();
        report
            .add_batch(
                &batch(&[record(10, 0.0, 0.0, 1.0), record(5, 0.0, 0.0, 2.0)]),
                read(0, 1),
            )
            .unwrap();
        report
            .add_batch(&batch(&[record(4, 0.0, 0.0, 3.0)]), read(1, 2))
            .unwrap();
        report
            .add_batch(&batch(&[record(20, 0.0, 0.0, 4.0)]), read(2, 3))
            .unwrap();
        assert_eq!(report.backwards, 2);
    }

    /// Exact stamps, one per read, pass every check: the sim's model, with no error, is green.
    /// Fails on a check that bounds the stamps tighter than a read's own latency plus one period.
    #[test]
    fn exact_stamps_pass_all_three_checks() {
        // 4000 records (20 s), 10 per batch, a read latency of 0.6 ms.
        let report = clean_report(4000, 10, 600_000);
        let a = report.check_a(P);
        assert_eq!(a.violations, 0, "{a:?}");
        let b = report.check_b(P).expect("model points");
        assert!(b.rate_ppm.abs() <= b.bound_ppm, "{b:?}");
        assert!(all_pass(&report, P));
    }

    /// Fails on tick-time stamping, the pre-fix behaviour: a newest stamp taken at the previous tick,
    /// a whole read period before this read. Check (a) must see it as a stamp later-than-its-bracket
    /// lag, not pass it.
    #[test]
    fn a_stamp_taken_at_the_previous_tick_fails_check_a() {
        let mut report = Report::default();
        // Read at 100 ms, the stamp at the previous tick, 50 ms earlier.
        report
            .add_batch(
                &batch(&[stamped(Stamp::TickTime, 50_000_000, 0.0, 0.0, 9.8)]),
                read(100_000_000, 100_600_000),
            )
            .unwrap();
        let a = report.check_a(P);
        assert_eq!(a.violations, 1, "{a:?}");
        assert_eq!(a.max_lag_ns, 50_600_000, "read end minus the tick's stamp");
        assert!(
            a.max_lag_ns > a.bound_ns as i64,
            "the lag is larger than the bound"
        );
    }

    /// Fails on a planted rate error in the model's stamps: a stamp rate 1.5% fast (the fake's planted
    /// `stamp_error_ppm`). Check (b) must go red, and the bound is far below the error.
    #[test]
    fn a_planted_rate_error_fails_check_b() {
        let mut report = Report::default();
        let mut k = 0u64;
        // 20 s of 10-record batches; each stamp is 1.015 times its time: a 15 000 ppm rate error.
        while k < 4000 {
            let records: Vec<[u8; STAMPED_RECORD_LEN]> = (k..k + 10)
                .map(|i| record((i as f64 * P as f64 * 1.015) as u64, 0.0, 0.0, 9.8))
                .collect();
            let end = (k + 9) * P + P / 2 + 600_000;
            report
                .add_batch(&batch(&records), read(end - 600_000, end))
                .unwrap();
            k += 10;
        }
        let b = report.check_b(P).expect("model points");
        assert!((b.rate_ppm - 15_000.0).abs() < 200.0, "{b:?}");
        assert!(b.rate_ppm.abs() > b.bound_ppm, "{b:?}");
        assert!(!all_pass(&report, P));
    }

    /// The bound in check (b) is a bound on what the record numbers alone allow: a sub-bound error passes.
    #[test]
    fn a_rate_within_the_bound_passes_check_b() {
        let report = clean_report(4000, 10, 600_000);
        let b = report.check_b(P).unwrap();
        assert!(b.bound_ppm > 0.0 && b.bound_ppm < 1_000.0, "{b:?}");
    }

    /// Check 7: the spread of the offset to the nearest accelerometer stamp.
    #[test]
    fn check_7_spreads_the_offsets_to_the_nearest_accelerometer_stamp() {
        let accel = [0u64, 5_000_000, 10_000_000];
        // Offsets: 0, +1 ms, -1 ms: a spread of 2 ms.
        let gyro = [0u64, 6_000_000, 9_000_000];
        let c = check_7(&gyro, &accel).unwrap();
        assert_eq!((c.min_offset_ns, c.max_offset_ns), (-1_000_000, 1_000_000));
        assert_eq!(c.pairs, 3);
        assert_eq!(check_7(&[], &accel), None);
        // The gyroscope's stamps 1% longer than the accelerometer's: a rate of 10 000 ppm.
        let slow: Vec<u64> = (0..100u64).map(|i| i * 5_050_000).collect();
        let fast: Vec<u64> = (0..100u64).map(|i| i * 5_000_000).collect();
        let r = check_7(&slow, &fast).unwrap();
        assert!((r.rate_ppm - 10_000.0).abs() < 1.0, "{r:?}");
    }
}
