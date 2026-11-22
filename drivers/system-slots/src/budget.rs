//! One tick's share of an install's work, bounded by time as well as by bytes.
//!
//! A tick runs inside the host's call deadline (`call_deadline_ms`, 2000 ms by default). How long a
//! number of bytes takes depends on the board: SHA-256 and the store's write speed differ by an
//! order of magnitude across the shipped targets, and a slow card or an emulated disk is slower
//! still. So a tick's work is cut into pieces of [`PIECE`] bytes, and the tick stops at the first
//! piece boundary past a quarter of the deadline ([`SHARE`]) or past [`crate::install::READ_EVERY_TICK`]
//! bytes, whichever comes first. One piece always runs, so the install moves on however slow the
//! board is. A flush (fsync) never runs on a tick: it runs on the flush worker, and the tick only
//! polls it.

/// The bytes of one piece: what a tick reads, hashes or writes between two looks at the clock.
pub const PIECE: usize = 64 << 10;

/// The share of the call deadline one tick may spend: a quarter.
pub const SHARE: u64 = 4;

/// The deadline a tick is bounded by when the host gives none (a driver built alone, a test): the
/// host's default.
pub const DEFAULT_DEADLINE_MS: u64 = 2000;

/// A clock in nanoseconds that only goes forward. The driver's is the monotonic clock; a test
/// gives its own, to stand in for a slow board.
pub type Clock = std::sync::Arc<dyn Fn() -> u64 + Send + Sync>;

/// The monotonic clock, in nanoseconds since its first read.
pub fn monotonic() -> Clock {
    let start = std::time::Instant::now();
    std::sync::Arc::new(move || u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX))
}

/// The time a tick may spend, from the call deadline in milliseconds (0: [`DEFAULT_DEADLINE_MS`]).
pub fn limit_ns(deadline_ms: u64) -> u64 {
    let ms = if deadline_ms == 0 {
        DEFAULT_DEADLINE_MS
    } else {
        deadline_ms
    };
    ms.saturating_mul(1_000_000) / SHARE
}

/// One tick's budget.
pub struct Budget<'a> {
    clock: &'a (dyn Fn() -> u64 + Send + Sync),
    start: u64,
    limit_ns: u64,
    bytes: u64,
    cap: u64,
}

impl<'a> Budget<'a> {
    /// A tick's budget from now: `limit_ns` of time and `cap` bytes.
    pub fn new(clock: &'a (dyn Fn() -> u64 + Send + Sync), limit_ns: u64, cap: u64) -> Self {
        Self {
            start: clock(),
            clock,
            limit_ns,
            bytes: 0,
            cap,
        }
    }

    /// Counts `n` bytes of work done.
    pub fn spend(&mut self, n: usize) {
        self.bytes = self.bytes.saturating_add(n as u64);
    }

    /// Whether the tick has used its share: its bytes, or its time. Asked after each piece, so a
    /// tick always does one.
    pub fn spent(&self) -> bool {
        self.bytes >= self.cap || (self.clock)().saturating_sub(self.start) >= self.limit_ns
    }

    /// The bytes the next piece may take: [`PIECE`], or less when the byte cap is nearer.
    pub fn piece(&self) -> usize {
        usize::try_from(self.cap.saturating_sub(self.bytes))
            .unwrap_or(usize::MAX)
            .clamp(1, PIECE)
    }

    /// The time the tick has used so far, in nanoseconds.
    pub fn used_ns(&self) -> u64 {
        (self.clock)().saturating_sub(self.start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn a_quarter_of_the_deadline_and_the_default_when_none_is_given() {
        assert_eq!(limit_ns(2000), 500_000_000);
        assert_eq!(limit_ns(0), 500_000_000);
        assert_eq!(limit_ns(60_000), 15_000_000_000);
    }

    #[test]
    fn the_budget_ends_at_its_time_or_its_bytes_whichever_comes_first() {
        let now = AtomicU64::new(0);
        let clock = || now.load(Ordering::Relaxed);
        let mut budget = Budget::new(&clock, 100, 1 << 20);
        assert!(!budget.spent());
        budget.spend(PIECE);
        now.store(99, Ordering::Relaxed);
        assert!(!budget.spent());
        now.store(100, Ordering::Relaxed);
        assert!(budget.spent(), "the time is used");
        let mut bytes = Budget::new(&clock, u64::MAX, 2 * PIECE as u64);
        bytes.spend(PIECE);
        assert_eq!(bytes.piece(), PIECE);
        bytes.spend(PIECE);
        assert!(bytes.spent(), "the bytes are used");
    }
}
