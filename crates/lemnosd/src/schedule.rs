//! The read scheduler's rules, as plain functions of times (microseconds on
//! the boot clock). The service feeds them the sensors and asks what to read.
//!
//! - A sensor's deadlines lie on a fixed grid from its first read. A late read
//!   does not move the grid, and deadlines that already passed are skipped,
//!   not replayed in a burst.
//! - The most overdue due sensor goes first. On a tie the cheaper read goes
//!   first, so a slow device does not delay a fast one that is due at the same
//!   time.
//! - A read that would still run when a cheaper sensor's deadline comes waits
//!   for that deadline, and the cheaper sensor goes first. Only a cheaper one
//!   counts: an expensive read never jumps ahead of a cheap one that is due
//!   first. A sensor a whole period late runs regardless, so a slow device is
//!   never starved.
//!
//! One thread cannot interrupt a bus transaction. So this holds a fast sensor's
//! rate as long as no single read is longer than the gap it has to fill. A
//! read longer than the fast sensor's period still delays it.

/// A sensor, as the scheduler sees it. Times are microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Due {
    /// The sensor's slot.
    pub index: usize,
    /// Its next deadline.
    pub deadline_us: u64,
    pub period_us: u64,
    /// How long its last read took (0 before the first).
    pub cost_us: u64,
}

/// What to do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Next {
    /// Read this slot now.
    Run(usize),
    /// Nothing may run before this time: a deferred read waits for it.
    WaitUntil(u64),
    /// No sensor is due.
    Idle,
}

/// The first grid point after `now_us` for a grid that starts at
/// `deadline_us`. Called after a read at `now_us` (a late read moves the
/// deadline on, skipping the points it missed).
pub(crate) fn advance(deadline_us: u64, period_us: u64, now_us: u64) -> u64 {
    let period = period_us.max(1);
    let missed = now_us.saturating_sub(deadline_us) / period;
    deadline_us + (missed + 1) * period
}

/// Chooses the next read among `sensors` at `now_us`.
pub(crate) fn choose(sensors: &[Due], now_us: u64) -> Next {
    let Some(first) = sensors
        .iter()
        .filter(|s| s.deadline_us <= now_us)
        .min_by_key(|s| (s.deadline_us, s.cost_us, s.index))
    else {
        return Next::Idle;
    };
    let late = now_us - first.deadline_us;
    if late >= first.period_us {
        return Next::Run(first.index);
    }
    // The read would still be running at `horizon`: a cheaper sensor whose
    // deadline falls in (first's deadline, horizon] would be made late by it.
    let horizon = now_us + first.cost_us;
    let blocker = sensors
        .iter()
        .filter(|s| {
            s.index != first.index
                && s.cost_us < first.cost_us
                && s.deadline_us > first.deadline_us
                && s.deadline_us <= horizon
        })
        .min_by_key(|s| (s.deadline_us, s.index));
    match blocker {
        None => Next::Run(first.index),
        Some(b) if b.deadline_us > now_us => Next::WaitUntil(b.deadline_us),
        Some(b) => Next::Run(b.index),
    }
}

/// Stamps a buffered device's batches on the device's own clock.
///
/// A device with a buffer (an IMU's FIFO) takes samples on its own oscillator,
/// which runs off its nominal rate: board #1's accelerometer ran 1 % fast
/// (403.9 samples a second at a nominal 400 Hz). Stamping its samples the
/// nominal period apart would fall behind the reads and squeeze the stamps
/// at every batch, so the clock measures the period.
///
/// Every read bounds the device's next sample: it had not arrived when the
/// read started (the read did not find it), and some reads start just before
/// one arrives. So over a second of reads, the read that started latest
/// against the sample count lies on the device's own clock, within a fraction
/// of a period. A read the bus thread was preempted in after its start found
/// more samples than were there at its start, so it only ever looks early:
/// under load, picking the earliest read instead measured preemption, and
/// the period went 0.02 % off in the test below and on board #1 under
/// load. The clock keeps that read for each of the
/// last [`BLOCKS`] seconds; the period is the slope between the oldest and
/// the newest. (Stamping on that line instead jumps by tens of µs at every
/// block: the latest read of a second is up to a few percent of a period
/// before its sample.)
///
/// Each batch's newest sample arrived after `started_us - period` (the device
/// adds one each period, and the read found it) and before `ended_us` (the
/// read took it). Within that window the stamp follows the stream: one
/// measured period per sample on from the previous batch, so a read delayed
/// after it started (the bus thread preempted before the drain) does not move
/// the samples. Outside it (samples lost to a full buffer) the window's
/// nearer edge wins, and the measurement starts over. The first batch starts
/// the stream in the window's middle. A stamp never goes back: the batch's
/// first comes after the previous batch's last.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct StreamClock {
    /// The last sample's stamp, in ns (0: no batch yet).
    last_ns: u64,
    /// The measured period in ns (0: not measured yet, the nominal one is used).
    period_ns: u64,
    /// Samples counted since the measurement started.
    samples: u64,
    /// The current block: when it started, and its latest read against the
    /// count (samples, read start in ns; samples 0: none yet).
    block_ns: u64,
    best: (u64, u64),
    /// The latest read of each finished block, oldest at `head`.
    blocks: [(u64, u64); BLOCKS],
    filled: usize,
    head: usize,
}

/// Seconds of reads the period is measured over. The oscillator drifts as
/// the device warms (board #1's accelerometer: 650 ppm over 90 s under
/// load), and a longer window lags it: in a replay of that run's reads, 16 s
/// gave 95 stamps nudged back into a read's window in 145 s, 8 s gave 38,
/// 4 s gave 2.
const BLOCKS: usize = 4;
const BLOCK_NS: u64 = 1_000_000_000;

impl StreamClock {
    /// Stamps a batch of `count` samples, drained by a read that ran from
    /// `started_us` to `ended_us`, from a device whose nominal period is
    /// `nominal_us` (0: one sample a read, no buffer). Returns the newest
    /// sample's stamp and the period between the samples, both in ns.
    pub(crate) fn batch(
        &mut self,
        started_us: u64,
        ended_us: u64,
        count: u64,
        nominal_us: u64,
    ) -> (u64, u64) {
        let nominal = nominal_us * 1000;
        let started = started_us * 1000;
        let period = if self.period_ns == 0 {
            nominal
        } else {
            self.period_ns
        };
        let earliest = started.saturating_sub(period);
        let latest = ended_us.max(started_us) * 1000;
        // The first batch's newest sample arrived somewhere in the period
        // before the read: its middle.
        let expected = if self.last_ns == 0 {
            started.saturating_sub(period / 2)
        } else {
            self.last_ns + count * period
        };
        let mut stamp = expected.clamp(earliest, latest);
        if nominal != 0 {
            // Off by more than a period: samples lost, or a stall. The count
            // no longer follows the device's clock; measure again.
            if self.last_ns != 0 && stamp.abs_diff(expected) > period {
                self.restart();
            }
            self.measure(started, count, nominal, period);
        }
        if self.last_ns != 0 {
            stamp = stamp.max(self.last_ns + count.saturating_sub(1) * period + 1000);
        }
        self.last_ns = stamp;
        (stamp, period)
    }

    fn restart(&mut self) {
        self.samples = 0;
        self.block_ns = 0;
        self.best = (0, 0);
        self.filled = 0;
        self.head = 0;
    }

    /// Counts a read that found `count` samples, started at `started` (ns).
    fn measure(&mut self, started: u64, count: u64, nominal: u64, period: u64) {
        self.samples += count;
        // The read's start against the count on the clock as measured so
        // far. The latest is the one that started just before the device's
        // next sample: a read that is late past its start (the bus thread
        // preempted before the length read) finds more samples, so it only
        // ever looks early, and is never picked.
        let late = |(n, at): (u64, u64)| at as i128 - (n * period) as i128;
        if self.best.0 == 0 || late((self.samples, started)) > late(self.best) {
            self.best = (self.samples, started);
        }
        if self.block_ns == 0 {
            self.block_ns = started;
        }
        if started.saturating_sub(self.block_ns) < BLOCK_NS {
            return;
        }
        // The block is done: keep its latest read, dropping the oldest.
        let slot = (self.head + self.filled) % BLOCKS;
        self.blocks[slot] = self.best;
        if self.filled < BLOCKS {
            self.filled += 1;
        } else {
            self.head = (self.head + 1) % BLOCKS;
        }
        self.best = (0, 0);
        self.block_ns = started;
        if self.filled < 2 {
            return;
        }
        let (n0, t0) = self.blocks[self.head];
        let (n1, t1) = self.blocks[slot];
        if n1 <= n0 || t1 <= t0 {
            return;
        }
        let period = (t1 - t0) / (n1 - n0);
        // Beyond 5 % off the nominal rate is not the oscillator: ignore it.
        if period.abs_diff(nominal) <= nominal / 20 {
            self.period_ns = period;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sensor(index: usize, deadline_us: u64, cost_us: u64) -> Due {
        Due {
            index,
            deadline_us,
            period_us: 10_000,
            cost_us,
        }
    }

    #[test]
    fn deadlines_stay_on_the_grid_and_skip_missed_ones() {
        // On time: the next point is one period on.
        assert_eq!(advance(0, 10_000, 0), 10_000);
        // Read 4 ms late: the grid does not drift.
        assert_eq!(advance(0, 10_000, 4_000), 10_000);
        assert_eq!(advance(10_000, 10_000, 14_000), 20_000);
        // Read 25 ms late: the missed points are skipped, not replayed.
        assert_eq!(advance(0, 10_000, 25_000), 30_000);
    }

    #[test]
    fn idle_when_nothing_is_due() {
        assert_eq!(choose(&[sensor(0, 5_000, 0)], 4_000), Next::Idle);
        assert_eq!(choose(&[], 4_000), Next::Idle);
    }

    #[test]
    fn the_most_overdue_goes_first() {
        let sensors = [sensor(0, 3_000, 0), sensor(1, 1_000, 0)];
        assert_eq!(choose(&sensors, 4_000), Next::Run(1));
    }

    #[test]
    fn ties_go_to_the_cheaper_read() {
        // A slow device listed first does not delay the fast one.
        let sensors = [sensor(0, 0, 6_000), sensor(1, 0, 200)];
        assert_eq!(choose(&sensors, 0), Next::Run(1));
    }

    #[test]
    fn a_read_that_would_make_a_deadline_late_waits_for_it() {
        // The slow read (6 ms) started at 2 ms would run past the fast
        // sensor's 5 ms deadline, so the fast one goes first.
        let sensors = [sensor(0, 0, 6_000), sensor(1, 5_000, 200)];
        assert_eq!(choose(&sensors, 2_000), Next::WaitUntil(5_000));
        assert_eq!(choose(&sensors, 5_000), Next::Run(1));
        // After it, the slow one has room before the next fast deadline.
        let later = [sensor(0, 0, 6_000), sensor(1, 15_000, 200)];
        assert_eq!(choose(&later, 5_200), Next::Run(0));
    }

    #[test]
    fn an_expensive_read_does_not_jump_a_cheap_one_due_just_after_it() {
        // The IMU is due 35 us before the slow device. Running the slow read
        // first would make the IMU 6 ms late.
        let sensors = [sensor(0, 1_000, 6_100), sensor(1, 1_035, 6)];
        assert_eq!(choose(&sensors, 1_040), Next::Run(1));
    }

    #[test]
    fn a_read_a_period_late_runs_anyway() {
        let sensors = [sensor(0, 0, 6_000), sensor(1, 15_000, 200)];
        assert_eq!(choose(&sensors, 10_000), Next::Run(0));
    }

    /// Stamps in µs, as `batch` gives them in ns.
    fn us(clock: &mut StreamClock, started: u64, ended: u64, count: u64, nominal: u64) -> u64 {
        clock.batch(started, ended, count, nominal).0 / 1000
    }

    #[test]
    fn batch_stamps_follow_the_stream_and_never_go_back() {
        let p = 2_500;
        let mut c = StreamClock::default();
        // The first batch: its newest sample came in the period before the
        // read; stamped in that period's middle.
        assert_eq!(us(&mut c, 100_000, 100_300, 4, p), 98_750);
        // On time: four more samples, four periods on.
        assert_eq!(us(&mut c, 110_000, 110_300, 4, p), 108_750);
        // Preempted after starting: the read started at 120 ms but drained
        // at 125.3 ms and found six samples. Stamped from the stream (the
        // last at 123.75 ms), not counted back from 120 ms, which would put
        // the first at 107.5 ms, before the previous batch's last.
        assert_eq!(us(&mut c, 120_000, 125_300, 6, p), 123_750);
        // Two samples by the stream put the newest at 128.75 ms, but it is
        // younger than one period at the read's start (133 ms): the window's
        // edge wins.
        assert_eq!(us(&mut c, 133_000, 133_300, 2, p), 130_500);
        // Samples lost to a full buffer: the stream is behind; snap forward.
        assert_eq!(us(&mut c, 500_000, 500_400, 32, p), 497_500);
        // Never back, even with a read window that says otherwise.
        let mut c = StreamClock::default();
        assert_eq!(us(&mut c, 128_500, 128_600, 1, p), 127_250);
        assert_eq!(us(&mut c, 129_000, 129_100, 4, p), 134_751);
        // One sample a read (no buffer): the read's start, strictly after.
        let mut c = StreamClock::default();
        assert_eq!(us(&mut c, 10_000, 10_200, 1, 0), 10_000);
        assert_eq!(us(&mut c, 10_000, 10_200, 1, 0), 10_001);
    }

    /// A device 1 % fast (as board #1's accelerometer) or 1 % slow, read
    /// every 20 ms for two minutes with up to 3 ms of jitter and one read in
    /// eight preempted: the period is measured within 0.005 % of the
    /// device's, and the stamps step by it.
    #[test]
    fn a_device_clock_off_its_rate_is_measured_and_stamped_evenly() {
        stamp_device(2_475_000); // 404.04 Hz against a nominal 400 Hz
        stamp_device(2_525_000); // 396.04 Hz
    }

    fn stamp_device(device_ns: u64) {
        let mut c = StreamClock::default();
        let mut taken = 0u64;
        let mut stamps = Vec::new();
        let mut seed = 1u64;
        for read in 0..6_000u64 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let jitter = (seed >> 33) % 3_000;
            let started = 1_000_000 + read * 20_000 + jitter;
            // One read in eight is preempted for up to 5 ms after its start,
            // before it reads the buffer's length.
            let delay = if (seed >> 20).is_multiple_of(8) {
                (seed >> 40) % 5_000
            } else {
                0
            };
            // Samples the device took by the length read (it started at 1 s).
            let have = ((started + delay - 1_000_000) * 1000) / device_ns;
            let count = have - taken;
            taken = have;
            if count == 0 {
                continue;
            }
            let (last, period) = c.batch(started, started + delay + 400, count, 2_500);
            for k in (0..count).rev() {
                stamps.push(last - k * period);
            }
        }
        assert!(
            c.period_ns.abs_diff(device_ns) * 20_000 <= device_ns,
            "{}",
            c.period_ns
        );
        let steps: Vec<u64> = stamps.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(steps.iter().all(|&s| s > 0));
        // After the first 30 s: 999 steps in 1000 within 1 µs of the period,
        // none off by more than 100 µs (the stream nudged back into a read's
        // window: the 4 s measurement is off by up to 0.005 %).
        let late = &steps[steps.len() / 4..];
        let off = |s: &u64| s.abs_diff(c.period_ns);
        assert!(late.iter().filter(|s| off(s) > 1_000).count() * 1000 <= late.len());
        assert!(
            late.iter().all(|s| off(s) <= 100_000),
            "{:?}",
            late.iter().map(off).max()
        );
    }
}
