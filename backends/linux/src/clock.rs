//! The host clock on Linux: `CLOCK_MONOTONIC`, the clock every time Lemnos hands out is on.

use std::time::Instant;

use lemnos_core::HostClock;
use lemnos_linux_sys::time::monotonic_ns;

/// `CLOCK_MONOTONIC` in nanoseconds. Should a reading fail, the time since the first reading is
/// added to that reading: the same clock, as the standard library's `Instant` reads it.
pub(crate) struct MonotonicClock {
    base_ns: u64,
    start: Instant,
}

impl MonotonicClock {
    pub(crate) fn new() -> Self {
        Self {
            base_ns: monotonic_ns().unwrap_or(0),
            start: Instant::now(),
        }
    }
}

impl HostClock for MonotonicClock {
    fn now_ns(&self) -> u64 {
        monotonic_ns().unwrap_or_else(|_| {
            self.base_ns
                .saturating_add(u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_clock_reads_monotonic_time_and_never_goes_back() {
        let clock = MonotonicClock::new();
        let first = clock.now_ns();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = clock.now_ns();
        assert!(second >= first + 1_000_000, "{first} then {second}");
    }
}
