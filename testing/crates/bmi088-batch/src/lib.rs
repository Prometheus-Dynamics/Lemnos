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
//! The checks (B8.md, "Scoped verdicts"). Each record carries its mode, and a capture is judged per
//! mode: it passes only when the model's records pass the model's bounds and the tick-time records pass
//! theirs.
//!
//! - (a) Per batch, on its newest record, by that record's mode. The read started at `start` and
//!   returned at `end`, `L = end - start`, and the lag is `end - stamp`.
//!   - Model: the newest sample was taken before the FIFO was read, and not more than one sample
//!     period `P` before it, and the model knows the phase to `TOL` (1 ms, the driver's
//!     `PHASE_WIDTH_NS`). So the lag lies in `[-TOL, L + P + TOL]`.
//!   - Tick time: the stamp is the host time of the tick drain that stamped the record (the driver's
//!     `stamp_new`: the tick's time, less whole periods back from the newest). A read returns what the
//!     most recent tick drain stamped, and the driver's tick drains are at most one drain period apart:
//!     half the die's FIFO depth, plus up to one sample period for the drain's place on the sample grid
//!     (`Die::tick`). So the lag lies in `[-TOL, L + D]`, with `D = (depth / 2 + 1) * P`: 370 ms for the
//!     accelerometer and 255 ms for the gyroscope at 200 Hz. The lower end is the model's: a tick-time
//!     record after a model record is held to the model's last stamp.
//!   - A batch that filled the read's buffer is left out and counted: the driver kept newer records for the
//!     next read, so that batch's newest is not the newest the driver held (see [`BATCH_RECORDS`]).
//! - (b) Over the run, for the model's records. Each model batch's newest record gives one point: its
//!   record number, its stamp, and the end of its read. The least-squares slope of the stamps against the
//!   record number is compared with the slope of the read ends (the host's clock for the same samples). The
//!   difference is a rate, in ppm. Its bound is the most the difference can be given that each point's
//!   `end - stamp` lies in a band as wide as the widest point's `(end - start) + P + 2 TOL`: the largest
//!   least-squares slope such a band allows, from the points' own record numbers. A rate error beyond that
//!   bound is a fault, and a band-wide error at one batch cannot move the fit past it. A capture with
//!   fewer than two model batches has no rate, and that fails.
//! - (c) Order, over every record in every mode, across the switch between modes too: no stamp earlier
//!   than the one before it. Gaps, between two consecutive model records: none longer than 1.5 sample
//!   periods. A gap with a tick-time record on either side is counted and shown, not judged: a tick-time
//!   stamp is a tick's time less whole nominal periods, so the gap between two drains carries the tick's
//!   own timing and the chip's rate error over a whole drain, and a lost sample does not show in it.
//! - (d) The span of all the stamps against the run's wall window: within two poll periods and one
//!   sample period.
//! - Check 7 (gyroscope runs only): the two dies' stamps against each other. See [`check_7`].

use lemnos_contract_accelerometer::{STAMPED_RECORD_LEN, Stamp, Stamped};
use lemnos_core::{Decode, Reader};

/// The model's phase budget, in nanoseconds: the driver's `PHASE_WIDTH_NS` (1 ms). A stamp can be this
/// far from the bracket its reads give it, because the phase is only known to within that much.
pub const TOL_NS: u64 = 1_000_000;

/// The driver's tick drain period, in sample periods, for each die: half the die's FIFO depth (the
/// ring is the FIFO's depth in frames, `ACC_FIFO_FRAMES` = 146 and `GYRO_FIFO_FRAMES` = 100 in
/// `drivers/bmi088/src/chip.rs`, and `Die::tick` drains every `RING * period / 2`), plus one period: the
/// next drain is aimed at, or spread to, a point of the sample grid less than one period later.
pub const ACCEL_DRAIN_PERIODS: u64 = 146 / 2 + 1;
pub const GYRO_DRAIN_PERIODS: u64 = 100 / 2 + 1;

/// The range of chip rates the driver's stamping is tested over: 4 % slow to 4 % fast
/// (`drivers/bmi088/src/clock_tests.rs`, the two rate sweeps). Neither the driver nor its notes cite an
/// oscillator tolerance from the chip's datasheet, so this is the only stated range a die's rate can be
/// held to, and the ratio of two dies inside it is within `1.04 / 0.96 - 1`.
pub const RATE_RANGE: f64 = 0.04;

/// The bytes of the buffer each batch is read into, and the records it holds: 4 KiB, 141 records. A read that
/// returns this many filled its buffer, and the driver keeps the rest for the next read (`Die::deliver`), so that
/// batch's newest record is not the newest the driver holds. Checks (a) and (b), which judge a batch on its
/// newest record, leave such a batch out and count it. It happens when a capture begins on a backlog: the
/// first read of a capture begun more than 0.7 s after the die's bind, with nothing reading before it.
pub const BATCH_BYTES: usize = 4096;
pub const BATCH_RECORDS: usize = BATCH_BYTES / STAMPED_RECORD_LEN;

/// What a capture's stamps are held to: the sample period, and the driver's tick drain period for the
/// die that was captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// The sample period `P`, ns.
    pub period_ns: u64,
    /// The longest time between two tick drains `D`, ns (see [`ACCEL_DRAIN_PERIODS`]).
    pub drain_ns: u64,
}

impl Bounds {
    /// The bounds of a die that drains every `drain_periods` sample periods at most.
    pub fn new(period_ns: u64, drain_periods: u64) -> Self {
        Self {
            period_ns,
            drain_ns: drain_periods * period_ns,
        }
    }
}

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
    /// The poll the read was made in, counted from 0 over every read, empty ones included.
    poll: u64,
    /// The batch filled the read's buffer: its newest record is not the newest the driver held.
    full: bool,
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
    /// Each record's batch (counted from 0 over the non-empty batches) and that batch's read, in order: the
    /// capture file's `batch`, `read_start_ns` and `read_end_ns` columns.
    pub record_reads: Vec<(u64, ReadSpan)>,
    /// Each record's poll (counted from 0 over every read), in order: the capture file's `poll` column.
    pub record_polls: Vec<u64>,
    /// The first read that returned records: when it started, and its poll (how many reads came before it).
    /// The capture began that many poll periods before it. `None` for a file without read times.
    first_read: Option<(u64, u64)>,
}

/// One batch read that could not be decoded.
#[derive(Debug, PartialEq)]
pub enum BatchError {
    /// `bytes` is not a whole number of records: the chip or the transport handed over part of one.
    Partial { bytes: usize },
    /// A record did not decode.
    Undecodable { index: usize },
}

/// Check (a)'s figures for the batches of one mode.
#[derive(Debug, Clone, PartialEq)]
pub struct Lags {
    pub batches: u64,
    /// Batches whose newest stamp is outside its bounds.
    pub violations: u64,
    /// The smallest `end - stamp`, in ns: how long before its read's end the newest stamp is. Negative
    /// is a stamp after the read.
    pub min_lag_ns: i64,
    /// The largest `end - stamp`, in ns.
    pub max_lag_ns: i64,
    /// The widest allowed `end - stamp` over the batches, in ns.
    pub bound_ns: u64,
}

impl Lags {
    fn new() -> Self {
        Self {
            batches: 0,
            violations: 0,
            min_lag_ns: i64::MAX,
            max_lag_ns: i64::MIN,
            bound_ns: 0,
        }
    }

    fn add(&mut self, lag: i64, bound: u64) {
        self.batches += 1;
        self.min_lag_ns = self.min_lag_ns.min(lag);
        self.max_lag_ns = self.max_lag_ns.max(lag);
        self.bound_ns = self.bound_ns.max(bound);
        if lag < -(TOL_NS as i64) || lag > bound as i64 {
            self.violations += 1;
        }
    }

    fn done(mut self) -> Self {
        if self.batches == 0 {
            self.min_lag_ns = 0;
            self.max_lag_ns = 0;
        }
        self
    }
}

/// Check (a)'s figures: the model's batches against `L + P + TOL`, and the tick-time batches against
/// `L + D`.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckA {
    pub model: Lags,
    pub tick: Lags,
    /// Batches that filled the read's buffer: not judged (see [`BATCH_RECORDS`]).
    pub full: u64,
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

/// Check (d)'s figures and verdict: the stamps' span against the run's wall window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckD {
    /// Records the first read returned that were taken before the capture began: left out of the span.
    pub backlog_records: u64,
    pub span_ns: u64,
    pub elapsed_ns: u64,
    pub bound_ns: u64,
    /// The span minus the elapsed time: negative when the stamps cover less than the run.
    pub diff_ns: i64,
    pub pass: bool,
}

/// Check (d)'s bound in nanoseconds: 2 poll periods plus 1 sample period.
pub fn check_d_bound_ns(poll_ns: u64, sample_ns: u64) -> u64 {
    2 * poll_ns + sample_ns
}

/// Check (c)'s figures.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckC {
    /// Records stamped earlier than the record before them, in any mode.
    pub backwards: u64,
    /// Gaps longer than 1.5 sample periods between two consecutive model records.
    pub model_misses: u64,
    pub model_largest_gap_ns: u64,
    /// Gaps longer than 1.5 sample periods with a tick-time record on either side: shown, not judged.
    pub tick_gaps_over: u64,
    pub tick_largest_gap_ns: u64,
}

/// The tick drains a capture shows: each is a run of tick-time records one nominal period apart, whose
/// newest takes the tick's time. Information for the record, not a check.
#[derive(Debug, Clone, PartialEq)]
pub struct TickDrains {
    /// Tick drains seen.
    pub drains: u64,
    /// The median time between two drains, ns (`None` with fewer than two).
    pub median_period_ns: Option<u64>,
    /// Where each drain's tick time fell against the model's sample grid, carried back from the model's
    /// records at their own rate: the least and the most, ns (`None` without model records).
    pub grid_offset_ns: Option<(i64, i64)>,
    /// The median change of that offset from one drain to the next, ns: far under a sample period says
    /// the drains came at the same point of the grid again and again.
    pub median_grid_step_ns: Option<u64>,
}

/// Check 7's figures: the two dies' stamps against each other.
#[derive(Debug, Clone, PartialEq)]
pub struct Check7 {
    /// Gyroscope model records, each paired with its nearest accelerometer model record.
    pub pairs: u64,
    pub min_offset_ns: i64,
    pub max_offset_ns: i64,
    /// The bound on the spread of that offset: the accelerometer's own stamp spacing, plus `TOL` for
    /// each die.
    pub spread_bound_ns: u64,
    /// The gyroscope's stamp slope over the accelerometer's, minus one, in ppm.
    pub rate_ppm: f64,
    /// The bound on that ratio, in ppm (see [`RATE_RANGE`]).
    pub rate_bound_ppm: f64,
    /// Polls in which both dies returned a model batch.
    pub polls: u64,
    /// Polls whose two newest stamps are further apart than their two reads allow.
    pub poll_violations: u64,
    /// The least and the most of (gyroscope newest - accelerometer newest) over those polls, ns.
    pub poll_min_ns: i64,
    pub poll_max_ns: i64,
    /// The widest that difference was allowed to be, either way, over those polls, ns.
    pub poll_bound_ns: u64,
}

impl Check7 {
    /// The spread of the offset to the nearest accelerometer stamp.
    pub fn spread_ns(&self) -> u64 {
        (self.max_offset_ns - self.min_offset_ns).max(0) as u64
    }

    /// Whether the spread is within its bound.
    pub fn spread_ok(&self) -> bool {
        self.spread_ns() <= self.spread_bound_ns
    }

    /// Whether every poll's two newest stamps are within their reads' bounds, with at least one poll.
    pub fn polls_ok(&self) -> bool {
        self.polls > 0 && self.poll_violations == 0
    }

    /// Whether the rate ratio is inside the range the driver's stamping is tested over.
    pub fn rate_ok(&self) -> bool {
        self.rate_ppm.abs() <= self.rate_bound_ppm
    }

    pub fn pass(&self) -> bool {
        self.spread_ok() && self.polls_ok() && self.rate_ok()
    }
}

impl Report {
    /// Records one batch read of stamped records (`read_batch_stamped`) into `batch`, which the host
    /// read between `read.start_ns` and `read.end_ns`.
    pub fn add_batch(&mut self, batch: &[u8], read: ReadSpan) -> Result<(), BatchError> {
        let poll = self.reads;
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
        self.first_read.get_or_insert((read.start_ns, poll));
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
            self.count_stamp(batch_no, sample.at_ns, Some(stamped.stamp), read, poll);
            newest = Some((self.records - 1, sample.at_ns, stamped.stamp));
        }
        if let Some((index, stamp, mode)) = newest {
            self.points.push(Point {
                index,
                stamp,
                mode,
                read,
                poll,
                full: count >= BATCH_RECORDS,
            });
        }
        Ok(())
    }

    /// Counts one record: its stamp and, when known, its mode, in order, with the batch and read it came in.
    /// Used by `add_batch` (live) and `from_tsv` (a capture file), so both count the same way.
    fn count_stamp(
        &mut self,
        batch_no: u64,
        at_ns: u64,
        mode: Option<Stamp>,
        read: ReadSpan,
        poll: u64,
    ) {
        if let Some(mode) = mode {
            if self.last_mode != Some(mode) {
                // The first record of this batch that differs from the one before it.
                self.mode_changes.push((batch_no, mode));
                self.last_mode = Some(mode);
            }
            match mode {
                Stamp::TickTime => self.tick_records += 1,
                Stamp::Model => self.model_records += 1,
            }
            self.modes.push(mode);
        }
        if self.stamps.last().is_some_and(|&previous| at_ns < previous) {
            self.backwards += 1;
        }
        self.stamps.push(at_ns);
        self.record_reads.push((batch_no, read));
        self.record_polls.push(poll);
        self.records += 1;
    }

    /// The average time between consecutive stamps, in nanoseconds (`None` with fewer than two).
    pub fn mean_period_ns(&self) -> Option<f64> {
        let (first, last) = (self.stamps.first()?, self.stamps.last()?);
        (self.stamps.len() >= 2).then(|| (last - first) as f64 / (self.stamps.len() - 1) as f64)
    }

    /// The largest gap between consecutive stamps, in nanoseconds, in any mode.
    pub fn largest_gap_ns(&self) -> u64 {
        self.stamps
            .windows(2)
            .map(|pair| pair[1].saturating_sub(pair[0]))
            .max()
            .unwrap_or(0)
    }

    /// Whether record `i` is a tick-time record. A capture without modes (an older file) has none.
    fn is_tick(&self, i: usize) -> bool {
        self.modes.get(i) == Some(&Stamp::TickTime)
    }

    /// Check (a): each batch's newest stamp against its read, by the stamp's mode.
    pub fn check_a(&self, bounds: Bounds) -> CheckA {
        let mut model = Lags::new();
        let mut tick = Lags::new();
        let full = self.points.iter().filter(|p| p.full).count() as u64;
        for point in self.points.iter().filter(|p| !p.full) {
            let read_ns = point.read.end_ns - point.read.start_ns;
            let lag = point.read.end_ns as i64 - point.stamp as i64;
            match point.mode {
                Stamp::Model => model.add(lag, read_ns + bounds.period_ns + TOL_NS),
                Stamp::TickTime => tick.add(lag, read_ns + bounds.drain_ns),
            }
        }
        CheckA {
            model: model.done(),
            tick: tick.done(),
            full,
        }
    }

    /// Check (b): the model's stamps' rate against the host's, over the run. `None` with fewer than two
    /// model batches, which is not a measurement.
    pub fn check_b(&self, period_ns: u64) -> Option<CheckB> {
        let model: Vec<&Point> = self
            .points
            .iter()
            .filter(|p| p.mode == Stamp::Model && !p.full)
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

    /// Check (c): no stamp goes back in any mode, and no gap between two model records is over 1.5
    /// sample periods. A gap with a tick-time record on either side is counted apart.
    pub fn check_c(&self, period_ns: u64) -> CheckC {
        let limit = period_ns + period_ns / 2;
        let mut out = CheckC {
            backwards: self.backwards,
            model_misses: 0,
            model_largest_gap_ns: 0,
            tick_gaps_over: 0,
            tick_largest_gap_ns: 0,
        };
        for (i, pair) in self.stamps.windows(2).enumerate() {
            let gap = pair[1].saturating_sub(pair[0]);
            if self.is_tick(i) || self.is_tick(i + 1) {
                out.tick_largest_gap_ns = out.tick_largest_gap_ns.max(gap);
                out.tick_gaps_over += u64::from(gap > limit);
            } else {
                out.model_largest_gap_ns = out.model_largest_gap_ns.max(gap);
                out.model_misses += u64::from(gap > limit);
            }
        }
        out
    }

    /// Check (d): the span of the stamps against the run's wall window `elapsed_ns`. The first stamp is taken
    /// within one poll period of the run's start and the last within one poll period of its end, and each
    /// stamp is taken to within one sample period, so `|span - elapsed| <= 2 * poll + sample`. This needs no read
    /// times: it is the whole-run drift that a 1 ms bound used to call a fail on any run.
    ///
    /// The span is of the capture's own records. The first read returns what the driver's ring held before the
    /// capture began (up to the ring's depth, 0.73 s of the accelerometer's samples, when nothing read it
    /// before), and those stamps are older than the window by as much. With read times, the capture began at
    /// its first read: the first read that returned records, less one poll period for each read before it (in
    /// tick time the reads before the first tick drain return nothing). A record stamped more than one sample
    /// period before that is the backlog: it is counted, and the span starts at the first record after it. A
    /// file without read times and polls cannot tell a backlog from a drift, so its span is of every record.
    pub fn check_d(&self, elapsed_ns: u64, poll_ns: u64, sample_ns: u64) -> CheckD {
        let backlog = self.first_read.map_or(0, |(start, poll)| {
            let began = start.saturating_sub(poll.saturating_mul(poll_ns));
            let floor = began.saturating_sub(sample_ns);
            self.stamps.iter().take_while(|&&at| at < floor).count()
        });
        let span = match (self.stamps.get(backlog), self.stamps.last()) {
            (Some(first), Some(last)) => last.saturating_sub(*first),
            _ => 0,
        };
        let bound = check_d_bound_ns(poll_ns, sample_ns);
        let diff = span as i128 - elapsed_ns as i128;
        CheckD {
            backlog_records: backlog as u64,
            span_ns: span,
            elapsed_ns,
            bound_ns: bound,
            diff_ns: diff as i64,
            pass: self.records > 0 && diff.unsigned_abs() <= u128::from(bound),
        }
    }

    /// Gaps longer than `1.5` periods at `period_ns`, in any mode.
    pub fn misses(&self, period_ns: u64) -> u64 {
        let limit = period_ns + period_ns / 2;
        self.stamps
            .windows(2)
            .filter(|pair| pair[1].saturating_sub(pair[0]) > limit)
            .count() as u64
    }

    /// The span from the first stamp to the last, in nanoseconds.
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

    /// The tick drains the capture shows (see [`TickDrains`]).
    pub fn tick_drains(&self, period_ns: u64) -> TickDrains {
        // A drain's newest record: a tick-time record whose next record is not one nominal period
        // later in tick time. A record held to the one before it (the same stamp) is not a new drain.
        let mut newest: Vec<(usize, u64)> = Vec::new();
        for i in 0..self.stamps.len() {
            if !self.is_tick(i) {
                continue;
            }
            let run_goes_on =
                self.is_tick(i + 1) && self.stamps[i + 1] == self.stamps[i] + period_ns;
            if !run_goes_on && newest.last().is_none_or(|&(_, at)| at != self.stamps[i]) {
                newest.push((i, self.stamps[i]));
            }
        }
        let median = |mut values: Vec<u64>| {
            values.sort_unstable();
            (!values.is_empty()).then(|| values[values.len() / 2])
        };
        let periods: Vec<u64> = newest
            .windows(2)
            .map(|w| w[1].1.saturating_sub(w[0].1))
            .collect();
        // The model's grid: its stamps against their record numbers, carried back to the tick drains.
        let model: Vec<(f64, f64)> = (0..self.stamps.len())
            .filter(|&i| self.modes.get(i) == Some(&Stamp::Model))
            .map(|i| (i as f64, self.stamps[i] as f64 - self.stamps[0] as f64))
            .collect();
        let mut grid_offset_ns = None;
        let mut median_grid_step_ns = None;
        if model.len() >= 2 && !newest.is_empty() {
            let xs: Vec<f64> = model.iter().map(|p| p.0).collect();
            let ys: Vec<f64> = model.iter().map(|p| p.1).collect();
            let rate = slope(&xs, &ys);
            let (mx, my) = (mean(&xs), mean(&ys));
            let offsets: Vec<i64> = newest
                .iter()
                .map(|&(i, at)| {
                    let grid = my + rate * (i as f64 - mx);
                    (at as f64 - self.stamps[0] as f64 - grid) as i64
                })
                .collect();
            let (lo, hi) = offsets
                .iter()
                .fold((i64::MAX, i64::MIN), |(lo, hi), &o| (lo.min(o), hi.max(o)));
            grid_offset_ns = Some((lo, hi));
            median_grid_step_ns = median(offsets.windows(2).map(|w| w[1].abs_diff(w[0])).collect());
        }
        TickDrains {
            drains: newest.len() as u64,
            median_period_ns: median(periods),
            grid_offset_ns,
            median_grid_step_ns,
        }
    }

    /// The tick drains after the model's first record: each is a drain the model did not stamp (a
    /// bracket wider than the driver's 1 ms limit, or a re-anchor).
    pub fn tick_returns(&self) -> u64 {
        let mut seen_model = false;
        let mut returns = 0;
        for (_, mode) in &self.mode_changes {
            match mode {
                Stamp::Model => seen_model = true,
                Stamp::TickTime if seen_model => returns += 1,
                Stamp::TickTime => {}
            }
        }
        returns
    }
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

/// The least-squares slope of `ys` against `xs`.
fn slope(xs: &[f64], ys: &[f64]) -> f64 {
    let (mx, my) = (mean(xs), mean(ys));
    let cov: f64 = xs.iter().zip(ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    let var: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
    cov / var
}

/// Check 7: the two dies' model stamps against each other. `None` when either die has fewer than two
/// model stamps, or their spans do not overlap. Three things are measured, and each has its bound:
///
/// - The spread. Each gyroscope stamp is compared with the nearest accelerometer stamp. Two sample grids
///   that are not locked to each other put that offset anywhere within half an accelerometer period
///   either way, so its spread (max minus min) over a run is up to one accelerometer period, plus each
///   die's stamp error (`TOL` each). The period is the accelerometer's own stamp spacing in this run.
///   What this can show is small: the offset is to the nearest stamp, so the spread only passes one
///   period where the accelerometer's stamps have a hole. It cannot see a constant offset between the
///   dies (the nearest stamp is found again after any shift), and it cannot see a drift.
/// - The same poll. Both dies are read in each poll, and check (a) holds each die's newest stamp to its
///   own read: within `[start - P - TOL, end + TOL]`. So the gyroscope's newest less the accelerometer's
///   newest, from one poll, lies within `[g.start - a.end - P - 2 TOL, g.end - a.start + P + 2 TOL]`.
///   This is the one that sees a constant offset between the dies.
/// - The rate ratio: the gyroscope's stamp slope over the accelerometer's. The dies have separate
///   oscillators, so the ratio is their rates' ratio. No oscillator tolerance is cited by the driver or
///   its notes, so the bound is the range the driver's stamping is tested over ([`RATE_RANGE`]): two
///   dies inside it differ by at most `(1 + r) / (1 - r) - 1`.
pub fn check_7(gyro: &Report, accel: &Report, period_ns: u64) -> Option<Check7> {
    let (g, a) = (gyro.model_stamps(), accel.model_stamps());
    if g.len() < 2 || a.len() < 2 {
        return None;
    }
    let mut min = i64::MAX;
    let mut max = i64::MIN;
    let mut pairs = 0;
    // Only the gyroscope stamps inside the accelerometer's span: one before its first stamp or after
    // its last has no accelerometer stamp on one side, and its offset would be the captures' ends.
    let (first, last) = (a[0], a[a.len() - 1]);
    for &stamp in g.iter().filter(|&&stamp| stamp >= first && stamp <= last) {
        pairs += 1;
        let at = a.partition_point(|&x| x < stamp);
        let near = [at.checked_sub(1), (at < a.len()).then_some(at)]
            .into_iter()
            .flatten()
            .map(|i| a[i])
            .min_by_key(|&x| x.abs_diff(stamp))?;
        let offset = stamp as i64 - near as i64;
        min = min.min(offset);
        max = max.max(offset);
    }
    if pairs == 0 {
        return None;
    }
    let index = |n: usize| (0..n).map(|i| i as f64).collect::<Vec<f64>>();
    let stamps = |s: &[u64]| {
        let base = s[0] as f64;
        s.iter().map(|&v| v as f64 - base).collect::<Vec<f64>>()
    };
    let gyro_rate = slope(&index(g.len()), &stamps(&g));
    let accel_rate = slope(&index(a.len()), &stamps(&a));

    let mut out = Check7 {
        pairs,
        min_offset_ns: min,
        max_offset_ns: max,
        spread_bound_ns: accel_rate.max(0.0) as u64 + 2 * TOL_NS,
        rate_ppm: (gyro_rate / accel_rate - 1.0) * 1e6,
        rate_bound_ppm: ((1.0 + RATE_RANGE) / (1.0 - RATE_RANGE) - 1.0) * 1e6,
        polls: 0,
        poll_violations: 0,
        poll_min_ns: i64::MAX,
        poll_max_ns: i64::MIN,
        poll_bound_ns: 0,
    };
    // The polls in which both dies returned a batch whose newest record is the model's.
    let model = |r: &Report| -> Vec<Point> {
        r.points
            .iter()
            .filter(|p| p.mode == Stamp::Model && !p.full)
            .copied()
            .collect()
    };
    let accel_points = model(accel);
    let mut next = 0;
    for gp in model(gyro) {
        while next < accel_points.len() && accel_points[next].poll < gp.poll {
            next += 1;
        }
        let Some(ap) = accel_points.get(next).filter(|ap| ap.poll == gp.poll) else {
            continue;
        };
        let slack = (period_ns + 2 * TOL_NS) as i64;
        let low = gp.read.start_ns as i64 - ap.read.end_ns as i64 - slack;
        let high = gp.read.end_ns as i64 - ap.read.start_ns as i64 + slack;
        let diff = gp.stamp as i64 - ap.stamp as i64;
        out.polls += 1;
        out.poll_min_ns = out.poll_min_ns.min(diff);
        out.poll_max_ns = out.poll_max_ns.max(diff);
        out.poll_bound_ns = out
            .poll_bound_ns
            .max(low.unsigned_abs())
            .max(high.unsigned_abs());
        if diff < low || diff > high {
            out.poll_violations += 1;
        }
    }
    if out.polls == 0 {
        out.poll_min_ns = 0;
        out.poll_max_ns = 0;
    }
    Some(out)
}

/// A capture file read back: the report, and which of its columns the file had.
#[derive(Debug, Clone, PartialEq)]
pub struct Replay {
    pub report: Report,
    /// The file has a `mode` column, so each record's stamping mode is known.
    pub has_modes: bool,
    /// The file has the `batch`, `read_start_ns` and `read_end_ns` columns: checks (a) and (b) can run.
    pub has_read_times: bool,
    /// The file has a `poll` column: check 7 can pair the two dies' reads.
    pub has_polls: bool,
}

/// How a capture came out. `Fail`: an evaluated check failed. `Incomplete`: none failed, but a check
/// could not be evaluated from the file, which is not a pass. `Pass`: every check ran and passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Incomplete,
    Fail,
}

/// A capture file as the live tool writes it: one row per record, with its stamp, its mode, and the batch,
/// read and poll it came in. `from_tsv` reads it back.
pub fn stamps_tsv(report: &Report) -> String {
    let mut out = String::from("at_ns\tmode\tbatch\tread_start_ns\tread_end_ns\tpoll\n");
    for (((stamp, mode), (batch, read)), poll) in report
        .stamps
        .iter()
        .zip(&report.modes)
        .zip(&report.record_reads)
        .zip(&report.record_polls)
    {
        let mode = match mode {
            Stamp::TickTime => "tick",
            Stamp::Model => "model",
        };
        out.push_str(&format!(
            "{stamp}\t{mode}\t{batch}\t{}\t{}\t{poll}\n",
            read.start_ns, read.end_ns
        ));
    }
    out
}

/// One row of a capture file.
struct Row {
    at_ns: u64,
    mode: Option<Stamp>,
    batch: u64,
    read: ReadSpan,
    poll: u64,
}

/// Reads a capture file into a report. The `at_ns` column is required. A `mode` column gives the tick and
/// model counts, and lets each check judge each mode by its own bounds. The `batch`, `read_start_ns` and
/// `read_end_ns` columns give each batch's newest record and its read, for checks (a) and (b). A `poll`
/// column pairs two dies' reads for check 7. A file without those columns (the first board run's, for
/// one) still has its stamps counted, so check (c) runs on it. A file that is not a capture is an error,
/// never a partial report.
pub fn from_tsv(text: &str) -> Result<Replay, String> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let header: Vec<&str> = lines
        .next()
        .ok_or("the file is empty")?
        .split('\t')
        .collect();
    let column = |name: &str| header.iter().position(|h| *h == name);
    let at = column("at_ns").ok_or("no at_ns column")?;
    let mode_col = column("mode");
    let poll_col = column("poll");
    let read_cols = match (
        column("batch"),
        column("read_start_ns"),
        column("read_end_ns"),
    ) {
        (Some(batch), Some(start), Some(end)) => Some((batch, start, end)),
        _ => None,
    };
    let mut rows = Vec::new();
    for (i, line) in lines.enumerate() {
        let line_no = i + 2;
        let fields: Vec<&str> = line.split('\t').collect();
        let number = |col: usize| -> Result<u64, String> {
            let text = fields
                .get(col)
                .ok_or_else(|| format!("line {line_no}: too few fields"))?;
            text.parse::<u64>()
                .map_err(|_| format!("line {line_no}: {text:?} is not a number"))
        };
        let at_ns = number(at)?;
        let mode = match mode_col {
            None => None,
            Some(col) => match fields.get(col).copied() {
                Some("tick") => Some(Stamp::TickTime),
                Some("model") => Some(Stamp::Model),
                other => {
                    return Err(format!(
                        "line {line_no}: mode {other:?} is not tick or model"
                    ));
                }
            },
        };
        let (batch, read) = match read_cols {
            None => (
                0,
                ReadSpan {
                    start_ns: 0,
                    end_ns: 0,
                },
            ),
            Some((b, s, e)) => (
                number(b)?,
                ReadSpan {
                    start_ns: number(s)?,
                    end_ns: number(e)?,
                },
            ),
        };
        let poll = match poll_col {
            None => batch,
            Some(col) => number(col)?,
        };
        rows.push(Row {
            at_ns,
            mode,
            batch,
            read,
            poll,
        });
    }
    if rows.is_empty() {
        return Err("no records".into());
    }
    let mut report = Report::default();
    for row in &rows {
        report.count_stamp(row.batch, row.at_ns, row.mode, row.read, row.poll);
    }
    if read_cols.is_some() {
        // The capture's first read is known only with the `poll` column: without it the reads that returned
        // nothing are not counted, and check (d) takes every record.
        if poll_col.is_some() {
            report.first_read = Some((rows[0].read.start_ns, rows[0].poll));
        }
        // One point per batch: its newest record, with the batch's read, as `add_batch` makes them.
        let mut start = 0;
        while start < rows.len() {
            let batch = rows[start].batch;
            let end = rows[start..]
                .iter()
                .position(|r| r.batch != batch)
                .map_or(rows.len(), |p| start + p);
            report.max_per_read = report.max_per_read.max(end - start);
            let newest = &rows[end - 1];
            report.points.push(Point {
                index: (end - 1) as u64,
                stamp: newest.at_ns,
                // Without a mode column the mode is not known, and checks (a) and (b) are not run.
                mode: newest.mode.unwrap_or(Stamp::Model),
                read: newest.read,
                poll: newest.poll,
                full: end - start >= BATCH_RECORDS,
            });
            start = end;
        }
    }
    Ok(Replay {
        report,
        has_modes: mode_col.is_some(),
        has_read_times: read_cols.is_some(),
        has_polls: poll_col.is_some(),
    })
}

/// A capture as the checks see it: the report, the columns it has (a live capture has them all), the
/// run's wall time and poll period when they are known, and the die's bounds.
pub struct Capture<'a> {
    pub report: &'a Report,
    pub has_modes: bool,
    pub has_read_times: bool,
    /// The run's wall time and poll period, ns, for check (d).
    pub timing: Option<(u64, u64)>,
    pub bounds: Bounds,
}

impl<'a> Capture<'a> {
    /// A live capture: every column, and its own timing.
    pub fn live(report: &'a Report, bounds: Bounds, elapsed_ns: u64, poll_ns: u64) -> Self {
        Self {
            report,
            has_modes: true,
            has_read_times: true,
            timing: Some((elapsed_ns, poll_ns)),
            bounds,
        }
    }

    /// A capture file read back.
    pub fn replayed(replay: &'a Replay, bounds: Bounds, timing: Option<(u64, u64)>) -> Self {
        Self {
            report: &replay.report,
            has_modes: replay.has_modes,
            has_read_times: replay.has_read_times,
            timing,
            bounds,
        }
    }
}

fn ms(ns: impl Into<i128>) -> String {
    format!("{:.3}", ns.into() as f64 / 1e6)
}

/// The check rows of a capture, and how it came out. A row is `key`, `status`, `value`, `note`. A
/// verdict row's status is `pass`, `fail`, `none` (the capture has no record of that mode) or
/// `not_evaluated` (the file lacks what the check needs); a capture with a `not_evaluated` check is
/// `Incomplete`, never a pass. The capture is judged per mode: `b8.model.verdict` is the model's
/// records against the model's bounds, `b8.tick.verdict` the tick-time records against theirs, and
/// `b8.verdict` passes only when both do, no stamp goes back anywhere, and check (d) holds.
pub fn check_rows(capture: &Capture<'_>) -> (String, Outcome) {
    let report = capture.report;
    let Bounds {
        period_ns,
        drain_ns,
    } = capture.bounds;
    let mut out = String::new();
    let mut row = |key: &str, status: &str, value: String, note: &str| {
        out.push_str(&format!("{key}\t{status}\t{value}\t{note}\n"));
    };
    let mut failed = false;
    let mut incomplete = false;
    let has_tick = report.tick_records > 0;

    if capture.has_modes {
        row(
            "b8.tick_records",
            "measured",
            report.tick_records.to_string(),
            "records stamped from the host's tick time (the sample clock had not converged, or re-anchored)",
        );
        row(
            "b8.model_records",
            "measured",
            report.model_records.to_string(),
            "records stamped from the sample clock's model",
        );
        row(
            "b8.mode_changes",
            "measured",
            mode_changes(report),
            "batch:mode at which each stamping mode began (tick or model)",
        );
        row(
            "b8.tick_returns",
            "measured",
            report.tick_returns().to_string(),
            "times the stamps went back to tick time after the model began: a drain whose bracket was over the driver's 1 ms limit, or a re-anchor",
        );
        let drains = report.tick_drains(period_ns);
        row(
            "b8.tick.drains",
            "measured",
            drains.drains.to_string(),
            "tick drains seen in tick time (each a run of records one nominal period apart)",
        );
        if let Some(period) = drains.median_period_ns {
            row(
                "b8.tick.drain_period_ms",
                "measured",
                ms(period),
                "median time between two tick drains",
            );
        }
        if let Some((lo, hi)) = drains.grid_offset_ns {
            row(
                "b8.tick.grid_offset_ms",
                "info",
                format!("{} {}", ms(lo), ms(hi)),
                "least and most of (a drain's tick time - the model's time for its newest sample, carried back at the model's rate): where on the sample grid the drains started",
            );
        }
        if let Some(step) = drains.median_grid_step_ns {
            row(
                "b8.tick.grid_step_ms",
                "info",
                ms(step),
                "median change of that offset from one drain to the next: far under a sample period means the drains came at the same point of the grid, so each repeats the last one's phase bound",
            );
        }
    } else {
        row(
            "b8.mode",
            "not_evaluated",
            String::new(),
            "the capture has no mode column: tick and model records are not told apart, and every record is held to the model's bounds",
        );
    }
    if let Some(period) = report.mean_period_ns() {
        row(
            "b8.mean_period_us",
            "measured",
            format!("{:.2}", period / 1e3),
            "average between consecutive stamps",
        );
    }

    // Check (a), per mode.
    let (mut a_model, mut a_tick) = (None, None);
    if capture.has_read_times && capture.has_modes {
        let a = report.check_a(capture.bounds);
        let mut lags = |name: &str, lags: &Lags, bound: &str| {
            row(
                &format!("b8.a.{name}.batches"),
                "measured",
                lags.batches.to_string(),
                "non-empty batches whose newest record is of this mode",
            );
            if lags.batches > 0 {
                row(
                    &format!("b8.a.{name}.min_lag_ms"),
                    "measured",
                    ms(lags.min_lag_ns),
                    "smallest (read end - newest stamp): negative is a stamp after its read",
                );
                row(
                    &format!("b8.a.{name}.max_lag_ms"),
                    "measured",
                    ms(lags.max_lag_ns),
                    "largest (read end - newest stamp)",
                );
                row(
                    &format!("b8.a.{name}.bound_ms"),
                    "measured",
                    ms(lags.bound_ns),
                    bound,
                );
            }
            row(
                &format!("b8.a.{name}.violations"),
                "measured",
                lags.violations.to_string(),
                "batches whose (read end - newest stamp) is under -1 ms or over the bound",
            );
        };
        lags(
            "model",
            &a.model,
            "widest allowed lag: read latency + one sample period + 1 ms",
        );
        lags(
            "tick",
            &a.tick,
            "widest allowed lag: read latency + one tick drain period (half the FIFO depth plus one sample period)",
        );
        row(
            "b8.a.full_batches",
            "measured",
            a.full.to_string(),
            "batches that filled the read's buffer (141 records): the driver held newer records back for the next read, so these are not judged on their newest",
        );
        a_model = Some(a.model.batches > 0 && a.model.violations == 0);
        a_tick = Some(a.tick.violations == 0);
        let pass = a_model == Some(true) && a_tick == Some(true);
        row(
            "b8.a.verdict",
            verdict(pass),
            String::new(),
            "check (a): each mode's batches within that mode's bound, with at least one model batch",
        );
        failed |= !pass;
    } else {
        row(
            "b8.a.verdict",
            "not_evaluated",
            String::new(),
            if capture.has_read_times {
                "check (a) not evaluated: the capture has no mode column, so a batch's bound is not known"
            } else {
                "check (a) not evaluated: the capture has no read times"
            },
        );
        incomplete = true;
    }

    // Check (b), the model's rate.
    let mut b_pass = None;
    if !capture.has_read_times {
        row(
            "b8.b.verdict",
            "not_evaluated",
            String::new(),
            "check (b) not evaluated: the capture has no read times",
        );
        incomplete = true;
    } else if !capture.has_modes {
        row(
            "b8.b.verdict",
            "not_evaluated",
            String::new(),
            "check (b) not evaluated: it fits the model's records, and the capture has no mode column",
        );
        incomplete = true;
    } else {
        match report.check_b(period_ns) {
            Some(b) => {
                row(
                    "b8.b.points",
                    "measured",
                    b.points.to_string(),
                    "model batches in the rate fit",
                );
                row(
                    "b8.b.rate_ppm",
                    "measured",
                    format!("{:.1}", b.rate_ppm),
                    "stamps' fitted rate against the host's, over the same records (positive: stamps fast)",
                );
                row(
                    "b8.b.bound_ppm",
                    "measured",
                    format!("{:.1}", b.bound_ppm),
                    "bound on that rate from the record numbers and the widest read band",
                );
                let pass = b.rate_ppm.abs() <= b.bound_ppm;
                row("b8.b.verdict", verdict(pass), String::new(), "check (b)");
                b_pass = Some(pass);
                failed |= !pass;
            }
            // No rate measured is a fail, never a pass: the model never published in this capture.
            None => {
                row(
                    "b8.b.verdict",
                    "fail",
                    String::new(),
                    "check (b): fewer than two model batches, so no rate was measured: the model did not publish in this capture",
                );
                b_pass = Some(false);
                failed = true;
            }
        }
    }

    // Check (c): the order over every record, the gaps over the model's.
    let c = report.check_c(period_ns);
    row(
        "b8.c.backwards",
        "measured",
        c.backwards.to_string(),
        "records stamped earlier than the record before them, in any mode and across a mode change",
    );
    row(
        "b8.c.model.misses",
        "measured",
        c.model_misses.to_string(),
        "gaps over 1.5 sample periods between two consecutive model records",
    );
    row(
        "b8.c.model.largest_gap_ms",
        "measured",
        ms(c.model_largest_gap_ns),
        "largest gap between two consecutive model records",
    );
    if has_tick {
        row(
            "b8.c.tick.gaps_over",
            "info",
            c.tick_gaps_over.to_string(),
            "gaps over 1.5 sample periods with a tick-time record on either side: shown, not judged (they fall between two tick drains)",
        );
        row(
            "b8.c.tick.largest_gap_ms",
            "info",
            ms(c.tick_largest_gap_ns),
            "largest gap with a tick-time record on either side",
        );
    }
    let c_pass = c.backwards == 0 && c.model_misses == 0 && report.records > 0;
    row(
        "b8.c.verdict",
        verdict(c_pass),
        String::new(),
        "check (c): no backwards stamp in any mode, and no model gap over 1.5 sample periods",
    );
    failed |= !c_pass;

    // Check (d) needs the run's wall time and poll period.
    match capture.timing {
        Some((elapsed, poll)) => {
            let d = report.check_d(elapsed, poll, period_ns);
            row(
                "b8.d.backlog_records",
                "measured",
                d.backlog_records.to_string(),
                "records stamped over one sample period before the capture's first read: taken before the capture, and left out of the span",
            );
            row(
                "b8.d.span_ms",
                "measured",
                ms(d.span_ns),
                "first stamp after the backlog to the last stamp",
            );
            row(
                "b8.d.elapsed_ms",
                "measured",
                ms(d.elapsed_ns),
                "the run's wall time",
            );
            row(
                "b8.d.span_vs_elapsed_ms",
                "measured",
                ms(d.diff_ns),
                "span minus elapsed",
            );
            row(
                "b8.d.bound_ms",
                "measured",
                ms(d.bound_ns),
                "2 poll periods plus 1 sample period",
            );
            row("b8.d.verdict", verdict(d.pass), String::new(), "check (d)");
            failed |= !d.pass;
        }
        None => {
            row(
                "b8.d.verdict",
                "not_evaluated",
                String::new(),
                "check (d) not evaluated: give --elapsed-s and --poll-ms (the run's wall time and poll period)",
            );
            incomplete = true;
        }
    }

    // The verdict of each mode, by its own bounds.
    let model_verdict = match (a_model, b_pass) {
        (Some(a), Some(b)) => verdict(a && b && c.model_misses == 0),
        _ => "not_evaluated",
    };
    row(
        "b8.model.verdict",
        model_verdict,
        String::new(),
        "the model's records: (a) within read latency + P + 1 ms, (b) the rate, (c) no gap over 1.5 P",
    );
    let tick_verdict = match a_tick {
        _ if capture.has_modes && !has_tick => "none",
        Some(a) => verdict(a),
        None => "not_evaluated",
    };
    row(
        "b8.tick.verdict",
        tick_verdict,
        String::new(),
        &format!(
            "the tick-time records: (a) within read latency + {} ms, one tick drain period; none: the capture has no tick-time record",
            ms(drain_ns)
        ),
    );

    let outcome = if failed {
        Outcome::Fail
    } else if incomplete {
        Outcome::Incomplete
    } else {
        Outcome::Pass
    };
    row(
        "b8.verdict",
        outcome.verdict(),
        String::new(),
        "each mode by its own bounds, no backwards stamp in any mode, and check (d)",
    );
    (out, outcome)
}

impl Outcome {
    /// The verdict word of a row.
    pub fn verdict(self) -> &'static str {
        match self {
            Outcome::Pass => "pass",
            Outcome::Incomplete => "not_evaluated",
            Outcome::Fail => "fail",
        }
    }

    /// The worse of two outcomes: a fail over an incomplete one, over a pass.
    #[must_use]
    pub fn and(self, other: Outcome) -> Outcome {
        match (self, other) {
            (Outcome::Fail, _) | (_, Outcome::Fail) => Outcome::Fail,
            (Outcome::Incomplete, _) | (_, Outcome::Incomplete) => Outcome::Incomplete,
            _ => Outcome::Pass,
        }
    }

    /// The tool's exit code for a run that ended with this outcome.
    pub fn exit_code(self) -> i32 {
        match self {
            Outcome::Pass => 0,
            Outcome::Incomplete => 3,
            Outcome::Fail => 5,
        }
    }
}

/// The last row of a run: its status, the records it had, and what the status means.
pub fn status_row(outcome: Outcome, what: &str) -> String {
    let (status, note) = match outcome {
        Outcome::Fail => (
            "checks_failed",
            "one or more evaluated checks failed: see the verdict rows",
        ),
        Outcome::Incomplete => (
            "not_fully_measured",
            "no check failed, but one or more could not be evaluated: not a pass",
        ),
        Outcome::Pass => ("measured", "every check ran and passed"),
    };
    format!("b8.status\t{status}\t{what}\t{note}\n")
}

/// The rows of `text` with their keys moved under `prefix`: `b8.a.verdict` becomes `b8.accel.a.verdict`
/// for `prefix` = `b8.accel.`. For the second die of a gyroscope run.
pub fn prefixed(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|line| format!("{prefix}{}\n", line.strip_prefix("b8.").unwrap_or(line)))
        .collect()
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

/// The rows a live capture has and a file does not: what the reads returned.
pub fn read_rows(report: &Report) -> String {
    let mut out = String::new();
    let mut row = |key: &str, value: String, note: &str| {
        out.push_str(&format!("{key}\tmeasured\t{value}\t{note}\n"));
    };
    row(
        "b8.records",
        report.records.to_string(),
        "records decoded from the FIFO",
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
    out
}

/// The check-7 rows, for a gyroscope capture with the accelerometer's records from the same polls, and
/// whether check 7 passes. `has_polls` is false for files without a `poll` column: the same-poll part is
/// then `not_evaluated`, and check 7 is not a pass.
pub fn check7_rows(
    gyro: &Report,
    accel: &Report,
    period_ns: u64,
    has_polls: bool,
) -> (String, Outcome) {
    let Some(c) = check_7(gyro, accel, period_ns) else {
        return (
            "b8.check7.verdict\tfail\t\tcheck 7: fewer than two model records on one of the dies, so the dies were not compared\n"
                .to_string(),
            Outcome::Fail,
        );
    };
    let mut out = String::new();
    let mut row = |key: &str, status: &str, value: String, note: &str| {
        out.push_str(&format!("{key}\t{status}\t{value}\t{note}\n"));
    };
    row(
        "b8.check7.pairs",
        "measured",
        c.pairs.to_string(),
        "gyroscope model records, each paired with its nearest accelerometer model record",
    );
    row(
        "b8.check7.spread_ms",
        "measured",
        ms(c.spread_ns()),
        "max minus min of (gyroscope stamp - nearest accelerometer stamp)",
    );
    row(
        "b8.check7.min_offset_ms",
        "measured",
        ms(c.min_offset_ns),
        "smallest offset",
    );
    row(
        "b8.check7.max_offset_ms",
        "measured",
        ms(c.max_offset_ns),
        "largest offset",
    );
    row(
        "b8.check7.spread_bound_ms",
        "measured",
        ms(c.spread_bound_ns),
        "one accelerometer period (its own stamp spacing in this run) plus 1 ms for each die's stamp",
    );
    row(
        "b8.check7.spread_verdict",
        verdict(c.spread_ok()),
        String::new(),
        "the spread within its bound",
    );
    let mut outcome = if c.spread_ok() {
        Outcome::Pass
    } else {
        Outcome::Fail
    };
    if has_polls {
        row(
            "b8.check7.polls",
            "measured",
            c.polls.to_string(),
            "polls in which both dies returned a model batch",
        );
        row(
            "b8.check7.poll_offset_ms",
            "measured",
            format!("{} {}", ms(c.poll_min_ns), ms(c.poll_max_ns)),
            "least and most of (gyroscope newest stamp - accelerometer newest stamp) in one poll",
        );
        row(
            "b8.check7.poll_bound_ms",
            "measured",
            ms(c.poll_bound_ns),
            "widest that difference was allowed to be: the two reads' span, one sample period, and 1 ms for each die",
        );
        row(
            "b8.check7.poll_violations",
            "measured",
            c.poll_violations.to_string(),
            "polls whose two newest stamps are further apart than their reads allow",
        );
        row(
            "b8.check7.poll_verdict",
            verdict(c.polls_ok()),
            String::new(),
            "every poll within its bound, with at least one poll: this is what sees a constant offset between the dies",
        );
        if !c.polls_ok() {
            outcome = Outcome::Fail;
        }
    } else {
        row(
            "b8.check7.poll_verdict",
            "not_evaluated",
            String::new(),
            "the files have no poll column, so the two dies' reads cannot be paired",
        );
        if outcome == Outcome::Pass {
            outcome = Outcome::Incomplete;
        }
    }
    row(
        "b8.check7.rate_ppm",
        "measured",
        format!("{:.1}", c.rate_ppm),
        "the gyroscope's stamp slope over the accelerometer's, minus one: the two oscillators' ratio",
    );
    row(
        "b8.check7.rate_bound_ppm",
        "measured",
        format!("{:.1}", c.rate_bound_ppm),
        "two dies each within the 4 % the driver's stamping is tested over; no oscillator tolerance is cited for the chip",
    );
    row(
        "b8.check7.rate_verdict",
        verdict(c.rate_ok()),
        String::new(),
        "the ratio within its bound",
    );
    if !c.rate_ok() {
        outcome = Outcome::Fail;
    }
    row(
        "b8.check7.verdict",
        outcome.verdict(),
        String::new(),
        "check 7: the spread, the same-poll difference and the rate ratio, each within its bound",
    );
    (out, outcome)
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

    /// The accelerometer's bounds at 200 Hz: a 5 ms period and a 370 ms tick drain period.
    const B: Bounds = Bounds {
        period_ns: P,
        drain_ns: ACCEL_DRAIN_PERIODS * P,
    };
    const POLL: u64 = 50_000_000;

    /// The wall time of a capture: from its first read's start to its last read's end. The first read is the
    /// first one that returned records, less a poll period for each read before it.
    fn wall(report: &Report) -> u64 {
        let first = &report.points[0];
        report.points.last().unwrap().read.end_ns - (first.read.start_ns - first.poll * POLL)
    }

    /// How a live capture of `report` comes out, with its own wall time.
    fn outcome(report: &Report) -> (String, Outcome) {
        check_rows(&Capture::live(report, B, wall(report), POLL))
    }

    /// A capture from bind: `drains` tick drains of 73 records each, then `model` model records in
    /// batches of 10. Sample `i` is taken at `(i + 1) * P`. A tick drain runs 2 ms after its newest
    /// sample (and 1 us later each drain, as no host ticks to the nanosecond) and stamps back from its
    /// own time by whole periods, and is read 20 ms later; a model
    /// batch is read half a period after its newest sample. Every read takes 0.6 ms. `edit` may move a
    /// record's stamp (its number, its mode, its stamp), to plant a fault.
    fn from_bind(drains: u64, model: u64, edit: impl Fn(u64, Stamp, u64) -> u64) -> Report {
        let mut report = Report::default();
        let latency = 600_000;
        let taken = |i: u64| (i + 1) * P;
        for d in 0..drains {
            let (first, last) = (d * 73, d * 73 + 72);
            let tick = taken(last) + 2_000_000 + d * 1_000;
            let records: Vec<[u8; STAMPED_RECORD_LEN]> = (first..=last)
                .map(|i| {
                    let at = edit(i, Stamp::TickTime, tick - (last - i) * P);
                    stamped(Stamp::TickTime, at, 0.0, 0.0, i as f32)
                })
                .collect();
            let end = tick + 20_000_000 + latency;
            report
                .add_batch(&batch(&records), read(end - latency, end))
                .unwrap();
        }
        let start = drains * 73;
        let mut k = start;
        while k < start + model {
            let end_of = (k + 10).min(start + model);
            let records: Vec<[u8; STAMPED_RECORD_LEN]> = (k..end_of)
                .map(|i| {
                    let at = edit(i, Stamp::Model, taken(i));
                    stamped(Stamp::Model, at, 0.0, 0.0, i as f32)
                })
                .collect();
            let end = taken(end_of - 1) + P / 2 + latency;
            report
                .add_batch(&batch(&records), read(end - latency, end))
                .unwrap();
            k = end_of;
        }
        report
    }

    /// Exact stamps, one per read, pass every check: the sim's model, with no error, is green.
    /// Fails on a check that bounds the stamps tighter than a read's own latency plus one period.
    #[test]
    fn exact_stamps_pass_every_check() {
        // 4000 records (20 s), 10 per batch, a read latency of 0.6 ms.
        let report = clean_report(4000, 10, 600_000);
        let a = report.check_a(B);
        assert_eq!((a.model.violations, a.tick.batches), (0, 0), "{a:?}");
        let b = report.check_b(P).expect("model points");
        assert!(b.rate_ppm.abs() <= b.bound_ppm, "{b:?}");
        let (text, outcome) = outcome(&report);
        assert_eq!(outcome, Outcome::Pass, "{text}");
        assert!(text.contains("b8.model.verdict\tpass"), "{text}");
        assert!(text.contains("b8.tick.verdict\tnone"), "{text}");
    }

    /// A capture from bind has both modes, and each is judged by its own bound. The tick-time batches'
    /// stamps are 20.6 ms before their reads' ends: over the model's bound (6.6 ms here), inside the
    /// tick-time one (370.6 ms). The capture passes, with both verdicts shown.
    /// Fails on the tool before this change: it held the tick-time batches to the model's bound, so
    /// every one of them was a violation of check (a) (the BOARD-4 from-bind captures).
    #[test]
    fn a_capture_from_bind_is_judged_per_mode_and_passes_when_both_pass() {
        let report = from_bind(16, 4000, |_, _, at| at);
        let a = report.check_a(B);
        assert_eq!((a.tick.batches, a.tick.violations), (16, 0), "{a:?}");
        assert_eq!(a.tick.max_lag_ns, 20_600_000, "{a:?}");
        assert_eq!(a.tick.min_lag_ns, 20_600_000, "{a:?}");
        assert_eq!(
            a.tick.bound_ns, 370_600_000,
            "read latency + one drain period"
        );
        assert_eq!((a.model.batches, a.model.violations), (400, 0), "{a:?}");
        assert_eq!(a.model.bound_ns, 6_600_000, "read latency + P + 1 ms");
        assert!(
            a.tick.max_lag_ns > a.model.bound_ns as i64,
            "the tick-time lag is over the model's bound"
        );
        let (text, outcome) = outcome(&report);
        assert_eq!(outcome, Outcome::Pass, "{text}");
        assert!(text.contains("b8.model.verdict\tpass"), "{text}");
        assert!(text.contains("b8.tick.verdict\tpass"), "{text}");
        assert!(text.contains("b8.tick.drains\tmeasured\t16"), "{text}");
    }

    /// Planted: a tick-time stamp earlier than the record before it. The order is checked over every
    /// record in every mode, so the capture fails on check (c), and on nothing else.
    #[test]
    fn a_backwards_stamp_in_tick_time_fails_the_capture() {
        // The first record of the third drain is stamped 10 ms early: before the second drain's newest.
        let report = from_bind(
            16,
            4000,
            |i, _, at| if i == 146 { at - 10_000_000 } else { at },
        );
        assert_eq!(report.backwards, 1);
        let (text, outcome) = outcome(&report);
        assert_eq!(outcome, Outcome::Fail, "{text}");
        assert!(text.contains("b8.c.backwards\tmeasured\t1"), "{text}");
        assert!(text.contains("b8.c.verdict\tfail"), "{text}");
        assert!(text.contains("b8.a.verdict\tpass"), "{text}");
        assert!(text.contains("b8.b.verdict\tpass"), "{text}");
    }

    /// Planted: the model's first record is earlier than the last tick-time record. The switch between
    /// the modes is held to the order too.
    #[test]
    fn a_backwards_stamp_across_the_switch_fails_the_capture() {
        let report = from_bind(
            16,
            4000,
            |i, _, at| if i == 16 * 73 { at - 5_000_000 } else { at },
        );
        assert_eq!(report.backwards, 1);
        let (text, outcome) = outcome(&report);
        assert_eq!(outcome, Outcome::Fail, "{text}");
        assert!(text.contains("b8.c.verdict\tfail"), "{text}");
    }

    /// Planted: a gap of two periods between two model records (one sample lost). The gap rule is the
    /// model's, so the capture fails on check (c), and the model's verdict is the one that fails.
    #[test]
    fn a_gap_between_model_records_fails_the_capture() {
        // Every model record from the 400th on is one period late: one gap of 10 ms.
        let at_gap = 16 * 73 + 400;
        let report = from_bind(16, 4000, |i, _, at| if i >= at_gap { at + P } else { at });
        let c = report.check_c(P);
        assert_eq!(
            (c.model_misses, c.model_largest_gap_ns),
            (1, 2 * P),
            "{c:?}"
        );
        let (text, outcome) = outcome(&report);
        assert_eq!(outcome, Outcome::Fail, "{text}");
        assert!(text.contains("b8.c.verdict\tfail"), "{text}");
        assert!(text.contains("b8.model.verdict\tfail"), "{text}");
        assert!(text.contains("b8.tick.verdict\tpass"), "{text}");
    }

    /// The same gap between two tick-time records is counted and shown, and is not a miss: a gap in
    /// tick time falls between two tick drains, which the gap rule does not judge.
    #[test]
    fn a_gap_between_tick_records_is_shown_and_not_judged() {
        // The third drain, and every later one, runs 2.9 ms late: a 7.9 ms gap before its first record.
        let report = from_bind(16, 4000, |i, mode, at| {
            if mode == Stamp::TickTime && i >= 146 {
                at + 2_900_000
            } else {
                at
            }
        });
        let c = report.check_c(P);
        assert_eq!((c.model_misses, c.tick_gaps_over), (0, 1), "{c:?}");
        assert_eq!(c.tick_largest_gap_ns, 7_901_000, "{c:?}");
        assert_eq!(report.backwards, 0);
        let (text, outcome) = outcome(&report);
        assert_eq!(outcome, Outcome::Pass, "{text}");
        assert!(text.contains("b8.c.tick.gaps_over\tinfo\t1"), "{text}");
    }

    /// Planted: tick-time stamps two drain periods old. Each is beyond what tick time can be held to
    /// (the read's latency and one drain period), so the tick-time verdict fails, and the model's
    /// passes: the capture fails.
    #[test]
    fn a_tick_stamp_beyond_its_bound_fails_the_capture() {
        let report = from_bind(16, 4000, |_, mode, at| {
            if mode == Stamp::TickTime {
                at.saturating_sub(730_000_000)
            } else {
                at
            }
        });
        let a = report.check_a(B);
        // The first two drains' stamps saturate at 0; every drain is over the bound.
        assert_eq!((a.tick.batches, a.tick.violations), (16, 16), "{a:?}");
        assert_eq!(a.model.violations, 0, "{a:?}");
        assert_eq!(report.backwards, 0, "the stale stamps are still in order");
        let (text, outcome) = outcome(&report);
        assert_eq!(outcome, Outcome::Fail, "{text}");
        assert!(text.contains("b8.tick.verdict\tfail"), "{text}");
        assert!(text.contains("b8.model.verdict\tpass"), "{text}");
    }

    /// The tick-time bound is the read's latency and one drain period, and no more: a stamp one poll
    /// old is inside it, and one just past a drain period is not. The model's bound is unchanged: the
    /// same 50 ms on a model record fails.
    #[test]
    fn the_tick_bound_is_one_drain_period_and_the_models_is_one_sample_period() {
        let one = |mode: Stamp, lag: u64| {
            let mut report = Report::default();
            let end = 1_000_000_000;
            report
                .add_batch(
                    &batch(&[stamped(mode, end - lag, 0.0, 0.0, 9.8)]),
                    read(end - 600_000, end),
                )
                .unwrap();
            report.check_a(B)
        };
        assert_eq!(one(Stamp::TickTime, 50_600_000).tick.violations, 0);
        assert_eq!(one(Stamp::TickTime, 370_600_000).tick.violations, 0);
        assert_eq!(one(Stamp::TickTime, 370_600_001).tick.violations, 1);
        assert_eq!(one(Stamp::Model, 6_600_000).model.violations, 0);
        assert_eq!(one(Stamp::Model, 6_600_001).model.violations, 1);
        assert_eq!(one(Stamp::Model, 50_600_000).model.violations, 1);
    }

    /// A capture in which the model never publishes is not a pass, whatever its tick-time records do:
    /// check (b) has no rate to measure.
    #[test]
    fn a_capture_with_no_model_record_fails() {
        let report = from_bind(16, 0, |_, _, at| at);
        let (text, outcome) = outcome(&report);
        assert_eq!(outcome, Outcome::Fail, "{text}");
        assert!(text.contains("b8.b.verdict\tfail"), "{text}");
        assert!(text.contains("b8.tick.verdict\tpass"), "{text}");
        assert!(text.contains("b8.model.verdict\tfail"), "{text}");
    }

    /// The tick drains are counted from the capture, with their period, and where they fell on the
    /// model's grid. Here every drain runs 2 ms after its newest sample, so the offsets are all 2 ms
    /// and the step between two drains is nothing.
    #[test]
    fn the_tick_drains_are_counted_and_placed_on_the_models_grid() {
        let report = from_bind(16, 4000, |_, _, at| at);
        let drains = report.tick_drains(P);
        assert_eq!(drains.drains, 16);
        assert_eq!(drains.median_period_ns, Some(365_001_000));
        let (lo, hi) = drains.grid_offset_ns.unwrap();
        assert!(
            (lo - 2_000_000).abs() < 1_000,
            "the first drain, 2 ms after its sample: {lo}"
        );
        assert!(
            (hi - 2_015_000).abs() < 1_000,
            "the last, 15 us later: {hi}"
        );
        assert!((drains.median_grid_step_ns.unwrap() as i64 - 1_000).abs() < 100);
        assert_eq!(report.tick_returns(), 0);
    }

    /// A drain the model did not stamp, after the model began, is a return to tick time.
    #[test]
    fn a_return_to_tick_time_after_the_model_is_counted() {
        let mut report = Report::default();
        for (i, mode) in [Stamp::TickTime, Stamp::Model, Stamp::TickTime, Stamp::Model]
            .into_iter()
            .enumerate()
        {
            let at = i as u64 * P;
            report
                .add_batch(
                    &batch(&[stamped(mode, at, 0.0, 0.0, i as f32)]),
                    read(at, at),
                )
                .unwrap();
        }
        assert_eq!(report.tick_returns(), 1);
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
        assert_eq!(outcome(&report).1, Outcome::Fail);
    }

    /// The bound in check (b) is a bound on what the record numbers alone allow: a sub-bound error passes.
    #[test]
    fn a_rate_within_the_bound_passes_check_b() {
        let report = clean_report(4000, 10, 600_000);
        let b = report.check_b(P).unwrap();
        assert!(b.bound_ppm > 0.0 && b.bound_ppm < 1_000.0, "{b:?}");
    }

    /// A capture file written from a live report reads back to the same checks: the same (a) and (b)
    /// figures, and the same outcome, with both modes in it.
    #[test]
    fn a_capture_file_replays_to_the_same_checks() {
        for report in [
            clean_report(4000, 10, 600_000),
            from_bind(16, 4000, |_, _, at| at),
        ] {
            let replay = from_tsv(&stamps_tsv(&report)).unwrap();
            assert!(replay.has_modes && replay.has_read_times && replay.has_polls);
            assert_eq!(replay.report.check_a(B), report.check_a(B));
            assert_eq!(replay.report.check_c(P), report.check_c(P));
            let b0 = report.check_b(P).unwrap();
            let b1 = replay.report.check_b(P).unwrap();
            assert_eq!(b0.points, b1.points);
            assert!((b0.rate_ppm - b1.rate_ppm).abs() < 1e-6, "{b0:?} {b1:?}");
            let timing = Some((wall(&report), POLL));
            let (text, replayed) = check_rows(&Capture::replayed(&replay, B, timing));
            assert_eq!(replayed, Outcome::Pass, "{text}");
            assert_eq!(text, outcome(&report).0, "the live rows and the replay's");
        }
    }

    /// A file with stamps only evaluates check (c) and no more: a clean stream is not a pass (checks (a) and
    /// (b) were not run), and a missed sample fails. With no mode column every record is held to the
    /// model's gap rule, as before the modes were recorded.
    #[test]
    fn a_stamps_only_file_evaluates_check_c_and_no_more() {
        let clean = format!(
            "at_ns\n{}",
            (0..100u64)
                .map(|i| (i * P).to_string())
                .collect::<Vec<_>>()
                .join("\n")
        );
        let replay = from_tsv(&clean).unwrap();
        assert!(!replay.has_modes && !replay.has_read_times && !replay.has_polls);
        let (text, outcome) = check_rows(&Capture::replayed(&replay, B, None));
        assert_eq!(outcome, Outcome::Incomplete, "{text}");
        assert!(text.contains("b8.a.verdict\tnot_evaluated"), "{text}");
        assert!(text.contains("b8.b.verdict\tnot_evaluated"), "{text}");
        assert!(text.contains("b8.c.verdict\tpass"), "{text}");
        assert!(text.contains("b8.d.verdict\tnot_evaluated"), "{text}");
        assert!(text.contains("b8.model.verdict\tnot_evaluated"), "{text}");
        // A 20 ms gap between two stamps is a missed run of samples: check (c) fails.
        let missed = "at_ns\n0\n5000000\n30000000\n35000000";
        let replay = from_tsv(missed).unwrap();
        let (text, outcome) = check_rows(&Capture::replayed(&replay, B, None));
        assert_eq!(outcome, Outcome::Fail, "{text}");
        assert!(text.contains("b8.c.model.misses\tmeasured\t1"), "{text}");
    }

    /// A file with read times and no mode column cannot say which bound a batch is held to, so check (a)
    /// is not evaluated: it is never judged by a bound that may not be its own.
    #[test]
    fn a_file_with_read_times_and_no_modes_leaves_check_a_not_evaluated() {
        let text = "at_ns\tbatch\tread_start_ns\tread_end_ns\n\
                    0\t0\t0\t10\n\
                    5000000\t0\t0\t10";
        let replay = from_tsv(text).unwrap();
        let (text, outcome) = check_rows(&Capture::replayed(&replay, B, None));
        assert_eq!(outcome, Outcome::Incomplete, "{text}");
        assert!(text.contains("b8.a.verdict\tnot_evaluated"), "{text}");
    }

    /// Check (d): the span against the wall window, within 2 poll periods plus 1 sample period (105 ms at a
    /// 50 ms poll and 5 ms samples). The first board capture's +668 ms is red; a 100 ms difference is green.
    #[test]
    fn check_d_bounds_the_span_against_the_wall_window() {
        let report = clean_report(4000, 10, 600_000);
        let span = report.check_d(0, POLL, P).span_ns;
        assert_eq!(check_d_bound_ns(POLL, P), 105_000_000);
        assert!(report.check_d(span + 100_000_000, POLL, P).pass);
        assert!(report.check_d(span - 100_000_000, POLL, P).pass);
        let red = report.check_d(span + 668_490_000, POLL, P);
        assert!(!red.pass, "{red:?}");
        assert_eq!(red.diff_ns, -668_490_000);
        assert!(!report.check_d(span + 106_000_000, POLL, P).pass);
    }

    /// A read that fills its buffer does not reach the driver's newest record: the driver keeps the rest for the
    /// next read. Here a capture begins on a backlog of three tick drains (219 records): the first read returns
    /// 141 of them, whose newest is 0.6 s older than the read. That batch is counted and not judged; the next
    /// read, which empties the backlog, is judged, and passes.
    /// Fails on the tool before this change: the full batch was held to the tick-time bound on its newest record
    /// and failed it (a from-bind case in the rehearsal begun 1.3 s after its bind failed this way).
    #[test]
    fn a_batch_that_fills_the_buffer_is_counted_and_not_judged_on_its_newest() {
        let mut report = Report::default();
        let stamps: Vec<u64> = (0..219).map(|i| (i + 1) * P).collect();
        let now = 220 * P + 2_000_000;
        let first: Vec<[u8; STAMPED_RECORD_LEN]> = stamps[..BATCH_RECORDS]
            .iter()
            .map(|&at| stamped(Stamp::TickTime, at, 0.0, 0.0, at as f32))
            .collect();
        report
            .add_batch(&batch(&first), read(now, now + 600_000))
            .unwrap();
        let rest: Vec<[u8; STAMPED_RECORD_LEN]> = stamps[BATCH_RECORDS..]
            .iter()
            .map(|&at| stamped(Stamp::TickTime, at, 0.0, 0.0, at as f32))
            .collect();
        report
            .add_batch(&batch(&rest), read(now + POLL, now + POLL + 600_000))
            .unwrap();
        let a = report.check_a(B);
        assert_eq!(a.full, 1, "{a:?}");
        assert_eq!((a.tick.batches, a.tick.violations), (1, 0), "{a:?}");
        // Judged, the full batch would fail: its newest is older than the read by more than a drain period.
        let lag = now + 600_000 - stamps[BATCH_RECORDS - 1];
        assert!(lag > B.drain_ns + 600_000, "{lag}");
        let replay = from_tsv(&stamps_tsv(&report)).unwrap();
        assert_eq!(replay.report.check_a(B), a, "the file reads back the same");
    }

    /// The first read of a capture returns what the driver's ring held before the capture began. Here that is
    /// 130 records, the oldest 0.65 s older than the first read. They are a backlog, not part of the capture's
    /// span: check (d) counts them, and passes. A file without read times cannot tell them from a drift, and
    /// fails.
    /// Fails on the tool before this change: the backlog was in the span, so a capture begun on a full ring
    /// read 600 ms long (a from-bind capture in the simulator, begun 0.17 s after its bind, failed this way).
    #[test]
    fn a_backlog_in_the_first_read_is_counted_and_left_out_of_check_d() {
        let mut report = Report::default();
        let taken = |i: u64| (i + 1) * P;
        // The first read: records 0 to 129, the newest half a period old.
        let first: Vec<[u8; STAMPED_RECORD_LEN]> = (0..130)
            .map(|i| record(taken(i), 0.0, 0.0, i as f32))
            .collect();
        let start = taken(129) + P / 2;
        report
            .add_batch(&batch(&first), read(start, start + 600_000))
            .unwrap();
        let mut k = 130;
        while k < 4130 {
            let records: Vec<[u8; STAMPED_RECORD_LEN]> = (k..k + 10)
                .map(|i| record(taken(i), 0.0, 0.0, i as f32))
                .collect();
            let end = taken(k + 9) + P / 2 + 600_000;
            report
                .add_batch(&batch(&records), read(end - 600_000, end))
                .unwrap();
            k += 10;
        }
        let d = report.check_d(wall(&report), POLL, P);
        // Records 0 to 128 are more than one period older than the first read's start.
        assert_eq!(d.backlog_records, 129, "{d:?}");
        assert!(d.pass && d.diff_ns.abs() < 2 * P as i64, "{d:?}");
        let (text, outcome) = outcome(&report);
        assert_eq!(outcome, Outcome::Pass, "{text}");
        // The same stamps with no read times: the backlog is in the span, 645 ms over the wall time.
        let stamps_only = format!(
            "at_ns\n{}",
            report
                .stamps
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        );
        let replay = from_tsv(&stamps_only).unwrap();
        let d = replay.report.check_d(wall(&report), POLL, P);
        assert_eq!(d.backlog_records, 0);
        assert!(!d.pass && d.diff_ns > 600_000_000, "{d:?}");
    }

    /// Without the run's wall time and poll period, check (d) is not evaluated and the replay is not a pass.
    #[test]
    fn a_replay_without_timing_leaves_check_d_not_evaluated() {
        let report = clean_report(4000, 10, 600_000);
        let replay = from_tsv(&stamps_tsv(&report)).unwrap();
        let (text, outcome) = check_rows(&Capture::replayed(&replay, B, None));
        assert_eq!(outcome, Outcome::Incomplete, "{text}");
        assert!(text.contains("b8.d.verdict\tnot_evaluated"), "{text}");
    }

    /// Check (b) needs two model batches. A file with one is a fail, as the live tool reports it.
    #[test]
    fn check_b_with_one_model_batch_fails_as_the_live_tool_does() {
        let text = "at_ns\tmode\tbatch\tread_start_ns\tread_end_ns\n\
                    0\tmodel\t0\t0\t10\n\
                    5000000\tmodel\t0\t0\t10";
        let replay = from_tsv(text).unwrap();
        let (text, outcome) = check_rows(&Capture::replayed(&replay, B, None));
        assert_eq!(outcome, Outcome::Fail, "{text}");
        assert!(text.contains("b8.b.verdict\tfail"), "{text}");
    }

    /// The mode column gives the tick and model counts.
    #[test]
    fn a_mode_column_counts_the_modes() {
        let replay = from_tsv("at_ns\tmode\n0\ttick\n5000000\tmodel\n10000000\tmodel").unwrap();
        assert!(replay.has_modes && !replay.has_read_times);
        assert_eq!(
            (replay.report.tick_records, replay.report.model_records),
            (1, 2)
        );
    }

    /// A file that is not a capture is refused, not read as a partial one.
    #[test]
    fn a_file_that_is_not_a_capture_is_refused() {
        assert!(from_tsv("").is_err());
        assert!(from_tsv("stamp\n1").is_err(), "no at_ns column");
        assert!(from_tsv("at_ns\nabc").is_err(), "not a number");
        assert!(
            from_tsv("at_ns\tmode\n1").is_err(),
            "a row with too few fields"
        );
        assert!(from_tsv("at_ns\tmode\n1\tfoo").is_err(), "an unknown mode");
        assert!(from_tsv("at_ns\n").is_err(), "no records");
    }

    /// Two dies read on the same polls: each die takes a sample every `period` from `phase`, and each
    /// poll (every 50 ms, a 0.6 ms read for each die, the gyroscope first) returns the die's new samples,
    /// stamped at their own times plus `offset`.
    fn die(period: u64, phase: u64, offset: i64, polls: u64) -> Report {
        let mut report = Report::default();
        let mut taken = 0u64;
        for poll in 1..=polls {
            let start = poll * POLL;
            let end = start + 600_000;
            let mut records = Vec::new();
            while phase + taken * period <= start {
                let at = (phase + taken * period).saturating_add_signed(offset);
                records.push(record(at, 0.0, 0.0, taken as f32));
                taken += 1;
            }
            report
                .add_batch(&batch(&records), read(start, end))
                .unwrap();
        }
        report
    }

    /// Check 7 on two dies whose stamps are their samples' times: the gyroscope 0.73 % slower than the
    /// accelerometer, as the board's two oscillators were. The spread is close to one accelerometer
    /// period, inside its bound; every poll's two newest stamps are within their reads; the ratio is
    /// the oscillators'. All three pass.
    #[test]
    fn check_7_passes_two_dies_stamped_at_their_own_times() {
        let accel = die(4_962_500, 1_000_000, 0, 900);
        let gyro = die(4_999_000, 3_000_000, 0, 900);
        let c = check_7(&gyro, &accel, P).unwrap();
        assert!(c.pass(), "{c:?}");
        assert!(
            c.spread_ns() > 4_800_000 && c.spread_ns() <= 4_962_500,
            "{c:?}"
        );
        assert_eq!(c.spread_bound_ns, 4_962_500 + 2 * TOL_NS, "{c:?}");
        assert!((c.rate_ppm - 7_355.0).abs() < 5.0, "{c:?}");
        assert!((c.rate_bound_ppm - 83_333.3).abs() < 0.1, "{c:?}");
        assert_eq!((c.polls, c.poll_violations), (900, 0), "{c:?}");
        let (text, outcome) = check7_rows(&gyro, &accel, P, true);
        assert_eq!(outcome, Outcome::Pass, "{text}");
        assert!(text.contains("b8.check7.verdict\tpass"), "{text}");
    }

    /// Planted: every gyroscope stamp 20 ms late, a constant offset between the dies. The spread does
    /// not see it (the nearest accelerometer stamp is found again after any shift), and the same-poll
    /// difference does: every poll is beyond its bound, and check 7 fails.
    #[test]
    fn a_constant_offset_between_the_dies_fails_check_7_on_the_same_poll() {
        let accel = die(4_962_500, 1_000_000, 0, 900);
        for offset in [20_000_000, -20_000_000] {
            let gyro = die(4_999_000, 3_000_000, offset, 900);
            let c = check_7(&gyro, &accel, P).unwrap();
            assert!(
                c.spread_ok(),
                "the spread does not see a constant offset: {c:?}"
            );
            assert_eq!(c.poll_violations, c.polls, "{c:?}");
            assert!(c.poll_bound_ns < 20_000_000, "{c:?}");
            assert!(!c.pass(), "{c:?}");
            let (text, outcome) = check7_rows(&gyro, &accel, P, true);
            assert_eq!(outcome, Outcome::Fail, "{text}");
            assert!(text.contains("b8.check7.poll_verdict\tfail"), "{text}");
            assert!(text.contains("b8.check7.verdict\tfail"), "{text}");
        }
        // An offset inside the bound is not a fault: 2 ms is each die's 1 ms.
        let gyro = die(4_999_000, 3_000_000, 2_000_000, 900);
        assert!(check_7(&gyro, &accel, P).unwrap().polls_ok());
    }

    /// Planted: a hole in the accelerometer's stamps (four records lost). The gyroscope stamps in the
    /// hole are further from any accelerometer stamp than half a period, so the spread goes over one
    /// accelerometer period and two stamp errors, and check 7 fails on the spread. This is the one fault
    /// the spread can show: the offset is to the nearest stamp, so nothing else moves it past a period.
    #[test]
    fn a_hole_in_the_accelerometers_stamps_fails_check_7_on_the_spread() {
        let mut accel = Report::default();
        let mut taken = 0u64;
        for poll in 1..=900u64 {
            let start = poll * POLL;
            let mut records = Vec::new();
            while 1_000_000 + taken * 5_000_000 <= start {
                if !(2000..2004).contains(&taken) {
                    records.push(record(
                        1_000_000 + taken * 5_000_000,
                        0.0,
                        0.0,
                        taken as f32,
                    ));
                }
                taken += 1;
            }
            accel
                .add_batch(&batch(&records), read(start, start + 600_000))
                .unwrap();
        }
        let gyro = die(5_000_000, 3_400_000, 0, 900);
        let c = check_7(&gyro, &accel, P).unwrap();
        assert!(c.spread_ns() > c.spread_bound_ns, "{c:?}");
        let (text, outcome) = check7_rows(&gyro, &accel, P, true);
        assert_eq!(outcome, Outcome::Fail, "{text}");
        assert!(text.contains("b8.check7.spread_verdict\tfail"), "{text}");
    }

    /// Planted: a gyroscope whose stamps run 10 % slow against the accelerometer's. The ratio is beyond
    /// two dies each inside the 4 % the driver is tested over, and check 7 fails on the rate.
    #[test]
    fn a_rate_ratio_beyond_the_tested_range_fails_check_7() {
        let accel = die(5_000_000, 1_000_000, 0, 900);
        let gyro = die(5_500_000, 3_000_000, 0, 900);
        let c = check_7(&gyro, &accel, P).unwrap();
        assert!((c.rate_ppm - 100_000.0).abs() < 50.0, "{c:?}");
        assert!(!c.rate_ok() && !c.pass(), "{c:?}");
        // 0.73 %, the board's ratio, and 8 % are inside it.
        for period in [5_036_500, 5_400_000] {
            assert!(
                check_7(&die(period, 3_000_000, 0, 900), &accel, P)
                    .unwrap()
                    .rate_ok()
            );
        }
    }

    /// Check 7 needs model records on both dies, and files with a `poll` column to pair the reads.
    #[test]
    fn check_7_is_not_a_pass_without_both_dies_or_without_polls() {
        let accel = die(5_000_000, 1_000_000, 0, 100);
        let (text, outcome) = check7_rows(&Report::default(), &accel, P, true);
        assert_eq!(outcome, Outcome::Fail, "{text}");
        let gyro = die(5_000_000, 3_000_000, 0, 100);
        let (text, outcome) = check7_rows(&gyro, &accel, P, false);
        assert_eq!(outcome, Outcome::Incomplete, "{text}");
        assert!(
            text.contains("b8.check7.poll_verdict\tnot_evaluated"),
            "{text}"
        );
    }

    /// The second die's rows of a gyroscope run carry their own prefix.
    #[test]
    fn the_second_dies_rows_are_prefixed() {
        assert_eq!(
            prefixed(
                "b8.a.verdict\tpass\t\tx\nb8.verdict\tpass\t\ty\n",
                "b8.accel."
            ),
            "b8.accel.a.verdict\tpass\t\tx\nb8.accel.verdict\tpass\t\ty\n"
        );
    }
}
