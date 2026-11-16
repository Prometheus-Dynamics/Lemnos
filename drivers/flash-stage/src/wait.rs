//! Bounded waits. Every wait on an on-chip stage is a deadline check, never an open spin: when a deadline
//! passes the caller ends the session, and the watchdog's reset is the exit. A single wait's budget must
//! sit below the longest period the watchdog offers, so a stuck wait is caught by the reset and never by a
//! second hang.

/// A monotonic time source, in microseconds. A chip's stage supplies its timer; the tests use a fake.
pub trait Clock {
    fn now_us(&mut self) -> u64;
}

/// A point in time after which a wait gives up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deadline {
    at_us: u64,
}

impl Deadline {
    /// The deadline `budget_us` after `now_us`.
    pub fn after(now_us: u64, budget_us: u64) -> Self {
        Self {
            at_us: now_us.saturating_add(budget_us),
        }
    }

    /// Whether `now_us` has reached the deadline.
    pub fn passed(&self, now_us: u64) -> bool {
        now_us >= self.at_us
    }
}

/// Why a bounded wait ended without its condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitError {
    /// The condition did not come true within the budget.
    Timeout { budget_us: u64 },
    /// The budget is not below the longest period, so it could outlast the reset.
    BudgetAbovePeriod { budget_us: u64 },
}

/// Polls `poll` until it yields a value, or the budget runs out. The budget must be below
/// `longest_period_us`, the longest period the watchdog offers.
pub fn wait_for<T, C: Clock>(
    clock: &mut C,
    budget_us: u64,
    longest_period_us: u64,
    mut poll: impl FnMut() -> Option<T>,
) -> Result<T, WaitError> {
    if budget_us >= longest_period_us {
        return Err(WaitError::BudgetAbovePeriod { budget_us });
    }
    let deadline = Deadline::after(clock.now_us(), budget_us);
    loop {
        if let Some(value) = poll() {
            return Ok(value);
        }
        if deadline.passed(clock.now_us()) {
            return Err(WaitError::Timeout { budget_us });
        }
    }
}

/// A clock that counts polls: each read advances one microsecond. It bounds a wait by iterations, not by
/// time, until a chip's timer is bound.
pub struct IterClock(u64);

impl IterClock {
    pub const fn new() -> Self {
        Self(0)
    }
}

impl Default for IterClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for IterClock {
    fn now_us(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clock that moves forward by `step_us` each time it is read.
    struct Fake {
        now_us: u64,
        step_us: u64,
    }

    impl Clock for Fake {
        fn now_us(&mut self) -> u64 {
            let now = self.now_us;
            self.now_us += self.step_us;
            now
        }
    }

    #[test]
    fn a_condition_that_holds_returns_at_once() {
        let mut clock = Fake {
            now_us: 0,
            step_us: 1,
        };
        assert_eq!(wait_for(&mut clock, 10, 100, || Some(7)), Ok(7));
    }

    #[test]
    fn a_condition_that_never_holds_times_out_at_the_budget() {
        let mut clock = Fake {
            now_us: 0,
            step_us: 10,
        };
        assert_eq!(
            wait_for::<(), _>(&mut clock, 50, 100, || None),
            Err(WaitError::Timeout { budget_us: 50 })
        );
    }

    #[test]
    fn a_budget_at_or_above_the_longest_period_is_refused_before_any_poll() {
        let mut clock = Fake {
            now_us: 0,
            step_us: 1,
        };
        let mut polls = 0;
        let result = wait_for(&mut clock, 100, 100, || {
            polls += 1;
            Some(())
        });
        assert_eq!(result, Err(WaitError::BudgetAbovePeriod { budget_us: 100 }));
        assert_eq!(polls, 0);
    }

    #[test]
    fn a_deadline_passes_exactly_at_its_time() {
        let deadline = Deadline::after(5, 10);
        assert!(!deadline.passed(14));
        assert!(deadline.passed(15));
    }
}
