//! Where each die's samples fall in time, so the gyroscope can be resampled at
//! the accelerometer's sample times.
//!
//! The two dies run on their own oscillators (on one board 403.9 Hz and
//! 399.6 Hz for a 400 Hz setting), so pairing their FIFO frames one to one
//! puts a gyroscope sample milliseconds away from the accelerometer sample it
//! is paired with. Instead each die gets a [`DieClock`] fed with its FIFO
//! count and the host-clock time just after the count's transaction: the
//! die's newest sample was taken before then. Each such time is an upper
//! bound on a sample's time, tight when the read came just after the sample,
//! and as the reads drift through the die's period some always are. The
//! clock keeps the lower convex hull of the bounds of the last
//! [`WINDOW_NS`]: its edge across the middle of the window is the line of
//! the die's samples, its slope the die's period.
//!
//! Lower bounds (the next sample came after the transaction started) are not
//! used: on the board both dies sometimes show a sample late in their count
//! (the accelerometer by up to about 0.3 ms, the gyroscope by up to 1.6 ms),
//! which would make a lower bound wrong. An upper bound never was.

/// How far back the hull reaches, nanoseconds.
const WINDOW_NS: i64 = 6_000_000_000;
/// The hull's most vertices (the oldest go first).
const HULL: usize = 16;

/// One die's sample clock. Samples are numbered from the first one this
/// clock saw; times are host-clock nanoseconds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DieClock {
    /// The nominal period, ns (0: not started).
    nominal: i64,
    /// Samples read out of the FIFO so far.
    read: u64,
    /// The line: sample `k0` at `t0`, `period` apart.
    k0: u64,
    t0: i64,
    period: i64,
    /// The lower hull of `(sample, bound)`, oldest first.
    hull: [(u64, i64); HULL],
    len: usize,
}

impl DieClock {
    pub(crate) const fn new() -> Self {
        Self {
            nominal: 0,
            read: 0,
            k0: 0,
            t0: 0,
            period: 0,
            hull: [(0, 0); HULL],
            len: 0,
        }
    }

    /// The die's FIFO held `fill` samples when its count was read, and the
    /// read ended at `after` (ns). `period_ns` is the nominal period.
    pub(crate) fn count(&mut self, fill: usize, after: u64, period_ns: u32) {
        let nominal = i64::from(period_ns);
        if self.nominal != nominal {
            *self = Self::new();
            self.nominal = nominal;
            self.period = nominal;
        }
        let Some(newest) = (self.read + fill as u64).checked_sub(1) else {
            return;
        };
        self.add((newest, after as i64));
        self.fit();
    }

    /// The samples just read out of the FIFO.
    pub(crate) fn took(&mut self, samples: usize) {
        self.read += samples as u64;
    }

    /// Adds a bound to the hull: the vertices it puts above the hull go.
    fn add(&mut self, p: (u64, i64)) {
        if let Some(&(k, t)) = self.hull[..self.len].last() {
            if p.0 < k || (p.0 == k && p.1 >= t) {
                return;
            }
            if p.0 == k {
                self.len -= 1;
            }
        }
        while self.len >= 2 {
            let (a, b) = (self.hull[self.len - 2], self.hull[self.len - 1]);
            // b is on or above the segment a-p: not a vertex.
            let cross = i128::from(b.0 - a.0) * i128::from(p.1 - a.1)
                - i128::from(b.1 - a.1) * i128::from(p.0 - a.0);
            if cross > 0 {
                break;
            }
            self.len -= 1;
        }
        if self.len == HULL {
            self.hull.copy_within(1.., 0);
            self.len -= 1;
        }
        self.hull[self.len] = p;
        self.len += 1;
        // The window: a vertex goes once the next one is older than it.
        let oldest = p.0.saturating_sub((WINDOW_NS / self.period.max(1)) as u64);
        let mut drop = 0;
        while self.len - drop > 2 && self.hull[drop + 1].0 <= oldest {
            drop += 1;
        }
        if drop > 0 {
            self.hull.copy_within(drop..self.len, 0);
            self.len -= drop;
        }
    }

    /// The line: the hull's edge across the middle of its span.
    fn fit(&mut self) {
        let hull = &self.hull[..self.len];
        let (Some(&first), Some(&last)) = (hull.first(), hull.last()) else {
            return;
        };
        if hull.len() == 1 {
            (self.k0, self.t0) = first;
            return;
        }
        let middle = first.0 + (last.0 - first.0) / 2;
        let Some(edge) = hull.windows(2).find(|w| w[1].0 >= middle) else {
            return;
        };
        let ((k1, t1), (k2, t2)) = (edge[0], edge[1]);
        let p = (t2 - t1) / (k2 - k1) as i64;
        if (p - self.nominal).abs() <= self.nominal / 20 {
            self.period = p;
            (self.k0, self.t0) = (k2, t2);
        }
    }

    /// The estimated time of sample `k`, ns.
    pub(crate) fn time_of(&self, k: u64) -> i64 {
        self.t0 + (k as i64 - self.k0 as i64) * self.period
    }

    /// The number of the first sample of the next read.
    pub(crate) fn first_unread(&self) -> u64 {
        self.read
    }

    pub(crate) fn period(&self) -> i64 {
        self.period
    }
}

/// The accelerometer's sensor time: a 24-bit counter of the die's own
/// oscillator, 25.6 kHz nominal (39.0625 µs a tick), read right after the
/// FIFO count. The die takes its samples on a grid of it: sample `k` at tick
/// `step * k + phase`, `step` the ticks per sample (64 at 400 Hz). On board #1
/// the counter read just after the count less `64 k` spanned exactly one
/// period (1348 to 1412) over 3000 reads, with no late count. The tick times
/// on the host clock come from a least-squares line through each read's
/// (counter, transaction midpoint), forgetting with a time constant of
/// [`FORGET_NS`] as the oscillator drifts. A read latches the counter anywhere
/// in a tick, so the line at counter value `c` is the middle of tick `c`.
/// The phase is the least counter a read showing the sample latched, so that
/// read latched somewhere in tick `phase`: a sample's time is the line at
/// `step * k + phase` (the middle of that tick), less half the transaction
/// (when the count read before it ended, as the gyroscope's count hull
/// places its samples).
#[derive(Clone, Copy, Debug)]
pub(crate) struct TickClock {
    /// Ticks per sample (0: not started).
    step: u64,
    /// The last counter value read, unwrapped to 64 bits.
    ticks: u64,
    /// The grid's phase: the least `ticks - step * k` seen.
    phase: Option<u64>,
    /// The line, relative to the last point (`x0` ticks, `y0` ns): weighted
    /// sums of the points' offsets from it.
    x0: u64,
    y0: u64,
    w: f64,
    sx: f64,
    sy: f64,
    sxx: f64,
    sxy: f64,
    /// The usual transaction length, ns (a running average of the accepted).
    txn: f64,
    /// Points taken.
    points: u32,
}

/// The line's forgetting time constant, ns.
const FORGET_NS: f64 = 500_000_000.0;
/// Points before the line is used.
const MIN_POINTS: u32 = 16;
/// A read whose transaction is this much longer than usual was preempted:
/// its midpoint is not where the counter was latched, so it is skipped.
const LONG_TXN: f64 = 1.5;
/// A host-clock gap after which the 24-bit counter may have wrapped unseen
/// (it wraps every 655 s): start again.
const WRAP_GAP_NS: u64 = 600_000_000_000;

impl TickClock {
    pub(crate) const fn new() -> Self {
        Self {
            step: 0,
            ticks: 0,
            phase: None,
            x0: 0,
            y0: 0,
            w: 0.0,
            sx: 0.0,
            sy: 0.0,
            sxx: 0.0,
            sxy: 0.0,
            txn: 0.0,
            points: 0,
        }
    }

    /// The counter read `counter` between `before` and `after` (ns), right
    /// after a count that showed sample `newest` as the newest; `period_us`
    /// is the accelerometer's nominal period.
    pub(crate) fn read(
        &mut self,
        counter: u32,
        before: u64,
        after: u64,
        newest: Option<u64>,
        period_us: u32,
    ) {
        let step = u64::from(period_us) * 256 / 10_000;
        let counter = u64::from(counter & 0x00ff_ffff);
        if step != self.step
            || step == 0
            || before.saturating_sub(self.y0) > WRAP_GAP_NS && self.points > 0
        {
            *self = Self::new();
            self.step = step;
            self.ticks = counter;
        } else {
            let wrapped = (counter + (1 << 24) - (self.ticks & 0x00ff_ffff)) & 0x00ff_ffff;
            self.ticks += wrapped;
        }
        let ticks = self.ticks;
        if let Some(k) = newest {
            let p = ticks.saturating_sub(step * k);
            self.phase = Some(self.phase.map_or(p, |q| q.min(p)));
        }
        let txn = after.saturating_sub(before) as f64;
        if self.points > 0 && txn > self.txn * LONG_TXN {
            return;
        }
        self.txn = if self.points == 0 {
            txn
        } else {
            self.txn + (txn - self.txn) / 16.0
        };
        let mid = before + (after - before) / 2;
        if self.points == 0 {
            self.x0 = ticks;
            self.y0 = mid;
        }
        // Move the sums to this point and forget by the time since the last.
        let dx = ticks as f64 - self.x0 as f64;
        let dy = mid as f64 - self.y0 as f64;
        let w = self.w;
        self.sxx += -2.0 * dx * self.sx + w * dx * dx;
        self.sxy += -dx * self.sy - dy * self.sx + w * dx * dy;
        self.sx -= w * dx;
        self.sy -= w * dy;
        let keep = (1.0 - dy / FORGET_NS).clamp(0.0, 1.0);
        self.w = self.w * keep + 1.0;
        self.sx *= keep;
        self.sy *= keep;
        self.sxx *= keep;
        self.sxy *= keep;
        self.x0 = ticks;
        self.y0 = mid;
        self.points = self.points.saturating_add(1);
    }

    /// The host time of accelerometer sample `k`, ns, once the line is
    /// measured.
    pub(crate) fn time_of(&self, k: u64) -> Option<i64> {
        let phase = self.phase?;
        if self.points < MIN_POINTS {
            return None;
        }
        let det = self.w * self.sxx - self.sx * self.sx;
        if det <= 0.0 {
            return None;
        }
        let slope = (self.w * self.sxy - self.sx * self.sy) / det;
        let at0 = (self.sy - slope * self.sx) / self.w;
        // Both the fit's points and the sample are a counter value: the
        // middle of their tick. (Until 2026-11-07 half a tick was added here
        // as well, which put every sample one tick late: 31-38 µs on board #1.)
        let x = (self.step * k + phase) as f64 - self.x0 as f64;
        Some(self.y0 as i64 + (at0 + slope * x - self.txn / 2.0) as i64)
    }
}

/// Both dies' clocks and the gyroscope sample before the current batch, to
/// interpolate across a batch boundary.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Align {
    pub(crate) accel: DieClock,
    pub(crate) gyro: DieClock,
    /// The accelerometer's sensor time, which places its samples closer than
    /// its counts do.
    pub(crate) ticks: TickClock,
    /// The last gyroscope sample read, and its number.
    pub(crate) last_gyro: Option<(u64, [i16; 3])>,
}

impl Align {
    pub(crate) const fn new() -> Self {
        Self {
            accel: DieClock::new(),
            gyro: DieClock::new(),
            ticks: TickClock::new(),
            last_gyro: None,
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    /// The host time of accelerometer sample `k`: from the sensor time once
    /// it is measured, from the counts before.
    pub(crate) fn accel_time(&self, k: u64) -> i64 {
        self.ticks
            .time_of(k)
            .unwrap_or_else(|| self.accel.time_of(k))
    }
}

/// The gyroscope at time `t` (ns) from `gyro` (numbered from `first`, times
/// from `clock`) and the sample before them (`before`): linear between the
/// samples around `t`, extrapolated from the newest two for at most one
/// period past the newest, and held beyond that.
pub(crate) fn gyro_at(
    t: i64,
    clock: &DieClock,
    first: u64,
    gyro: &[[i16; 3]],
    before: Option<(u64, [i16; 3])>,
) -> [i16; 3] {
    let sample = |k: u64| -> Option<[i16; 3]> {
        if k >= first {
            gyro.get((k - first) as usize).copied()
        } else {
            before.filter(|(n, _)| *n == k).map(|(_, s)| s)
        }
    };
    let period = clock.period().max(1);
    let Some(newest) = (first + gyro.len() as u64).checked_sub(1) else {
        return [0; 3];
    };
    let oldest = before.map_or(first, |(n, _)| n.min(first));
    // The sample at or before t, and how far past it (ns); before the oldest
    // sample, the oldest is held.
    let since = t - clock.time_of(0);
    let at_or_before = since.div_euclid(period);
    if at_or_before < oldest as i64 {
        return sample(oldest).unwrap_or([0; 3]);
    }
    let k = at_or_before as u64;
    let frac = since - at_or_before * period;
    if k >= newest {
        // Past the newest: extrapolate from the two newest, at most a period.
        let over = (frac + (k - newest) as i64 * period).min(period);
        return match (newest.checked_sub(1).and_then(sample), sample(newest)) {
            (Some(a), Some(b)) => lerp(a, b, period + over, period),
            (_, Some(b)) => b,
            _ => [0; 3],
        };
    }
    match (sample(k), sample(k + 1)) {
        (Some(a), Some(b)) => lerp(a, b, frac, period),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => [0; 3],
    }
}

/// `a + (b - a) * num / den` per axis, rounded, clamped to `i16`.
fn lerp(a: [i16; 3], b: [i16; 3], num: i64, den: i64) -> [i16; 3] {
    let mut out = [0i16; 3];
    for i in 0..3 {
        let (a, b) = (i64::from(a[i]), i64::from(b[i]));
        let v = a + ((b - a) * num * 2 + den).div_euclid(2 * den);
        out[i] = v.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A die 1% fast of its 2.5 ms setting, read every 20 ms (with up to
    /// 0.3 ms of scheduling jitter) by a 180 µs count transaction that sees
    /// the count 20 µs before it ends; one count in seven shows its newest
    /// sample late (as the board's do). The clock lands on the tightest
    /// bound: 20 µs after each sample.
    #[test]
    fn a_die_clock_follows_its_tightest_counts() {
        const PERIOD: i64 = 2_475_000;
        const PHASE: i64 = 700_000;
        let sample_time = |k: i64| PHASE + k * PERIOD;
        let mut clock = DieClock::new();
        let mut seed = 12_345u32;
        let mut produced_read = 0u64;
        let mut worst = 0i64;
        for read in 0..600u64 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let jitter = i64::from(seed >> 8) % 300_000;
            let before = 5_000_000 + read as i64 * 20_000_000 + jitter;
            let after = before + 180_000;
            let latch = after - 20_000;
            // Samples taken by the latch: k with sample_time(k) <= latch.
            let mut produced = ((latch - PHASE).div_euclid(PERIOD) + 1) as u64;
            if read % 7 == 3 {
                produced -= 1;
            }
            let fill = produced.saturating_sub(produced_read) as usize;
            clock.count(fill, after as u64, 2_500_000);
            clock.took(fill);
            produced_read += fill as u64;
            if read >= 400 {
                let k = produced_read - 1;
                let err = clock.time_of(k) - (sample_time(k as i64) + 20_000);
                worst = worst.max(err.abs());
            }
        }
        assert!(worst < 40_000, "worst {worst} ns");
        assert!(
            (clock.period() - PERIOD).abs() < 25,
            "period {}",
            clock.period()
        );
    }

    /// A die 1.1 % fast: its counter ticks every 38 638 ns. Samples at tick
    /// 64 k + 20 (400 Hz). Reads every 20 ms with up to 0.3 ms of jitter; the
    /// counter read takes 0.2 ms and latches in its middle; one read in nine
    /// is preempted (2 ms, latched early). The count is read just before the
    /// counter (ending 10 µs before it starts), so a sample's time as the gyro
    /// clock defines it (the end of the count read that first shows it) is
    /// its tick plus 10 µs. The clock lands within 11 µs of it.
    #[test]
    fn the_sensor_time_places_the_accelerometer_samples() {
        const TICK: f64 = 39_062.5 / 1.011;
        const PHASE: u64 = 20;
        let host_of_tick = |x: f64| (1.0e9 + x * TICK) as i64;
        let tick_at = |t: i64| ((t as f64 - 1.0e9) / TICK) as u64;
        let mut clock = TickClock::new();
        let mut seed = 777u32;
        let mut worst = 0i64;
        for read in 0..500i64 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let before = 2_000_000_000 + read * 20_000_000 + i64::from(seed >> 8) % 300_000;
            let (after, latch) = if read % 9 == 4 {
                (before + 2_000_000, before + 100_000)
            } else {
                (before + 200_000, before + 100_000)
            };
            // The count, read just before: its newest sample.
            let newest = (tick_at(before - 10_000) - PHASE) / 64;
            let counter = (tick_at(latch) & 0x00ff_ffff) as u32;
            clock.read(counter, before as u64, after as u64, Some(newest), 2_500);
            if read >= 100 {
                let expect = host_of_tick((64 * newest + PHASE) as f64) + 10_000;
                let got = clock.time_of(newest).unwrap();
                worst = worst.max((got - expect).abs());
            }
        }
        assert!(worst < 20_000, "worst {worst} ns");
    }

    fn clock_at(period: i64) -> DieClock {
        let mut clock = DieClock::new();
        clock.nominal = period;
        clock.period = period;
        clock
    }

    #[test]
    fn the_gyroscope_is_interpolated_at_a_time() {
        let clock = clock_at(2_500_000);
        let at = |k10: i64| k10 * 250_000;
        // Samples 10..=13, x = 100 * k; sample 9 from the batch before.
        let gyro = [
            [1000, 0, -1000],
            [1100, 0, -1100],
            [1200, 0, -1200],
            [1300, 0, -1300],
        ];
        let before = Some((9, [900, 0, -900]));
        let x = |k10: i64| gyro_at(at(k10), &clock, 10, &gyro, before)[0];
        assert_eq!(x(100), 1000);
        assert_eq!(x(125), 1250);
        assert_eq!(gyro_at(at(125), &clock, 10, &gyro, before)[2], -1250);
        // Across the batch boundary, from the sample before it.
        assert_eq!(x(95), 950);
        // Before the oldest: held.
        assert_eq!(x(80), 900);
        // Past the newest: extrapolated, by at most one period.
        assert_eq!(x(135), 1350);
        assert_eq!(x(140), 1400);
        assert_eq!(x(170), 1400);
    }
}
