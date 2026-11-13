use std::time::Instant;

use lemnos_core::HostClock;
use lemnos_linux_sys::time::monotonic_ns;

/// The host's clock: `CLOCK_MONOTONIC` nanoseconds, the clock every time Lemnos hands out on
/// Linux is on. A driver's `tick(now_ns)`, so a frame or completion it stamps with it, a value's
/// stamp on the wire, and the kernel's `ktime_get_ns()` all compare directly.
pub(crate) struct MonotonicClock {
    /// The clock at `start`, for the reading that can't fail.
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

    pub(crate) fn now_us(&self) -> u64 {
        self.now_ns() / 1_000
    }
}

impl HostClock for MonotonicClock {
    fn now_ns(&self) -> u64 {
        // `CLOCK_MONOTONIC` is always there on Linux. Should a reading fail, the time since the
        // first reading is added to that reading: the same clock, as the standard library's
        // `Instant` reads it.
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
    fn the_host_clock_is_clock_monotonic() {
        let clock = MonotonicClock::new();
        let before = monotonic_ns().expect("monotonic clock");
        let now = clock.now_ns();
        let after = monotonic_ns().expect("monotonic clock");
        assert!(
            before <= now && now <= after,
            "{before} <= {now} <= {after}"
        );
        let us = clock.now_us();
        let later = monotonic_ns().expect("monotonic clock");
        assert!(after / 1_000 <= us && us <= later / 1_000);
    }
}
