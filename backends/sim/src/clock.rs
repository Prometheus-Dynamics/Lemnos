use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lemnos_core::HostClock;

/// Time that moves only when told to. Clones share it.
#[derive(Debug, Clone, Default)]
pub struct SimClock(Arc<AtomicU64>);

impl SimClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn now_ns(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    pub fn advance(&self, by: Duration) {
        self.0.fetch_add(by.as_nanos() as u64, Ordering::AcqRel);
    }
}

impl HostClock for SimClock {
    fn now_ns(&self) -> u64 {
        SimClock::now_ns(self)
    }
}
