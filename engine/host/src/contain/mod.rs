//! The failure rule: never give up, retry after 1 s doubling to 8 s, no
//! automatic off unless a board explicitly configures it.

mod lane;

pub(crate) use lane::{Lane, LaneError, Work};

use std::time::Duration;

/// An explicit, opt-in policy: stop retrying a device after this many failed
/// attempts in a row. Off unless configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoOff {
    pub after_attempts: u32,
}

/// How the host treats its drivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// How long any one driver call may run.
    pub call_deadline: Duration,
    pub retry_first: Duration,
    pub retry_cap: Duration,
    /// Errors in a row that mean a device has gone away and needs rebinding.
    pub error_limit: u32,
    pub auto_off: Option<AutoOff>,
    /// Stack of each lane thread.
    pub lane_stack: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            call_deadline: Duration::from_secs(2),
            retry_first: Duration::from_secs(1),
            retry_cap: Duration::from_secs(8),
            error_limit: 5,
            auto_off: None,
            lane_stack: 256 * 1024,
        }
    }
}

impl Options {
    /// The wait before retry number `attempt` (0 is the first).
    pub(crate) fn backoff(&self, attempt: u32) -> Duration {
        let doubled = self
            .retry_first
            .checked_mul(1u32.checked_shl(attempt).unwrap_or(u32::MAX))
            .unwrap_or(self.retry_cap);
        doubled.min(self.retry_cap)
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
}
