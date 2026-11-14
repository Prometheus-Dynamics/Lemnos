//! Retry delays for every reconnect of the service: 20 ms, doubling, capped at 8 s.
//! Nothing gives up; the delay only stops growing. The first try after a loss is made at once,
//! by the caller, before any delay: this is the delay after the first failed try. It starts
//! small because the usual cause is a peer that is restarting and is back within tens of
//! milliseconds; a long first delay was a measured second of waiting on every comm core restart.

use std::time::Duration;

const FIRST: Duration = Duration::from_millis(20);
const CAP: Duration = Duration::from_secs(8);

#[derive(Debug, Clone)]
pub struct Backoff {
    next: Duration,
}

impl Backoff {
    pub const fn new() -> Self {
        Self { next: FIRST }
    }

    /// The delay before the next try. Each call doubles the one after it, up to the cap.
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = (delay * 2).min(CAP);
        delay
    }

    /// Back to the first delay, after a try worked.
    pub fn reset(&mut self) {
        self.next = FIRST;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_to_the_cap_and_stays_there() {
        let mut backoff = Backoff::new();
        let delays: Vec<u64> = (0..12)
            .map(|_| backoff.next_delay().as_millis() as u64)
            .collect();
        assert_eq!(
            delays,
            [
                20, 40, 80, 160, 320, 640, 1280, 2560, 5120, 8000, 8000, 8000
            ]
        );
    }

    #[test]
    fn a_success_starts_over() {
        let mut backoff = Backoff::new();
        let _ = backoff.next_delay();
        let _ = backoff.next_delay();
        backoff.reset();
        assert_eq!(backoff.next_delay(), FIRST);
    }
}
