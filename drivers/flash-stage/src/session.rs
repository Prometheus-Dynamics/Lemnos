//! The session loop: enumerate the device, serve the bulk pair until an END, and enumerate again after a bus reset.
//!
//! **One clock.** Every limit in the session is one deadline, the **idle deadline**: [`IDLE_LIMIT_US`] after the last
//! watchdog feed, or after the start of the session (the watchdog is armed at the start, so the start counts as a
//! feed). The deadline moves only when a round feeds the watchdog. A reply does not move it, a refused request does
//! not, and a bus reset does not: a host that identifies every 29 seconds gets its idle expiry at the same time as a
//! host that never asks. Enumeration (before the first request, and after each bus reset) shares the same deadline,
//! so a reset does not grant a fresh limit beyond what the watchdog allows.
//!
//! The watchdog's period covers the worst gap between two feeds (see `timing`), and the idle deadline is earlier than
//! that, so the distinct idle outcome comes before any reset of the watchdog.
//!
//! After a stop the chip's stated exit follows: its report, then the park that never feeds the watchdog.

use crate::guard::Watchdog;
use crate::link::{IoError, Storage};
use crate::region::Region;
use crate::serve::{Context, ServeError};
use crate::usb::{BulkLink, Controller, EnumError, Ep0, enumerate};
use crate::wait::Clock;
use lemnos_flash_wire::frame::ResetKind;
use lemnos_flash_wire::timing::IDLE_LIMIT_US;

/// Why the session ended without an END.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStop {
    /// The host did not configure the device before the idle deadline, or it was disconnected.
    Enumerate(EnumError),
    /// The serve loop ended without an END: the idle deadline passed with no request, or the link failed for good.
    Serve(ServeError),
}

/// What the serve loop needs besides the controller and the storage: the card's regions, the identity record, and the
/// two buffers (each at least `serve::BUFFER_LEN` bytes).
pub struct Setup<'a> {
    pub regions: &'a [Region],
    pub identity: &'a [u8],
    pub buf: &'a mut [u8],
    pub scratch: &'a mut [u8],
}

/// Counts the feeds a step makes, so the session knows when the watchdog was fed and the idle deadline moves.
struct Counted<'a, W: Watchdog> {
    inner: &'a mut W,
    feeds: u32,
}

impl<W: Watchdog> Watchdog for Counted<'_, W> {
    fn feed(&mut self) {
        self.feeds += 1;
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
    let mut last_feed = clock.now_us();
    let mut counted = Counted {
        inner: wdt,
        feeds: 0,
    };
    loop {
        let deadline = last_feed.saturating_add(IDLE_LIMIT_US);
        let ctx = Context {
            regions: setup.regions,
            identity: setup.identity,
        };
        enumerate(&mut *ctrl, &mut *clock, &mut *ep0, deadline).map_err(SessionStop::Enumerate)?;
        loop {
            let budget = deadline.saturating_sub(clock.now_us());
            let before = counted.feeds;
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
            if counted.feeds != before {
                last_feed = clock.now_us();
            }
            match served {
                Ok(crate::serve::Progress::Ended(kind)) => return Ok(kind),
                Ok(crate::serve::Progress::Idle) => {
                    return Err(SessionStop::Serve(ServeError::Idle));
                }
                Ok(_) => {}
                Err(ServeError::Link(IoError::Reset)) => break,
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
    use lemnos_flash_wire::frame::{MAX_CHUNK, Request, SECTOR, payload_crc};
    use lemnos_flash_wire::identity::identity_record;
    use lemnos_flash_wire::usb::CONFIGURATION_VALUE;

    /// A clock that moves a millisecond per read: a thirty-second limit is thirty thousand reads.
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

    /// Runs the session once from reset, and returns what it ended with, the feeds, and the card.
    fn session(
        ctrl: &mut Model,
        clock: &mut impl Clock,
    ) -> (Result<ResetKind, SessionStop>, u32, Card) {
        let mut ep0 = Ep0::new();
        let mut card = Card::new(1024);
        let mut dog = Dog::default();
        let identity = identity_record(42, &REGIONS);
        let mut buf = vec![0u8; BUFFER_LEN];
        let mut scratch = vec![0u8; MAX_CHUNK];
        let result = run(
            ctrl,
            clock,
            &mut ep0,
            &mut card,
            &mut dog,
            Setup {
                regions: &REGIONS,
                identity: &identity,
                buf: &mut buf,
                scratch: &mut scratch,
            },
        );
        (result, dog.0, card)
    }

    fn identify() -> Vec<u8> {
        Request::identify().encode().to_vec()
    }

    fn end() -> Vec<u8> {
        Request::end(ResetKind::Normal).encode().to_vec()
    }

    fn configurations(ctrl: &Model) -> usize {
        ctrl.log
            .iter()
            .filter(|a| **a == Action::Configured(true))
            .count()
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
        let payload = vec![0x3C; SECTOR];
        let mut chunk = payload.clone();
        chunk.extend_from_slice(&payload_crc(&payload));
        ctrl.enumeration(7)
            .frame(Request::write(500, 1, false).encode().to_vec())
            .event(Event::BusReset)
            .enumeration(9)
            .frame(identify())
            .frame(end());
        let _ = chunk;
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
        let (result, feeds, _) = session(&mut ctrl, &mut Quick(0));
        assert_eq!(
            result,
            Err(SessionStop::Enumerate(EnumError::NotConfigured))
        );
        assert_eq!(
            feeds, 0,
            "the watchdog is not fed: the stage's exit is the reset that follows the report"
        );
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
        let (result, feeds, _) = session(&mut ctrl, &mut Quick(0));
        assert_eq!(result, Err(SessionStop::Serve(ServeError::Idle)));
        assert_eq!(feeds, 0);
    }

    #[test]
    fn the_configuration_value_the_host_sets_is_the_one_the_device_names() {
        assert_eq!(CONFIGURATION_VALUE, 1);
    }
}
