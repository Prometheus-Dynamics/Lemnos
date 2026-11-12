//! The failure rule: never give up, retry after 1 s doubling to 8 s, no
//! automatic off unless a device in the board file opts in (`auto_off_after`).

mod lane;

pub(crate) use lane::{Lane, LaneError, Work};

use std::time::Duration;

/// The longest wait between two binds of a device. The failure rule's cap: no setting goes
/// past it, and [`Options::retry_cap`] is held to it wherever it came from.
pub const MAX_RETRY_CAP: Duration = Duration::from_secs(8);

/// How the host treats its drivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// How long any one driver call may run.
    pub call_deadline: Duration,
    /// The wait before the first retry. It doubles with each failure in a row, up to the cap.
    pub retry_first: Duration,
    /// The longest wait. Held to [`MAX_RETRY_CAP`]: a longer one counts as that.
    pub retry_cap: Duration,
    /// Errors in a row that mean a device has gone away and needs rebinding.
    pub error_limit: u32,
    /// Stack of each lane thread.
    pub lane_stack: usize,
    /// The least time between two binds of one device that hints from the backend can bring
    /// about, however many arrive.
    pub hint_gap: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            call_deadline: Duration::from_secs(2),
            retry_first: Duration::from_secs(1),
            retry_cap: MAX_RETRY_CAP,
            error_limit: 5,
            lane_stack: 256 * 1024,
            hint_gap: Duration::from_millis(250),
        }
    }
}

impl Options {
    /// The cap in force: [`retry_cap`](Self::retry_cap), never more than [`MAX_RETRY_CAP`].
    pub(crate) fn cap(&self) -> Duration {
        self.retry_cap.min(MAX_RETRY_CAP)
    }

    /// The wait before retry number `attempt` (0 is the first).
    pub(crate) fn backoff(&self, attempt: u32) -> Duration {
        let cap = self.cap();
        let doubled = self
            .retry_first
            .checked_mul(1u32.checked_shl(attempt).unwrap_or(u32::MAX))
            .unwrap_or(cap);
        doubled.min(cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_the_cap_and_stays() {
        let options = Options::default();
        let waits: Vec<u64> = (0..7).map(|n| options.backoff(n).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 8, 8, 8]);
        assert_eq!(options.backoff(u32::MAX), Duration::from_secs(8));
    }

    #[test]
    fn a_cap_past_the_rules_is_held_to_it() {
        let options = Options {
            retry_first: Duration::from_secs(5),
            retry_cap: Duration::from_secs(60),
            ..Options::default()
        };
        let waits: Vec<u64> = (0..4).map(|n| options.backoff(n).as_secs()).collect();
        assert_eq!(waits, [5, 8, 8, 8]);
        let shorter = Options {
            retry_first: Duration::from_millis(100),
            retry_cap: Duration::from_millis(400),
            ..Options::default()
        };
        let waits: Vec<u128> = (0..4).map(|n| shorter.backoff(n).as_millis()).collect();
        assert_eq!(waits, [100, 200, 400, 400]);
    }
}
