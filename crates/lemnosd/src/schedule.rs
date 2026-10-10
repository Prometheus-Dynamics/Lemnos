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
}
