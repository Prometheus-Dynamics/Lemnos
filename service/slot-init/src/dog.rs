//! The hardware watchdog slot-init arms before `switch_root`: what it asks the device for, what the
//! device applies, how often it is fed, and the named refusals an image builder sees
//! (`contracts/slot-kernel.toml`, `[watchdog]`, which carries the same numbers).
//!
//! # The request and the bound
//!
//! slot-init asks for [`DOG_SECONDS`] and reads back the period the device applies. A device may apply
//! another period. The bound is [`MAX_APPLIED_S`]: a device applying more than that, or 0, is refused
//! with a named reason (see [`REASON_OUTSIDE`] and [`REASON_ZERO`]).
//!
//! Why the bound is twice the request. The DesignWare watchdog (`dw_wdt`, the rk35xx family) has only
//! power-of-two periods: from memory of the driver, period `i` is `2^(16+i) / clk` seconds for the
//! watchdog clock `clk` in hertz, `i` from 0 to 15. A request is rounded up to the first period at or
//! above it. The period below the first one that is at or above 15 s is below 15 s, and each period is
//! twice the one before, so the first one at or above 15 s is below 30 s, whatever `clk` is. Worked
//! out for two clocks: at 24 MHz it applies 22.4 s (`2^29 / 24e6`); at 100 MHz, 21.5 s (`2^31 / 100e6`).
//! The clock is NOT read here: it has to be read on the board (the watchdog's clock rate), and the
//! bound holds without it. The 16 s maximum of the sun60i family (`sunxi_wdt`) is inside the bound too.
//! [`the_bound_admits_every_power_of_two_period_above_the_request`] checks the derivation over a range of clocks.
//!
//! # Feeding
//!
//! A device that applies `s` seconds is fed at most every `s / 2`, and never less often than
//! [`FEED_EVERY`] (the feed points are the log and the waits of `sys`; see there). So a device that rounds
//! down (8 s) is fed every 4 s, and one that rounds up (22 s) every 5 s as before. A device that
//! reports 0 has no period to halve: it is fed every [`FEED_EVERY`].
//!
//! # Refusals: the device is kept and fed
//!
//! A refusal does not drop the open device. The device is armed whether slot-init likes its period or
//! not, so the descriptor is kept and fed for as long as boot runs, and the refusal is logged with its
//! reason. Feeding stops, as before, when boot returns (`main`): a stuck final loop must reset the board.
//! The only descriptor slot-init lets go of is the one the exec of `switch_root` closes, after a feed
//! right before the exec (see `sys::System::switch_root`).
//!
//! # Retries
//!
//! The node can appear late. An open that fails is retried with a backoff that starts at
//! [`BACKOFF_FIRST`], doubles, and is capped at [`BACKOFF_CAP`], until the waits add up to
//! [`APPEAR_BUDGET`]. Then the arm gives up with [`REASON_NO_NODE`]: slot-init arms nothing, and the device
//! (if it exists) is left to the kernel's own feeding, which runs until the open timeout
//! (`CONFIG_WATCHDOG_OPEN_TIMEOUT`, 30 s from the driver's probe), and then to lemnosd's bind.

use std::io;
use std::time::Duration;

/// The timeout slot-init asks for, in seconds.
pub const DOG_SECONDS: u32 = 15;
/// The longest period a device may apply for the request and still be armed (see the module
/// comment: the request doubled, which admits every power-of-two period above the request).
pub const MAX_APPLIED_S: u32 = 2 * DOG_SECONDS;
/// The longest a wait goes between two feeds of a device that applies a long period.
pub const FEED_EVERY: Duration = Duration::from_secs(5);
/// How long the opens may be retried before the arm gives up.
pub const APPEAR_BUDGET: Duration = Duration::from_secs(20);
/// The first wait between two opens; it doubles to [`BACKOFF_CAP`].
pub const BACKOFF_FIRST: Duration = Duration::from_millis(250);
pub const BACKOFF_CAP: Duration = Duration::from_secs(8);

/// The named reasons. The image builder and the log use these words.
pub const REASON_NO_NODE: &str = "watchdog-node-absent";
pub const REASON_OUTSIDE: &str = "watchdog-period-outside-bound";
pub const REASON_ZERO: &str = "watchdog-period-zero";
pub const REASON_UNREADABLE: &str = "watchdog-period-unreadable";

/// The watchdog device's requests and its feed write.
pub trait Device {
    /// Asks for `seconds` and returns the period the device applied.
    fn set_timeout(&mut self, seconds: u32) -> io::Result<u32>;
    /// The period the device applies now.
    fn timeout(&mut self) -> io::Result<u32>;
    /// One feed. Any byte but the magic close feeds the device.
    fn feed(&mut self) -> io::Result<()>;
}

/// A device slot-init armed. It is fed every `feed_every`.
pub struct Armed<D> {
    pub device: D,
    /// The period the device applies, or `None` when it cannot be read.
    pub applied_s: Option<u32>,
    pub feed_every: Duration,
    /// The reason the device is outside the rule, or `None` when it is within it. A refused device
    /// is still fed.
    pub refusal: Option<Refusal>,
    /// What the device said about the request when it was not simply taken (for the log), or `None`.
    pub note: Option<String>,
}

/// A named refusal: the reason (one of the `REASON_*` words) and the sentence for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub reason: &'static str,
    pub detail: String,
}

/// No device at all: the reason and the sentence for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Absent {
    pub reason: &'static str,
    pub detail: String,
}

/// How often a device that applies `applied_s` seconds is fed: half of it, and never more than
/// [`FEED_EVERY`] apart. A device that reports 0 (or none) is fed every [`FEED_EVERY`].
pub fn feed_every(applied_s: Option<u32>) -> Duration {
    match applied_s {
        Some(seconds) if seconds > 0 => {
            FEED_EVERY.min(Duration::from_millis(u64::from(seconds) * 500))
        }
        _ => FEED_EVERY,
    }
}

/// Opens the device (retrying as the module comment says, with `pause` for each wait), asks it for
/// [`DOG_SECONDS`], reads back the period it applies and judges that period against the bound.
pub fn arm<D: Device>(
    open: &mut dyn FnMut() -> io::Result<D>,
    pause: &mut dyn FnMut(Duration),
) -> Result<Armed<D>, Absent> {
    let mut waited = Duration::ZERO;
    let mut backoff = BACKOFF_FIRST;
    let mut device = loop {
        match open() {
            Ok(device) => break device,
            Err(error) => {
                if waited >= APPEAR_BUDGET {
                    return Err(Absent {
                        reason: REASON_NO_NODE,
                        detail: format!(
                            "the device did not appear in {} s of retries: {error}",
                            waited.as_secs()
                        ),
                    });
                }
                pause(backoff);
                waited += backoff;
                backoff = (backoff * 2).min(BACKOFF_CAP);
            }
        }
    };
    let (applied_s, note) = match device.set_timeout(DOG_SECONDS) {
        Ok(applied) => (Some(applied), None),
        Err(set) => match device.timeout() {
            Ok(applied) => (
                Some(applied),
                Some(format!(
                    "the device refused the {DOG_SECONDS} s request ({set}) and keeps {applied} s"
                )),
            ),
            Err(read) => (
                None,
                Some(format!(
                    "the device refused the {DOG_SECONDS} s request ({set}) and its period cannot be read ({read})"
                )),
            ),
        },
    };
    let refusal = match applied_s {
        None => Some(Refusal {
            reason: REASON_UNREADABLE,
            detail: "the period the device applies cannot be read".into(),
        }),
        Some(0) => Some(Refusal {
            reason: REASON_ZERO,
            detail: "the device reports a period of 0 s".into(),
        }),
        Some(seconds) if seconds > MAX_APPLIED_S => Some(Refusal {
            reason: REASON_OUTSIDE,
            detail: format!(
                "the device applies {seconds} s for the {DOG_SECONDS} s request, above the bound of {MAX_APPLIED_S} s"
            ),
        }),
        Some(_) => None,
    };
    Ok(Armed {
        device,
        applied_s,
        feed_every: feed_every(applied_s),
        refusal,
        note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device that applies `applied`, or (`applied` None) refuses the request and keeps `keeps`,
    /// or cannot be read at all (`readable` false). It counts its feeds.
    struct Fake {
        applied: Option<u32>,
        keeps: Option<u32>,
        readable: bool,
        feeds: u32,
    }

    impl Fake {
        fn applies(seconds: u32) -> Self {
            Self {
                applied: Some(seconds),
                keeps: None,
                readable: true,
                feeds: 0,
            }
        }
    }

    impl Device for Fake {
        fn set_timeout(&mut self, _seconds: u32) -> io::Result<u32> {
            self.applied
                .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "ENOTTY"))
        }
        fn timeout(&mut self) -> io::Result<u32> {
            match (self.readable, self.keeps.or(self.applied)) {
                (true, Some(seconds)) => Ok(seconds),
                _ => Err(io::Error::other("EIO")),
            }
        }
        fn feed(&mut self) -> io::Result<()> {
            self.feeds += 1;
            Ok(())
        }
    }

    /// Runs `open` through `arm` with pauses recorded, not slept.
    fn run<D: Device>(
        mut open: impl FnMut() -> io::Result<D>,
    ) -> (Result<Armed<D>, Absent>, Vec<Duration>) {
        let mut pauses = Vec::new();
        let result = arm(&mut open, &mut |d| pauses.push(d));
        (result, pauses)
    }

    /// The feeds the armed device gets over `seconds` of its feed schedule: the number of feeds, and
    /// the longest gap between two of them (the device is fed at `feed_every` apart, so it is what
    /// the schedule gives).
    fn schedule(armed: &mut Armed<Fake>, seconds: u64) -> (u32, Duration) {
        let every = armed.feed_every;
        let mut at = Duration::ZERO;
        let end = Duration::from_secs(seconds);
        let mut longest = Duration::ZERO;
        let mut last = Duration::ZERO;
        while at < end {
            armed.device.feed().unwrap();
            if at > Duration::ZERO {
                longest = longest.max(at - last);
            }
            last = at;
            at += every;
        }
        (armed.device.feeds, longest)
    }

    #[test]
    fn rounds_up_inside_the_bound_is_kept_and_fed_at_half_its_period() {
        // A DesignWare device at 24 MHz: 15 s is applied as 22 s, which is inside the bound.
        let (armed, pauses) = run(|| Ok(Fake::applies(22)));
        let mut armed = armed.unwrap();
        assert!(pauses.is_empty(), "the first open succeeds");
        assert_eq!(armed.applied_s, Some(22));
        assert_eq!(armed.refusal, None);
        assert!(armed.note.is_none(), "a plain request is not noted");
        // Fed every 5 s: under the 11 s half of 22 s, and as often as slot-init always was.
        assert_eq!(armed.feed_every, Duration::from_secs(5));
        let (feeds, longest) = schedule(&mut armed, 60);
        assert_eq!(feeds, 12, "60 s at 5 s is 12 feeds");
        assert!(longest <= Duration::from_secs(22) / 2);
    }

    #[test]
    fn rounds_up_outside_the_bound_is_a_named_refusal_and_the_device_is_kept_and_fed() {
        let (armed, _) = run(|| Ok(Fake::applies(60)));
        let mut armed = armed.unwrap();
        let refusal = armed.refusal.clone().expect("60 s is above the bound");
        assert_eq!(refusal.reason, REASON_OUTSIDE);
        assert!(refusal.detail.contains("60 s") && refusal.detail.contains("30 s"));
        assert_eq!(armed.applied_s, Some(60));
        // The device is kept: it is fed, at the same cadence (5 s, never slower than FEED_EVERY).
        assert_eq!(armed.feed_every, FEED_EVERY);
        let (feeds, _) = schedule(&mut armed, 30);
        assert_eq!(
            feeds, 6,
            "30 s at 5 s is 6 feeds: the refused device is fed"
        );
    }

    #[test]
    fn rounds_down_is_accepted_and_fed_faster_than_its_request() {
        // A device that applies 8 s for a 15 s request: fed every 4 s, half of it.
        let (armed, _) = run(|| Ok(Fake::applies(8)));
        let mut armed = armed.unwrap();
        assert_eq!(armed.refusal, None);
        assert_eq!(armed.applied_s, Some(8));
        assert_eq!(armed.feed_every, Duration::from_secs(4));
        let (feeds, longest) = schedule(&mut armed, 60);
        assert_eq!(feeds, 15, "60 s at 4 s is 15 feeds");
        assert!(longest <= Duration::from_secs(4));
    }

    #[test]
    fn a_device_reporting_zero_is_a_named_refusal_and_is_still_fed() {
        let (armed, _) = run(|| Ok(Fake::applies(0)));
        let mut armed = armed.unwrap();
        assert_eq!(armed.refusal.as_ref().map(|r| r.reason), Some(REASON_ZERO));
        assert_eq!(
            armed.feed_every, FEED_EVERY,
            "no period to halve: the longest feed"
        );
        let (feeds, _) = schedule(&mut armed, 20);
        assert_eq!(feeds, 4, "20 s at 5 s is 4 feeds");
    }

    #[test]
    fn a_refused_request_keeps_the_period_the_device_has_when_it_is_within_the_bound() {
        let (armed, _) = run(|| {
            Ok(Fake {
                applied: None,
                keeps: Some(15),
                readable: true,
                feeds: 0,
            })
        });
        let armed = armed.unwrap();
        assert_eq!(armed.applied_s, Some(15));
        assert_eq!(armed.refusal, None, "the period it keeps is in range");
        assert!(armed.note.unwrap().contains("refused the 15 s request"));
        assert_eq!(armed.feed_every, FEED_EVERY);
    }

    #[test]
    fn a_refused_request_whose_period_cannot_be_read_is_a_named_refusal_and_is_kept() {
        let (armed, _) = run(|| {
            Ok(Fake {
                applied: None,
                keeps: None,
                readable: false,
                feeds: 0,
            })
        });
        let armed = armed.unwrap();
        assert_eq!(armed.applied_s, None);
        assert_eq!(
            armed.refusal.as_ref().map(|r| r.reason),
            Some(REASON_UNREADABLE)
        );
        assert!(armed.note.unwrap().contains("cannot be read"));
        assert_eq!(armed.feed_every, FEED_EVERY);
    }

    #[test]
    fn a_node_that_appears_late_is_opened_after_backoff_waits() {
        let mut opens = 0;
        let (armed, pauses) = run(|| {
            opens += 1;
            if opens < 4 {
                Err(io::Error::new(io::ErrorKind::NotFound, "ENOENT"))
            } else {
                Ok(Fake::applies(15))
            }
        });
        let armed = armed.unwrap();
        assert_eq!(armed.applied_s, Some(15));
        assert_eq!(
            pauses,
            vec![
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_secs(1)
            ]
        );
        assert_eq!(opens, 4, "three failed opens, then the one that found it");
    }

    #[test]
    fn a_node_that_never_appears_is_a_named_refusal_after_the_budget() {
        let mut opens = 0;
        let (result, pauses) = run::<Fake>(|| {
            opens += 1;
            Err(io::Error::new(io::ErrorKind::NotFound, "ENOENT"))
        });
        let absent = result.err().expect("no node, no arm");
        assert_eq!(absent.reason, REASON_NO_NODE);
        assert!(absent.detail.contains("did not appear"));
        // The waits are capped at 8 s and add up to the budget: 0.25 + 0.5 + 1 + 2 + 4 + 8 + 8 = 23.75 s.
        assert_eq!(pauses.iter().copied().max(), Some(BACKOFF_CAP));
        let total: Duration = pauses.iter().sum();
        assert!(total >= APPEAR_BUDGET && total < APPEAR_BUDGET + BACKOFF_CAP);
        assert_eq!(opens as usize, pauses.len() + 1);
    }

    #[test]
    fn the_feed_interval_is_half_the_period_and_never_longer_than_the_feed_every() {
        assert_eq!(feed_every(Some(15)), Duration::from_secs(5));
        assert_eq!(feed_every(Some(8)), Duration::from_secs(4));
        assert_eq!(feed_every(Some(1)), Duration::from_millis(500));
        assert_eq!(feed_every(Some(22)), Duration::from_secs(5));
        assert_eq!(feed_every(Some(0)), FEED_EVERY);
        assert_eq!(feed_every(None), FEED_EVERY);
    }

    #[test]
    fn the_bound_admits_every_power_of_two_period_above_the_request() {
        // The DesignWare periods are 2^(16+i)/clk. For each clock from 1 MHz to 143 MHz (above that
        // the largest period is below the request), the first period at or above the request is
        // below the bound, and so is the 16 s maximum of the sun60i family.
        let request = f64::from(DOG_SECONDS);
        let bound = f64::from(MAX_APPLIED_S);
        for mhz in 1..=143u64 {
            let clk = mhz as f64 * 1e6;
            let first = (0..16)
                .map(|i| (2f64.powi(16 + i)) / clk)
                .find(|period| *period >= request)
                .expect("a period at or above the request");
            assert!(
                first < bound,
                "{mhz} MHz applies {first} s, above {bound} s"
            );
        }
        assert!(2f64.powi(29) / 24e6 >= request && 2f64.powi(29) / 24e6 < bound);
        assert!(2f64.powi(31) / 100e6 >= request && 2f64.powi(31) / 100e6 < bound);
        assert!(16.0 < bound);
    }
}
