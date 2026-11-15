//! The waiter behind `lemnos-good-boot`: the good-boot deadline (M36).
//!
//! It runs as a systemd unit of its own, started at boot with no dependency on lemnosd being
//! healthy, so it still decides when lemnosd is wedged or never starts. It polls two facts once a
//! second: whether lemnosd has reached ready, and the booted marker. A boot is good when both hold
//! ([`lemnos_slot_store::goodboot::good`]). Device health is not asked. If the deadline passes
//! first, [`wait`] reports [`Outcome::Late`] and the unit fails, and systemd restarts the board
//! (`FailureAction=reboot-force` in `packaging/systemd/lemnos-good-boot.service`).

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use lemnos_slot_store::goodboot;
use lemnos_slot_store::store::{Layout, read_line};

/// The systemd tool that asks whether lemnosd is active. `is-active` succeeds only in state
/// `active`: a `Type=notify` unit is active once it has sent `READY=1`, and is `activating`
/// through a restart.
pub const SYSTEMCTL: &str = "/usr/bin/systemctl";
/// How often the two facts are looked at.
pub const POLL: Duration = Duration::from_secs(1);

/// The two facts a good boot needs, and nothing else.
pub trait Probe {
    /// Whether lemnosd has reached ready.
    fn lemnosd_ready(&mut self) -> bool;
    /// The text of the booted marker, `""` when it is missing.
    fn booted_marker(&mut self) -> String;
}

/// Time since the wait began.
pub trait Clock {
    /// The time elapsed since the wait began.
    fn elapsed(&self) -> Duration;
    /// Waits for `for_`.
    fn sleep(&mut self, for_: Duration);
}

/// How the wait ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A good boot, on `slot`, `after` this long.
    Good { slot: String, after: Duration },
    /// The deadline passed without a good boot. `ready` and `marker` are the last facts seen.
    Late { ready: bool, marker: String },
}

/// Waits until the boot is good or `deadline` has passed. The facts are looked at first, so a
/// boot that is already good at the start is good at 0 s.
pub fn wait(probe: &mut dyn Probe, clock: &mut dyn Clock, deadline: Duration) -> Outcome {
    loop {
        let ready = probe.lemnosd_ready();
        let marker = probe.booted_marker();
        if let Some(slot) = goodboot::good(ready, &marker) {
            return Outcome::Good {
                slot,
                after: clock.elapsed(),
            };
        }
        let now = clock.elapsed();
        if now >= deadline {
            return Outcome::Late { ready, marker };
        }
        clock.sleep(POLL.min(deadline - now));
    }
}

/// The board's own probe: lemnosd's unit state from systemd, and the marker on the data partition.
pub struct System {
    /// The data partition's mount point (`/data` on a slot board).
    pub data: PathBuf,
}

impl Probe for System {
    fn lemnosd_ready(&mut self) -> bool {
        Command::new(SYSTEMCTL)
            .args(["is-active", "--quiet", "lemnosd.service"])
            .status()
            .is_ok_and(|status| status.success())
    }

    fn booted_marker(&mut self) -> String {
        read_line(&Layout::new(&self.data, "/boot").marker("booted"))
    }
}

/// The real clock: the monotonic time since it was started.
pub struct Monotonic {
    started: Instant,
}

impl Default for Monotonic {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl Clock for Monotonic {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn sleep(&mut self, for_: Duration) {
        std::thread::sleep(for_);
    }
}

#[cfg(test)]
mod tests;
