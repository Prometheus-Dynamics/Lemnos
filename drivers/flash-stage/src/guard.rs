//! The watchdog as a stage sees it, and the entry contract every stage keeps.
//!
//! Order, which a stage follows from its first instruction:
//! 1. **Arm before anything.** [`start`] arms the watchdog at the longest period, then reads the arming
//!    back. A stage that cannot read its own arming does not run: [`StartError::NotArmed`].
//! 2. **Lower the period** to the smallest one that covers the session's budget, so a stuck session is
//!    caught in bounded time.
//! 3. **Feed only after verified work.** The serve loop is the only caller of [`Watchdog::feed`], and it
//!    feeds once per verified round (see `serve`). A failed round is never fed.
//! 4. **Report the cause before any reset.** A session that ends in a failure hands its [`Cause`] to a
//!    [`Report`] before it parks: a reset that comes later is the exit, and the cause is written first.
//!
//! Nothing here knows which chip. The chip implements [`Guard`] over its own registers.

use core::hint::spin_loop;

/// Feeds the watchdog: restarts its counter.
pub trait Watchdog {
    fn feed(&mut self);
    /// Feeds the watchdog and says when: `now_us` is the instant of the feed, read from the link's clock at the point the
    /// round's work was verified. The session counts its idle deadline from it. A watchdog that does not need the instant
    /// uses the default, which is `feed`.
    fn feed_at(&mut self, now_us: u64) {
        let _ = now_us;
        self.feed();
    }
}

/// A chip's watchdog, as the stage's start and its feed use it.
pub trait Guard: Watchdog {
    /// Arms the watchdog at its longest period. The arming is written, not assumed: see [`Guard::armed`].
    fn arm(&mut self);
    /// Whether the watchdog reads back as armed. Read from the device, not from a shadow.
    fn armed(&mut self) -> bool;
    /// The highest `top` (period index) the watchdog offers.
    fn max_top(&self) -> u8;
    /// The period, in microseconds, that `top` gives.
    fn period_us(&self, top: u8) -> u64;
    /// Lowers the period to `top` and restarts the counter with it.
    fn set_top(&mut self, top: u8);
}

/// Why the start failed. Either way the stage does not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartError {
    /// The watchdog does not read back as armed.
    NotArmed,
    /// No period covers the budget: the budget is above the longest period.
    NoPeriod { budget_us: u64 },
}

/// The smallest `top` whose period covers `budget_us`, if any.
pub fn smallest_top_covering<G: Guard + ?Sized>(guard: &G, budget_us: u64) -> Option<u8> {
    (0..=guard.max_top()).find(|&top| guard.period_us(top) >= budget_us)
}

/// The entry contract's first step: arm, read the arming back, then set the session period to cover
/// `budget_us`. Returns the period index chosen.
pub fn start<G: Guard>(guard: &mut G, budget_us: u64) -> Result<u8, StartError> {
    guard.arm();
    if !guard.armed() {
        return Err(StartError::NotArmed);
    }
    let top = smallest_top_covering(guard, budget_us).ok_or(StartError::NoPeriod { budget_us })?;
    guard.set_top(top);
    Ok(top)
}

/// Why a session ended without its normal exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The entry contract's start failed.
    Start(StartError),
    /// The chip's own bring-up failed at `step` (the chip numbers its steps).
    Setup { step: u8 },
    /// The serve loop ended before the end frame.
    Serve(crate::serve::ServeError),
    /// The end frame arrived; the reset it asked for is the exit.
    Ended(lemnos_flash_wire::frame::ResetKind),
}

/// Where a cause is written before the stage parks. A chip with no channel that survives a reset
/// implements it as a no-op, and says so in its own record.
pub trait Report {
    fn cause(&mut self, cause: Cause);
}

/// Reports the cause, then parks. The order is the rule: the report comes first, then [`park`], which never
/// feeds. The reset that follows is the exit. `park` is called directly, not through a function pointer: a
/// pointer to it would be a GOT entry in a position-independent image (see the Rockchip stage's `check-pic`).
pub fn finish<R: Report>(report: &mut R, cause: Cause) -> ! {
    report.cause(cause);
    park()
}

/// Parks without feeding the watchdog.
pub fn park() -> ! {
    loop {
        spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A watchdog model: arming can be made to fail, the periods double from 1 ms.
    struct Dog {
        armed: bool,
        refuse_arm: bool,
        top: Option<u8>,
        feeds: u32,
        log: std::vec::Vec<&'static str>,
    }

    impl Dog {
        fn new(refuse_arm: bool) -> Self {
            Self {
                armed: false,
                refuse_arm,
                top: None,
                feeds: 0,
                log: std::vec::Vec::new(),
            }
        }
    }

    impl Watchdog for Dog {
        fn feed(&mut self) {
            self.feeds += 1;
            self.log.push("feed");
        }
    }

    impl Guard for Dog {
        fn arm(&mut self) {
            self.log.push("arm");
            self.armed = !self.refuse_arm;
        }
        fn armed(&mut self) -> bool {
            self.log.push("read-back");
            self.armed
        }
        fn max_top(&self) -> u8 {
            7
        }
        fn period_us(&self, top: u8) -> u64 {
            1_000 << top
        }
        fn set_top(&mut self, top: u8) {
            self.log.push("set-top");
            self.top = Some(top);
        }
    }

    #[test]
    fn the_start_arms_reads_back_and_only_then_lowers_the_period() {
        let mut dog = Dog::new(false);
        assert_eq!(start(&mut dog, 3_000), Ok(2), "1000 << 2 covers 3000");
        assert_eq!(dog.log, vec!["arm", "read-back", "set-top"]);
        assert_eq!(dog.top, Some(2));
    }

    #[test]
    fn a_watchdog_that_does_not_read_back_as_armed_stops_the_start_before_any_period_is_set() {
        let mut dog = Dog::new(true);
        assert_eq!(start(&mut dog, 3_000), Err(StartError::NotArmed));
        assert_eq!(dog.log, vec!["arm", "read-back"]);
        assert_eq!(dog.top, None);
    }

    #[test]
    fn a_budget_no_period_covers_is_refused_after_arming() {
        let mut dog = Dog::new(false);
        assert_eq!(
            start(&mut dog, 1_000 << 8),
            Err(StartError::NoPeriod {
                budget_us: 1_000 << 8
            })
        );
        assert_eq!(dog.top, None);
    }

    #[test]
    fn the_smallest_period_that_covers_is_chosen() {
        let dog = Dog::new(false);
        assert_eq!(smallest_top_covering(&dog, 1), Some(0));
        assert_eq!(smallest_top_covering(&dog, 1_000), Some(0));
        assert_eq!(smallest_top_covering(&dog, 1_001), Some(1));
        assert_eq!(smallest_top_covering(&dog, 1_000 << 8), None);
    }

    #[test]
    fn the_cause_reaches_the_report_with_its_value() {
        struct Log(Option<Cause>);
        impl Report for Log {
            fn cause(&mut self, cause: Cause) {
                self.0 = Some(cause);
            }
        }
        let mut log = Log(None);
        log.cause(Cause::Setup { step: 3 });
        assert_eq!(log.0, Some(Cause::Setup { step: 3 }));
    }
}
