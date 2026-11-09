use std::time::Instant;

use lemnos_core::HostClock;

/// Time since the service started.
pub(crate) struct MonotonicClock {
    start: Instant,
}

impl MonotonicClock {
    pub(crate) fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    pub(crate) fn now_us(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }
}

impl HostClock for MonotonicClock {
    fn now_ns(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }
}
