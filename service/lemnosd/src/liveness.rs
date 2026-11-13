//! The daemon's own liveness, which a keep-alive device (a hardware watchdog, see
//! `docs/daemon.md`) is fed by. The host ticks such a device only while [`DaemonLiveness`] says the
//! daemon is healthy, so a daemon that is not healthy stops feeding it and the board is reset.
//!
//! Healthy means both of these hold:
//!
//! - the main loop is turning: it finished a pass within [`TURN_BOUND`]. A pass hands the drivers'
//!   ticks to their lanes and does not wait on a driver's work, except for the call deadline of a
//!   driver that is stuck, which is what the bound leaves room for. The gate itself is asked from
//!   the loop's pass, so a loop that is wedged asks nothing and feeds nothing.
//! - the control socket answers: a probe connects to it and gets the handshake back from the
//!   serving thread within [`ANSWER_BOUND`]. The probe runs every [`PROBE_EVERY`].
//!
//! A driver that panics or hangs is not part of this: it is contained on its own lane, and the
//! keep-alive device is fed through that.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::Core;

/// How old the loop's last finished pass may be and still count.
pub(crate) const TURN_BOUND: Duration = Duration::from_secs(10);
/// How old the socket's last answer may be and still count.
pub(crate) const ANSWER_BOUND: Duration = Duration::from_secs(5);
/// How often the probe asks the socket.
pub(crate) const PROBE_EVERY: Duration = Duration::from_secs(1);

/// The stamps the loop and the probe leave, on a clock of their own: nanoseconds since `start`,
/// plus one, so that zero means never.
pub(crate) struct DaemonLiveness {
    start: Instant,
    turned: AtomicU64,
    answered: AtomicU64,
}

impl DaemonLiveness {
    pub(crate) fn new() -> Self {
        Self::starting(Instant::now())
    }

    fn starting(start: Instant) -> Self {
        Self {
            start,
            turned: AtomicU64::new(0),
            answered: AtomicU64::new(0),
        }
    }

    fn stamp(&self) -> u64 {
        let elapsed = u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        elapsed.saturating_add(1)
    }

    /// The main loop finished a pass.
    pub(crate) fn turned(&self) {
        self.turned.store(self.stamp(), Ordering::Release);
    }

    /// The control socket answered the probe.
    pub(crate) fn answered(&self) {
        self.answered.store(self.stamp(), Ordering::Release);
    }

    /// Whether the daemon is healthy now.
    pub(crate) fn healthy(&self) -> bool {
        self.healthy_at(self.stamp())
    }

    fn healthy_at(&self, now: u64) -> bool {
        fresh(self.turned.load(Ordering::Acquire), now, TURN_BOUND)
            && fresh(self.answered.load(Ordering::Acquire), now, ANSWER_BOUND)
    }
}

/// Whether `stamp` was taken within `bound` before `now`. Zero, never taken, is not.
fn fresh(stamp: u64, now: u64, bound: Duration) -> bool {
    stamp != 0 && now.saturating_sub(stamp) <= u64::try_from(bound.as_nanos()).unwrap_or(u64::MAX)
}

impl lemnos_host::Liveness for DaemonLiveness {
    fn alive(&self) -> bool {
        self.healthy()
    }
}

/// The probe: asks the control socket every [`PROBE_EVERY`] until the service stops. A connection
/// is a full handshake through the serving thread, so an answer proves that thread works.
pub(crate) fn probe(core: &Arc<Core>, liveness: &DaemonLiveness, socket: &Path) {
    while !core.stopping() {
        if let Ok(client) = lemnos_client::Client::connect(socket, "liveness") {
            liveness.answered();
            drop(client);
        }
        core.wait_stop(PROBE_EVERY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: u64 = 1_000_000_000;

    #[test]
    fn nothing_is_healthy_until_both_the_loop_and_the_socket_have_said_so() {
        let live = DaemonLiveness::new();
        assert!(!live.healthy());
        live.turned();
        assert!(!live.healthy(), "the socket has not answered");
        live.answered();
        assert!(live.healthy());
    }

    #[test]
    fn a_stale_pass_or_a_stale_answer_is_not_healthy() {
        let live = DaemonLiveness::starting(Instant::now());
        let now = 100 * SEC;
        live.turned.store(now - 9 * SEC, Ordering::SeqCst);
        live.answered.store(now - 4 * SEC, Ordering::SeqCst);
        assert!(live.healthy_at(now), "within both bounds");

        live.answered.store(now - 6 * SEC, Ordering::SeqCst);
        assert!(!live.healthy_at(now), "the socket last answered 6 s ago");

        live.answered.store(now - SEC, Ordering::SeqCst);
        live.turned.store(now - 11 * SEC, Ordering::SeqCst);
        assert!(
            !live.healthy_at(now),
            "the loop last finished a pass 11 s ago"
        );
    }

    #[test]
    fn the_bounds_are_inside_the_watchdog_timeout() {
        // The board's watchdog is set to 15 s by default. A healthy daemon is never stale for
        // longer than the socket bound, and a stale loop is caught before the timeout runs out.
        assert!(ANSWER_BOUND < Duration::from_secs(15));
        assert!(TURN_BOUND < Duration::from_secs(15));
        assert!(PROBE_EVERY < ANSWER_BOUND);
    }
}
