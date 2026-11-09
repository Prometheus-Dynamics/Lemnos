//! Retry delays for both halves of the service: 1 s, doubling, capped at 8 s.
//! Nothing gives up; the delay only stops growing.

use std::time::Duration;

const FIRST: Duration = Duration::from_secs(1);
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
        let delays: Vec<u64> = (0..6).map(|_| backoff.next_delay().as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 8, 8]);
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
