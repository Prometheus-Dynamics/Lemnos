//! The K230 stage's entry, in the order the shared contract requires:
//!
//! 1. read the clock (the session's clock starts at the arm);
//! 2. arm both watchdogs, read the arming back, and lower the period to the smallest that covers
//!    the session budget ([`lemnos_flash_stage::guard::start`]). Nothing else has run yet, so a
//!    stage whose watchdog does not arm makes no USB or SD call;
//! 3. bring the controllers up (`bring_up`), each bring-up wait bounded by the caller;
//! 4. run the shared session until an END or a stop.
//!
//! [`stage`] returns the outcome and never resets; [`run`] reports it and resets (the exit). Both
//! watchdogs are fed only by the shared session's verified rounds, never by this module.

use crate::fl1::guard::Dogs;
use crate::fl1::report::Uart;
use crate::fl1::{IDENTITY, REGIONS};
use crate::wdt::{CLOCK_HZ_MIN, RegAccess, TOP};
use core::cell::Cell;
use lemnos_flash_stage::guard::{Cause, Watchdog, finish, start};
use lemnos_flash_stage::link::Storage;
use lemnos_flash_stage::session::{self, SessionStop, Setup};
use lemnos_flash_stage::usb::{Controller, Ep0};
use lemnos_flash_stage::wait::Clock;
use lemnos_flash_wire::frame::ResetKind;
use lemnos_flash_wire::timing::SESSION_BUDGET_US;

/// Arms the watchdogs, brings the controllers up, and serves the session to its END.
///
/// `bring_up` makes the controller and the storage. It is called only after the arming read back,
/// and its failure is `Cause::Setup`; each of its waits must be bounded.
pub fn stage<C, S, K, R, B>(
    regs: &mut R,
    clock: &mut K,
    buf: &mut [u8],
    scratch: &mut [u8],
    bring_up: B,
) -> Result<ResetKind, Cause>
where
    C: Controller,
    S: Storage,
    K: Clock,
    R: RegAccess,
    B: FnOnce(&mut R, &mut K) -> Result<(C, S), Cause>,
{
    stage_with(
        regs,
        clock,
        buf,
        scratch,
        Profile {
            identity: &IDENTITY,
            budget_us: SESSION_BUDGET_US,
            clock_hz: CLOCK_HZ_MIN,
            max_top: TOP,
            feeds: true,
            rounds: None,
        },
        bring_up,
    )
}

/// What a build of the stage serves with: its identity record, the budget its watchdog period must cover,
/// the watchdog clock the periods are computed at, and whether a verified round feeds. The regions are the
/// K230's ([`REGIONS`]) in every build.
#[derive(Clone, Copy)]
pub struct Profile<'a> {
    /// The identity record a host reads first.
    pub identity: &'a [u8],
    /// The budget the period is lowered to cover ([`lemnos_flash_stage::guard::start`]).
    pub budget_us: u64,
    /// The watchdog clock, rounded up (a period is never taken as longer than it is).
    pub clock_hz: u64,
    /// The longest setting, armed first.
    pub max_top: u8,
    /// Whether a verified round feeds the watchdogs. A build that never feeds is bounded by one period
    /// from the arming, whatever the host does.
    pub feeds: bool,
    /// Counts the verified rounds (the feeds the shared rule asks for, made or not), when given.
    pub rounds: Option<&'a Cell<u32>>,
}

/// The session's watchdog: the two dogs, fed or not as the profile says, with the rounds counted.
struct Rounds<'a, R: RegAccess> {
    dogs: Dogs<R>,
    feeds: bool,
    rounds: Option<&'a Cell<u32>>,
}

impl<R: RegAccess> Watchdog for Rounds<'_, R> {
    fn feed(&mut self) {
        if let Some(rounds) = self.rounds {
            rounds.set(rounds.get().saturating_add(1));
        }
        if self.feeds {
            self.dogs.feed();
        }
    }
}

/// [`stage`] with a build's own [`Profile`]. The order is the same: arm and read back first, then the
/// bring-up, then the session.
pub fn stage_with<C, S, K, R, B>(
    regs: &mut R,
    clock: &mut K,
    buf: &mut [u8],
    scratch: &mut [u8],
    profile: Profile<'_>,
    bring_up: B,
) -> Result<ResetKind, Cause>
where
    C: Controller,
    S: Storage,
    K: Clock,
    R: RegAccess,
    B: FnOnce(&mut R, &mut K) -> Result<(C, S), Cause>,
{
    let armed_at_us = clock.now_us();
    start(
        &mut Dogs::with_clock(&mut *regs, profile.clock_hz, profile.max_top),
        profile.budget_us,
    )
    .map_err(Cause::Start)?;
    let (mut ctrl, mut storage) = bring_up(regs, clock)?;
    let mut ep0 = Ep0::default();
    let mut guard = Rounds {
        dogs: Dogs::with_clock(&mut *regs, profile.clock_hz, profile.max_top),
        feeds: profile.feeds,
        rounds: profile.rounds,
    };
    session::run(
        &mut ctrl,
        clock,
        &mut ep0,
        &mut storage,
        &mut guard,
        Setup {
            regions: &REGIONS,
            identity: profile.identity,
            buf,
            scratch,
            armed_at_us,
        },
    )
    .map_err(|stop| match stop {
        SessionStop::Enumerate(_) => Cause::Setup { step: 2 },
        SessionStop::Serve(error) => Cause::Serve(error),
    })
}

/// The entry: [`stage`], then the report, then the reset. The reset is the exit, and this does not
/// return: a stage that is not reset parks, and never feeds.
pub fn run<C, S, K, R, B>(
    regs: &mut R,
    clock: &mut K,
    buf: &mut [u8],
    scratch: &mut [u8],
    bring_up: B,
) -> !
where
    C: Controller,
    S: Storage,
    K: Clock,
    R: RegAccess,
    B: FnOnce(&mut R, &mut K) -> Result<(C, S), Cause>,
{
    let outcome = stage(regs, clock, buf, scratch, bring_up);
    let cause = match outcome {
        Ok(kind) => Cause::Ended(kind),
        Err(cause) => cause,
    };
    finish(&mut Uart::new(regs), cause)
}
