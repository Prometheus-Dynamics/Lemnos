//! The session loop: enumerate the device, serve the bulk pair until an END, and enumerate again after a bus reset.
//!
//! **One clock.** Every limit in the session is one deadline, the **idle deadline**: [`IDLE_LIMIT_US`] after the last
//! watchdog feed. The first feed is the instant the watchdog is armed, which the chip takes before its own bring-up and
//! passes in as [`Setup::armed_at_us`]. The deadline moves only when a round feeds the watchdog, and the feed's instant
//! is the one read at the feed (after the verified work, before the status is sent). A reply does not move it, a refused
//! request does not, and a bus reset does not: a host that identifies every 29 seconds reaches the idle deadline at the
//! same time as a host that never asks. Enumeration, before the first request and after every bus reset, shares that
//! deadline, so a reset grants no fresh limit.
//!
//! The watchdog's period covers the worst gap between two feeds (see `timing`), and the idle deadline is earlier than
//! that, so the distinct idle outcome comes before any reset of the watchdog.
//!
//! After a stop the chip's stated exit follows: its report, then the park that never feeds the watchdog.

use crate::guard::Watchdog;
use crate::link::Storage;
use crate::region::Region;
use crate::serve::{Context, ServeError};
use crate::usb::{BulkLink, Controller, EnumError, Ep0, enumerate};
use crate::wait::Clock;
use lemnos_flash_wire::frame::ResetKind;
use lemnos_flash_wire::timing::IDLE_LIMIT_US;

/// Why the session ended without an END.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStop {
    /// The host did not configure the device before the idle deadline, it was disconnected, or it configured a link slower
    /// than high speed.
    Enumerate(EnumError),
    /// The serve loop ended without an END: the idle deadline passed with no request, or the link failed for good.
    Serve(ServeError),
}

/// What the session needs besides the controller and the storage.
pub struct Setup<'a> {
    /// The card's regions, as the stage polices them.
    pub regions: &'a [Region],
    /// The identity record.
    pub identity: &'a [u8],
    /// The chunk buffer and the scratch buffer (each at least `serve::BUFFER_LEN` bytes).
    pub buf: &'a mut [u8],
    pub scratch: &'a mut [u8],
    /// The instant the watchdog was armed, on the session's clock. The first idle deadline counts from it.
    pub armed_at_us: u64,
}

/// Counts the feeds a step makes, and keeps the instant of the last one, so the session's idle deadline moves exactly
/// when the watchdog is fed.
struct Counted<'a, W: Watchdog> {
    inner: &'a mut W,
    fed_at: Option<u64>,
}

impl<W: Watchdog> Watchdog for Counted<'_, W> {
    fn feed(&mut self) {
        self.inner.feed();
    }

    fn feed_at(&mut self, now_us: u64) {
        self.fed_at = Some(now_us);
        self.inner.feed();
    }
}

/// Runs the session to its END. Returns the reset kind the END asked for, or why it stopped.
///
/// The controller must be at reset when this is called (the EP0 state is `Ep0::new()`).
pub fn run<C: Controller, K: Clock, S: Storage, W: Watchdog>(
    ctrl: &mut C,
    clock: &mut K,
    ep0: &mut Ep0,
    storage: &mut S,
    wdt: &mut W,
    setup: Setup<'_>,
) -> Result<ResetKind, SessionStop> {
    let mut last_feed = setup.armed_at_us;
    let ctx = Context {
        regions: setup.regions,
        identity: setup.identity,
    };
    loop {
        let deadline = last_feed.saturating_add(IDLE_LIMIT_US);
        enumerate(&mut *ctrl, &mut *clock, &mut *ep0, deadline).map_err(SessionStop::Enumerate)?;
        loop {
            // The idle deadline moves with each feed: it is recomputed on every step, from the last feed.
            let deadline = last_feed.saturating_add(IDLE_LIMIT_US);
            let budget = deadline.saturating_sub(clock.now_us());
            let mut counted = Counted {
                inner: &mut *wdt,
                fed_at: None,
            };
            let served = {
                let mut link = BulkLink {
                    ctrl: &mut *ctrl,
                    clock: &mut *clock,
                    ep0: &mut *ep0,
                };
                crate::serve::step(
                    &mut link,
                    storage,
                    &mut counted,
                    &ctx,
                    budget,
                    setup.buf,
                    setup.scratch,
                )
            };
            if let Some(fed) = counted.fed_at {
                last_feed = fed;
            }
            match served {
                Ok(crate::serve::Progress::Ended(kind)) => return Ok(kind),
                Ok(crate::serve::Progress::Idle) => {
                    return Err(SessionStop::Serve(ServeError::Idle));
                }
                Ok(_) => {}
                Err(ServeError::Link(crate::link::IoError::Reset)) => break,
                Err(e) => return Err(SessionStop::Serve(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! The session from reset through the simulated controller. The clock is exact: each controller pass takes one
    //! millisecond, so every instant asserted below is the one the code reaches, not a tolerance.
    use super::*;
    use crate::region::Class;
    use crate::serve::BUFFER_LEN;
    use crate::sim::Card;
    use crate::sim::model::{Action, Model, SimClock};
    use crate::usb::Event;
    use lemnos_flash_wire::frame::{MAX_CHUNK, Request, ResetKind, SECTOR, payload_crc};
    use lemnos_flash_wire::identity::identity_record;
    use lemnos_flash_wire::timing::REFERENCE_WATCHDOG_PERIOD_US;

    const MS: u64 = 1_000;
    const SECOND: u64 = 1_000_000;

    #[derive(Default)]
    struct Dog(u32);

    impl Watchdog for Dog {
        fn feed(&mut self) {
            self.0 += 1;
        }
    }

    const REGIONS: [Region; 1] = [Region {
        first: 64,
        end: 100,
        class: Class::BootArea,
    }];

    /// What one session ended with: its result, the watchdog's feeds, the clock when it stopped, and the card and the
    /// host's record.
    struct Run {
        result: Result<ResetKind, SessionStop>,
        feeds: u32,
        at: u64,
        card: Card,
        host: Model,
    }

    /// One session from reset: the host's script on a simulated clock that starts at `start_us`, the watchdog armed at
    /// `armed_at_us`, and the card.
    fn run_session(script: Model, start_us: u64, armed_at_us: u64, card: Card) -> Run {
        let clock = SimClock::at(start_us);
        let mut host = script.on(&clock);
        let mut time = clock.clone();
        let mut ep0 = Ep0::new();
        let mut card = card;
        let mut dog = Dog::default();
        let identity = identity_record(42, &REGIONS);
        let mut buf = vec![0u8; BUFFER_LEN];
        let mut scratch = vec![0u8; MAX_CHUNK];
        let result = run(
            &mut host,
            &mut time,
            &mut ep0,
            &mut card,
            &mut dog,
            Setup {
                regions: &REGIONS,
                identity: &identity,
                buf: &mut buf,
                scratch: &mut scratch,
                armed_at_us,
            },
        );
        Run {
            result,
            feeds: dog.0,
            at: clock.now(),
            card,
            host,
        }
    }

    /// A session armed at the start, on a card that keeps up.
    fn session(script: Model) -> Run {
        run_session(script, 0, 0, Card::new(1024))
    }

    fn identify() -> Vec<u8> {
        Request::identify().encode().to_vec()
    }

    fn end() -> Vec<u8> {
        Request::end(ResetKind::Normal).encode().to_vec()
    }

    /// A write request for `count` sectors at `lba`, and its chunk (the payload, then its CRC).
    fn write_of(lba: u32, count: u16, fill: u8) -> (Vec<u8>, Vec<u8>) {
        let payload = vec![fill; usize::from(count) * SECTOR];
        let mut chunk = payload.clone();
        chunk.extend_from_slice(&payload_crc(&payload));
        (Request::write(lba, count, false).encode().to_vec(), chunk)
    }

    fn write_one(lba: u32, fill: u8) -> (Vec<u8>, Vec<u8>) {
        write_of(lba, 1, fill)
    }

    fn configurations(host: &Model) -> usize {
        host.log
            .iter()
            .filter(|a| **a == Action::Configured(true))
            .count()
    }

    // The enumeration from reset is six control events, one controller pass each: the first request is at 6 ms. Every
    // deadline below is 30 s after the arm (0), or after the last feed, so the instants are exact sums of those passes.

    #[test]
    fn a_cold_start_from_reset_enumerates_then_answers_the_first_request() {
        let mut script = Model::new();
        script.enumeration(7).frame(identify()).frame(end());
        let r = session(script);
        assert_eq!(r.result, Ok(ResetKind::Normal));
        assert_eq!(
            r.host.bulk_in.len(),
            2,
            "the identity, then the END's status"
        );
        assert_eq!(r.host.bulk_in[0], identity_record(42, &REGIONS).to_vec());
        assert_eq!(r.host.bulk_in[1], vec![0u8]);
        assert!(
            r.host.log.contains(&Action::Address(7)),
            "the host set the address first"
        );
        assert!(
            r.host.log.contains(&Action::Configured(true)),
            "and configured the device"
        );
        assert_eq!(
            r.feeds, 0,
            "enumeration and an identify feed nothing: no round was verified"
        );
    }

    #[test]
    fn a_bus_reset_between_requests_re_enumerates_and_the_session_serves_again() {
        let mut script = Model::new();
        script
            .enumeration(7)
            .frame(identify())
            .event(Event::BusReset)
            .enumeration(9)
            .frame(identify())
            .frame(end());
        let r = session(script);
        assert_eq!(r.result, Ok(ResetKind::Normal));
        assert_eq!(
            r.host.bulk_in.len(),
            3,
            "identity, identity after the reset, END"
        );
        assert_eq!(
            configurations(&r.host),
            2,
            "the host configured the device twice"
        );
        assert!(
            r.host.log.contains(&Action::Address(9)),
            "the second enumeration set its new address"
        );
    }

    #[test]
    fn a_reset_with_a_chunk_in_flight_loses_the_chunk_and_writes_nothing() {
        let (header, _chunk) = write_one(500, 0x3C);
        let mut script = Model::new();
        script
            .enumeration(7)
            .frame(header)
            .event(Event::BusReset)
            .enumeration(9)
            .frame(identify())
            .frame(end());
        let r = session(script);
        assert_eq!(
            r.result,
            Ok(ResetKind::Normal),
            "the session goes on after the reset"
        );
        assert_eq!(r.feeds, 0, "no round was verified");
        assert_eq!(
            r.card.sector(500),
            [0u8; SECTOR],
            "the write that was in flight is not stored"
        );
    }

    #[test]
    fn a_host_that_never_sets_a_configuration_ends_the_session_at_the_enumeration_limit() {
        let mut script = Model::new();
        script.event(Model::setup(0x80, 6, 0x0100, 0, 18));
        let r = session(script);
        assert_eq!(
            r.result,
            Err(SessionStop::Enumerate(EnumError::NotConfigured))
        );
        assert_eq!(
            r.feeds, 0,
            "the watchdog is not fed: the stage's exit is the reset that follows the report"
        );
        assert_eq!(r.at, 30 * SECOND, "stopped at the idle deadline, exactly");
        assert!(r.host.bulk_in.is_empty());
    }

    #[test]
    fn an_enumeration_that_starts_after_the_deadline_is_not_configured_and_feeds_nothing() {
        // The host waits 31 s before it starts enumerating: the deadline (30 s after the arm) passes first.
        let mut script = Model::new();
        script.gap(31_000).enumeration(7).frame(identify());
        let r = session(script);
        assert_eq!(
            r.result,
            Err(SessionStop::Enumerate(EnumError::NotConfigured))
        );
        assert_eq!(r.feeds, 0);
        assert_eq!(
            r.at,
            30 * SECOND,
            "the deadline is the arm plus 30 s, whenever the host starts"
        );
        assert!(
            r.host.bulk_in.is_empty(),
            "nothing was served after the deadline"
        );
    }

    #[test]
    fn an_unsupported_control_request_is_stalled_and_the_session_continues() {
        let mut script = Model::new();
        script
            .event(Model::setup(0xC0, 1, 0, 0, 16))
            .enumeration(7)
            .frame(identify())
            .frame(end());
        let r = session(script);
        assert_eq!(r.result, Ok(ResetKind::Normal));
        assert_eq!(
            r.host.log[0],
            Action::Stall,
            "the vendor request is stalled first"
        );
        assert_eq!(
            r.host.bulk_in.len(),
            2,
            "and the requests after it are served"
        );
    }

    #[test]
    fn an_idle_host_ends_the_session_at_the_idle_limit_with_its_own_outcome() {
        let mut script = Model::new();
        script.enumeration(7);
        let r = session(script);
        assert_eq!(
            r.result,
            Err(SessionStop::Serve(ServeError::Idle)),
            "not a link timeout"
        );
        assert_eq!(r.feeds, 0);
        assert_eq!(r.at, 30 * SECOND, "idle at the deadline, exactly");
    }

    #[test]
    fn a_host_that_identifies_every_29_seconds_is_idle_at_30_seconds_and_the_watchdog_is_not_reached()
     {
        // Identify at 6 ms, at 29.006 s, and then the third would be at 58 s. Replies do not move the deadline (30 s after
        // the arm), so the session ends at exactly 30 s with its own outcome, before the watchdog's period.
        let mut script = Model::new();
        script
            .enumeration(7)
            .frame(identify())
            .gap(29_000)
            .frame(identify())
            .gap(29_000)
            .frame(identify());
        let r = session(script);
        assert_eq!(r.result, Err(SessionStop::Serve(ServeError::Idle)));
        assert_eq!(r.feeds, 0);
        assert_eq!(
            r.host.bulk_in.len(),
            2,
            "the two identifies inside the window were answered; the third was not"
        );
        assert_eq!(r.at, 30 * SECOND, "idle at the deadline, exactly");
        assert!(
            r.at < REFERENCE_WATCHDOG_PERIOD_US,
            "before the watchdog's period"
        );
    }

    #[test]
    fn refusals_leave_the_deadline_unmoved_and_the_session_ends_idle_at_30_seconds() {
        // Two refused requests (a bad CRC), at 6 ms and at 29.006 s: each is answered with status 1, and neither moves the
        // deadline.
        let mut bad = Request::read(1, 1).encode();
        bad[7] ^= 0x40;
        let mut script = Model::new();
        script
            .enumeration(7)
            .frame(bad.to_vec())
            .gap(29_000)
            .frame(bad.to_vec());
        let r = session(script);
        assert_eq!(r.result, Err(SessionStop::Serve(ServeError::Idle)));
        assert_eq!(r.feeds, 0);
        assert_eq!(
            r.host.bulk_in,
            vec![vec![1u8], vec![1u8]],
            "two refusals, each with status 1"
        );
        assert_eq!(
            r.at,
            30 * SECOND,
            "idle at the deadline from the arm, exactly"
        );
    }

    #[test]
    fn a_bus_reset_at_25_seconds_leaves_5_seconds_for_the_next_request() {
        // A reset at 25.006 s; the host configures again at once and identifies at 28.012 s, inside the 5 s left; then
        // nothing. The re-enumeration did not extend the deadline: the session ends idle at exactly 30 s.
        let mut script = Model::new();
        script
            .enumeration(7)
            .gap(25_000)
            .event(Event::BusReset)
            .enumeration(9)
            .gap(3_000)
            .frame(identify());
        let r = session(script);
        assert_eq!(r.result, Err(SessionStop::Serve(ServeError::Idle)));
        assert_eq!(r.feeds, 0);
        assert_eq!(
            r.host.bulk_in.len(),
            1,
            "the identify inside the 5 s was answered"
        );
        assert_eq!(configurations(&r.host), 2);
        assert_eq!(r.at, 30 * SECOND, "idle at 30 s, not 50 s");
    }

    #[test]
    fn a_bus_reset_does_not_grant_a_fresh_enumeration_limit() {
        // A reset at 20.006 s; the host never configures after it. Enumeration ends at the deadline from the arm: 30 s.
        let mut script = Model::new();
        script
            .enumeration(7)
            .gap(20_000)
            .event(Event::BusReset)
            .event(Model::setup(0x80, 6, 0x0100, 0, 18));
        let r = session(script);
        assert_eq!(
            r.result,
            Err(SessionStop::Enumerate(EnumError::NotConfigured))
        );
        assert_eq!(r.feeds, 0);
        assert_eq!(
            r.at,
            30 * SECOND,
            "the limit is the one from the arm, exactly"
        );
    }

    #[test]
    fn a_write_round_moves_the_idle_deadline_to_its_feed_plus_30_seconds() {
        // A write at 25.006 s is verified and fed at that instant; the deadline moves to 55.006 s. An identify at 54.006 s is
        // inside the new window. Without the feed the session would have ended at 30 s.
        let (header, chunk) = write_one(500, 0x3C);
        let mut script = Model::new();
        script
            .enumeration(7)
            .gap(25_000)
            .frame(header)
            .frame(chunk)
            .gap(29_000)
            .frame(identify());
        let r = session(script);
        assert_eq!(r.result, Err(SessionStop::Serve(ServeError::Idle)));
        assert_eq!(r.feeds, 1, "the write was verified and fed once");
        assert_eq!(r.card.sector(500), [0x3C; SECTOR], "and stored");
        assert_eq!(
            r.host.bulk_in.len(),
            2,
            "the write's status, then the identify after the feed"
        );
        assert_eq!(r.at, 55_006 * MS, "the feed at 25.006 s plus 30 s, exactly");
    }

    #[test]
    fn the_first_deadline_counts_from_the_arm_not_from_the_start_of_the_session() {
        // The clock already reads 10 s when the session starts; the watchdog was armed at 0. The idle deadline is 30 s after
        // the arm, so the session ends at 30 s, not at 40 s.
        let mut script = Model::new();
        script.enumeration(7);
        let r = run_session(script, 10 * SECOND, 0, Card::new(1024));
        assert_eq!(r.result, Err(SessionStop::Serve(ServeError::Idle)));
        assert_eq!(r.feeds, 0);
        assert_eq!(r.at, 30 * SECOND, "idle at 30 s from the arm, exactly");
    }

    #[test]
    fn a_write_that_the_card_cannot_finish_within_its_budget_is_status_two_and_the_session_goes_on()
    {
        // A slow card: 4 s a sector, against a 3.5 s storage budget. The write at 26.006 s cannot write its one sector, so its
        // status is storage (2); nothing is fed; the identify after it is still answered; the session ends idle at 30 s.
        let (header, chunk) = write_one(500, 0x3C);
        let mut script = Model::new();
        script
            .enumeration(7)
            .gap(26_000)
            .frame(header)
            .frame(chunk)
            .frame(identify());
        let r = run_session(script, 0, 0, Card::new(1024).slow(4 * SECOND));
        assert_eq!(r.result, Err(SessionStop::Serve(ServeError::Idle)));
        assert_eq!(r.feeds, 0, "a failed write is not fed");
        assert_eq!(
            r.host.bulk_in[0],
            vec![2u8],
            "the write's status is storage (2)"
        );
        assert_eq!(
            r.card.sector(500),
            [0u8; SECTOR],
            "and nothing was stored: the one sector did not fit"
        );
        assert_eq!(
            r.at,
            30 * SECOND,
            "idle at 30 s, well before the watchdog's period"
        );
    }

    #[test]
    fn a_write_cut_off_by_its_deadline_is_status_two_with_part_of_the_range_written_and_nothing_outside_it()
     {
        // Four sectors at 1 s a sector against a 3.5 s budget: sectors 500 to 502 fit and are written; sector 503 does not.
        // The status is storage (2), the write is not fed, and nothing outside 500..504 is written.
        let (header, chunk) = write_of(500, 4, 0x3C);
        let mut script = Model::new();
        script
            .enumeration(7)
            .frame(header)
            .frame(chunk)
            .frame(identify());
        let r = run_session(script, 0, 0, Card::new(1024).slow(SECOND));
        assert_eq!(
            r.host.bulk_in[0],
            vec![2u8],
            "storage status: the range may be partly written"
        );
        assert_eq!(r.feeds, 0);
        for lba in 500..=502 {
            assert_eq!(
                r.card.sector(lba),
                [0x3C; SECTOR],
                "sector {lba} fits the budget and is written"
            );
        }
        assert_eq!(
            r.card.sector(503),
            [0u8; SECTOR],
            "sector 503 does not fit and is not written"
        );
        assert_eq!(
            r.card.sector(499),
            [0u8; SECTOR],
            "nothing before the range"
        );
        assert_eq!(r.card.sector(504), [0u8; SECTOR], "nothing after the range");
        assert_eq!(
            r.host.bulk_in.len(),
            2,
            "the session went on: the identify after the write was answered"
        );
    }

    #[test]
    fn a_control_request_near_the_deadline_does_not_drop_the_frame_that_follows_it() {
        // A control request is serviced at 29.999 s, one pass before the deadline; the identify frame is behind it. The
        // frame is taken on the pass after the control request, even though that pass is at the deadline: a frame is
        // never lost to a timeout because a control request came first.
        let mut script = Model::new();
        script
            .enumeration(7)
            .gap(29_993)
            .event(Model::setup(0x80, 0, 0, 0, 2))
            .frame(identify());
        let r = session(script);
        let last_data = r.host.log.iter().rev().find_map(|a| match a {
            Action::Data(d) => Some(d.clone()),
            _ => None,
        });
        assert_eq!(
            last_data,
            Some(vec![0u8, 0u8]),
            "the GET_STATUS was answered, last, with two zero bytes"
        );
        assert_eq!(
            r.host.bulk_in.len(),
            1,
            "the identify behind it was served, not dropped"
        );
        assert_eq!(
            r.result,
            Err(SessionStop::Serve(ServeError::Idle)),
            "then the session ends idle"
        );
        assert_eq!(r.at, 30 * SECOND);
    }

    #[test]
    fn a_request_armed_one_second_before_the_deadline_is_not_accepted_when_the_command_takes_one_second()
     {
        // The host's identify is armed at 29.006 s: 994 ms before the 30 s deadline. The controller's command for the arm
        // takes 1 s, so it runs past the deadline. The arm does not complete, the identify is not answered, and the session
        // ends idle at exactly 30 s.
        let mut script = Model::new().with_command_us(SECOND);
        script.enumeration(7).gap(29_000).frame(identify());
        let r = session(script);
        assert_eq!(r.result, Err(SessionStop::Serve(ServeError::Idle)));
        assert!(
            r.host.bulk_in.is_empty(),
            "the request armed past the deadline is not answered"
        );
        assert_eq!(r.at, 30 * SECOND, "the phase ends at the deadline, exactly");
    }

    #[test]
    fn a_request_armed_one_second_before_the_deadline_is_accepted_when_the_command_takes_half_a_second()
     {
        // The same arm with a 500 ms command completes inside the 994 ms left: the identify is answered.
        let mut script = Model::new().with_command_us(SECOND / 2);
        script.enumeration(7).gap(29_000).frame(identify());
        let r = session(script);
        assert_eq!(
            r.host.bulk_in.len(),
            1,
            "the identify inside the window is answered"
        );
        assert_eq!(r.result, Err(SessionStop::Serve(ServeError::Idle)));
        assert_eq!(r.at, 30 * SECOND);
    }

    #[test]
    fn the_configuration_value_the_host_sets_is_the_one_the_device_names() {
        // The value SET_CONFIGURATION accepts is the one the configuration descriptor names: a host that reads the
        // descriptor and sets that value configures the device.
        use crate::usb::descriptors::CONFIGURATION;
        use lemnos_flash_wire::usb::CONFIGURATION_VALUE;
        assert_eq!(CONFIGURATION_VALUE, 1);
        assert_eq!(
            CONFIGURATION[5], CONFIGURATION_VALUE,
            "bConfigurationValue in the descriptor"
        );
    }
}
