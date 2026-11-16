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
    use super::*;
    use crate::region::Class;
    use crate::serve::BUFFER_LEN;
    use crate::sim::Card;
    use crate::sim::model::{Action, Model};
    use crate::usb::Event;
    use crate::wait::IterClock;
    use lemnos_flash_wire::frame::{MAX_CHUNK, Request, ResetKind, SECTOR, payload_crc};
    use lemnos_flash_wire::identity::identity_record;
    use lemnos_flash_wire::timing::{FEED_TO_FEED_US, REFERENCE_WATCHDOG_PERIOD_US};

    const SECOND: u64 = 1_000_000;

    /// A clock that moves a millisecond per read, starting at `start_us`: a thirty-second limit is thirty thousand reads.
    struct Quick(u64);

    impl Clock for Quick {
        fn now_us(&mut self) -> u64 {
            self.0 += 1_000;
            self.0
        }
    }

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

    /// One session from reset, with the card and the arm instant given. Returns the result, the feeds, the clock's
    /// reading when the session stopped, and the card.
    fn session_on(
        ctrl: &mut Model,
        clock_start_us: u64,
        armed_at_us: u64,
        card: Card,
    ) -> (Result<ResetKind, SessionStop>, u32, u64, Card) {
        let mut clock = Quick(clock_start_us);
        let mut ep0 = Ep0::new();
        let mut card = card;
        let mut dog = Dog::default();
        let identity = identity_record(42, &REGIONS);
        let mut buf = vec![0u8; BUFFER_LEN];
        let mut scratch = vec![0u8; MAX_CHUNK];
        let result = run(
            ctrl,
            &mut clock,
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
        (result, dog.0, clock.0, card)
    }

    /// The common case: the clock and the arm both at 0, a card that keeps up.
    fn session(
        ctrl: &mut Model,
        clock: &mut impl Clock,
    ) -> (Result<ResetKind, SessionStop>, u32, Card) {
        let _ = clock;
        let (result, feeds, _, card) = session_on(ctrl, 0, 0, Card::new(1024));
        (result, feeds, card)
    }

    fn identify() -> Vec<u8> {
        Request::identify().encode().to_vec()
    }

    fn end() -> Vec<u8> {
        Request::end(ResetKind::Normal).encode().to_vec()
    }

    /// A write request for one sector at `lba`, and its chunk (the payload, then its CRC).
    fn write_one(lba: u32, fill: u8) -> (Vec<u8>, Vec<u8>) {
        let payload = vec![fill; SECTOR];
        let mut chunk = payload.clone();
        chunk.extend_from_slice(&payload_crc(&payload));
        (Request::write(lba, 1, false).encode().to_vec(), chunk)
    }

    fn configurations(ctrl: &Model) -> usize {
        ctrl.log
            .iter()
            .filter(|a| **a == Action::Configured(true))
            .count()
    }

    fn at_about(clock_us: u64, expected_s: u64) -> bool {
        let want = expected_s * SECOND;
        clock_us + 2 * SECOND >= want && clock_us <= want + 2 * SECOND
    }

    #[test]
    fn a_cold_start_from_reset_enumerates_then_answers_the_first_request() {
        let mut ctrl = Model::new();
        ctrl.enumeration(7).frame(identify()).frame(end());
        let (result, feeds, _) = session(&mut ctrl, &mut IterClock::new());
        assert_eq!(result, Ok(ResetKind::Normal));
        assert_eq!(ctrl.bulk_in.len(), 2, "the identity, then the END's status");
        assert_eq!(ctrl.bulk_in[0], identity_record(42, &REGIONS).to_vec());
        assert_eq!(ctrl.bulk_in[1], vec![0u8]);
        assert!(
            ctrl.log.contains(&Action::Address(7)),
            "the host set the address first"
        );
        assert!(
            ctrl.log.contains(&Action::Configured(true)),
            "and configured the device"
        );
        assert_eq!(
            feeds, 0,
            "enumeration and an identify feed nothing: no round was verified"
        );
    }

    #[test]
    fn a_bus_reset_between_requests_re_enumerates_and_the_session_serves_again() {
        let mut ctrl = Model::new();
        ctrl.enumeration(7)
            .frame(identify())
            .event(Event::BusReset)
            .enumeration(9)
            .frame(identify())
            .frame(end());
        let (result, _, _) = session(&mut ctrl, &mut IterClock::new());
        assert_eq!(result, Ok(ResetKind::Normal));
        assert_eq!(
            ctrl.bulk_in.len(),
            3,
            "identity, identity after the reset, END"
        );
        assert_eq!(
            configurations(&ctrl),
            2,
            "the host configured the device twice"
        );
        assert!(
            ctrl.log.contains(&Action::Address(9)),
            "the second enumeration set its new address"
        );
    }

    #[test]
    fn a_reset_with_a_chunk_in_flight_loses_the_chunk_and_writes_nothing() {
        let mut ctrl = Model::new();
        let (header, _chunk) = write_one(500, 0x3C);
        ctrl.enumeration(7)
            .frame(header)
            .event(Event::BusReset)
            .enumeration(9)
            .frame(identify())
            .frame(end());
        let (result, feeds, card) = session(&mut ctrl, &mut IterClock::new());
        assert_eq!(
            result,
            Ok(ResetKind::Normal),
            "the session goes on after the reset"
        );
        assert_eq!(feeds, 0, "no round was verified");
        assert_eq!(
            card.sector(500),
            [0u8; SECTOR],
            "the write that was in flight is not stored"
        );
    }

    #[test]
    fn a_host_that_never_sets_a_configuration_ends_the_session_at_the_enumeration_limit() {
        let mut ctrl = Model::new();
        ctrl.event(Model::setup(0x80, 6, 0x0100, 0, 18));
        let (result, feeds, at, _) = session_on(&mut ctrl, 0, 0, Card::new(1024));
        assert_eq!(
            result,
            Err(SessionStop::Enumerate(EnumError::NotConfigured))
        );
        assert_eq!(
            feeds, 0,
            "the watchdog is not fed: the stage's exit is the reset that follows the report"
        );
        assert!(at_about(at, 30), "stopped at the idle deadline: {at} us");
        assert!(ctrl.bulk_in.is_empty());
    }

    #[test]
    fn an_unsupported_control_request_is_stalled_and_the_session_continues() {
        let mut ctrl = Model::new();
        ctrl.event(Model::setup(0xC0, 1, 0, 0, 16))
            .enumeration(7)
            .frame(identify())
            .frame(end());
        let (result, _, _) = session(&mut ctrl, &mut IterClock::new());
        assert_eq!(result, Ok(ResetKind::Normal));
        assert_eq!(
            ctrl.log[0],
            Action::Stall,
            "the vendor request is stalled first"
        );
        assert_eq!(
            ctrl.bulk_in.len(),
            2,
            "and the requests after it are served"
        );
    }

    #[test]
    fn an_idle_host_ends_the_session_at_the_idle_limit_with_its_own_outcome() {
        let mut ctrl = Model::new();
        ctrl.enumeration(7);
        let (result, feeds, at, _) = session_on(&mut ctrl, 0, 0, Card::new(1024));
        assert_eq!(
            result,
            Err(SessionStop::Serve(ServeError::Idle)),
            "not a link timeout"
        );
        assert_eq!(feeds, 0);
        assert!(at_about(at, 30), "stopped at the idle deadline: {at} us");
    }

    #[test]
    fn a_host_that_identifies_every_29_seconds_is_idle_at_30_seconds_and_the_watchdog_is_not_reached()
     {
        // Identify at about 0 s and at 29 s; the next would be at 58 s. Replies do not move the deadline (30 s after the
        // start, which is the arm), so the session ends at 30 s with its own outcome, before the watchdog's period.
        let mut ctrl = Model::new();
        ctrl.enumeration(7)
            .frame(identify())
            .gap(29_000)
            .frame(identify())
            .gap(29_000)
            .frame(identify());
        let (result, feeds, at, _) = session_on(&mut ctrl, 0, 0, Card::new(1024));
        assert_eq!(result, Err(SessionStop::Serve(ServeError::Idle)));
        assert_eq!(feeds, 0);
        assert_eq!(
            ctrl.bulk_in.len(),
            2,
            "the two identifies inside the window were answered; the third was not"
        );
        assert!(at_about(at, 30), "idle at the deadline: {at} us");
        assert!(
            at < REFERENCE_WATCHDOG_PERIOD_US,
            "before the watchdog's period"
        );
    }

    #[test]
    fn refusals_leave_the_deadline_unmoved_and_the_session_ends_idle_at_30_seconds() {
        // Two refused requests (a bad CRC), at about 0 s and 29 s: each is answered with status 1, and neither moves the
        // deadline.
        let mut bad = Request::read(1, 1).encode();
        bad[7] ^= 0x40;
        let mut ctrl = Model::new();
        ctrl.enumeration(7)
            .frame(bad.to_vec())
            .gap(29_000)
            .frame(bad.to_vec());
        let (result, feeds, at, _) = session_on(&mut ctrl, 0, 0, Card::new(1024));
        assert_eq!(result, Err(SessionStop::Serve(ServeError::Idle)));
        assert_eq!(feeds, 0);
        assert_eq!(
            ctrl.bulk_in,
            vec![vec![1u8], vec![1u8]],
            "two refusals, each with status 1"
        );
        assert!(
            at_about(at, 30),
            "idle at the deadline from the arm: {at} us"
        );
    }

    #[test]
    fn a_bus_reset_at_25_seconds_leaves_5_seconds_for_the_next_request() {
        // A reset at about 25 s; the host configures again at once, identifies at about 28 s (inside the 5 s left), and
        // then nothing. The re-enumeration did not extend the deadline: the session ends idle at 30 s.
        let mut ctrl = Model::new();
        ctrl.enumeration(7)
            .gap(25_000)
            .event(Event::BusReset)
            .enumeration(9)
            .gap(3_000)
            .frame(identify());
        let (result, feeds, at, _) = session_on(&mut ctrl, 0, 0, Card::new(1024));
        assert_eq!(result, Err(SessionStop::Serve(ServeError::Idle)));
        assert_eq!(feeds, 0);
        assert_eq!(
            ctrl.bulk_in.len(),
            1,
            "the identify inside the 5 s was answered"
        );
        assert_eq!(configurations(&ctrl), 2);
        assert!(at_about(at, 30), "idle at 30 s, not 50 s: {at} us");
    }

    #[test]
    fn a_bus_reset_does_not_grant_a_fresh_enumeration_limit() {
        // A reset at 20 s; the host never configures after it. Enumeration ends at the deadline counted from the arm, 30 s.
        let mut ctrl = Model::new();
        ctrl.enumeration(7)
            .gap(20_000)
            .event(Event::BusReset)
            .event(Model::setup(0x80, 6, 0x0100, 0, 18));
        let (result, feeds, at, _) = session_on(&mut ctrl, 0, 0, Card::new(1024));
        assert_eq!(
            result,
            Err(SessionStop::Enumerate(EnumError::NotConfigured))
        );
        assert_eq!(feeds, 0);
        assert!(
            at_about(at, 30),
            "the limit is the one from the arm: {at} us"
        );
    }

    #[test]
    fn a_write_round_moves_the_idle_deadline_to_its_feed_plus_30_seconds() {
        // A write at about 25 s is verified and fed; the deadline moves to about 55 s. An identify at 54 s is inside the new
        // window; without the feed the session would have ended at 30 s.
        let mut ctrl = Model::new();
        let (header, chunk) = write_one(500, 0x3C);
        ctrl.enumeration(7)
            .gap(25_000)
            .frame(header)
            .frame(chunk)
            .gap(29_000)
            .frame(identify());
        let (result, feeds, at, card) = session_on(&mut ctrl, 0, 0, Card::new(1024));
        assert_eq!(result, Err(SessionStop::Serve(ServeError::Idle)));
        assert_eq!(feeds, 1, "the write was verified and fed once");
        assert_eq!(card.sector(500), [0x3C; SECTOR], "and stored");
        assert_eq!(
            ctrl.bulk_in.len(),
            2,
            "the write's status, then the identify after the feed"
        );
        assert!(at_about(at, 55), "idle at the feed plus 30 s: {at} us");
    }

    #[test]
    fn the_first_deadline_counts_from_the_arm_not_from_the_start_of_the_session() {
        // The clock already reads 10 s when the session starts; the watchdog was armed at 0. The idle deadline is 30 s
        // after the arm, so the session ends at 30 s, not at 40 s.
        let mut ctrl = Model::new();
        ctrl.enumeration(7);
        let (result, feeds, at, _) = session_on(&mut ctrl, 10 * SECOND, 0, Card::new(1024));
        assert_eq!(result, Err(SessionStop::Serve(ServeError::Idle)));
        assert_eq!(feeds, 0);
        assert!(at_about(at, 30), "idle at 30 s from the arm: {at} us");
    }

    #[test]
    fn a_write_that_the_card_cannot_finish_within_its_budget_is_status_two_and_the_session_goes_on()
    {
        // A slow card: 4 s a sector, against a 3.5 s storage budget. The write at about 26 s ends in a storage failure (its
        // status is 2) before the deadline; nothing is fed; the identify after it is still answered, and the session ends
        // idle at 30 s, not in a watchdog reset.
        let mut ctrl = Model::new();
        let (header, chunk) = write_one(500, 0x3C);
        ctrl.enumeration(7)
            .gap(26_000)
            .frame(header)
            .frame(chunk)
            .frame(identify());
        let (result, feeds, at, card) =
            session_on(&mut ctrl, 0, 0, Card::new(1024).slow(4 * SECOND));
        assert_eq!(result, Err(SessionStop::Serve(ServeError::Idle)));
        assert_eq!(feeds, 0, "a failed write is not fed");
        assert_eq!(
            ctrl.bulk_in[0],
            vec![2u8],
            "the write's status is storage (2)"
        );
        assert_eq!(card.sector(500), [0u8; SECTOR], "and nothing was stored");
        assert!(
            at_about(at, 30),
            "idle at 30 s, well before the watchdog's period: {at} us"
        );
        assert!(
            at < FEED_TO_FEED_US + SECOND,
            "the session did not wait for the watchdog"
        );
    }
}
