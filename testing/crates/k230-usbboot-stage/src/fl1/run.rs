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
use crate::wdt::RegAccess;
use lemnos_flash_stage::guard::{Cause, finish, start};
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
    let armed_at_us = clock.now_us();
    start(&mut Dogs::new(&mut *regs), SESSION_BUDGET_US).map_err(Cause::Start)?;
    let (mut ctrl, mut storage) = bring_up(regs, clock)?;
    let mut ep0 = Ep0::default();
    let mut dogs = Dogs::new(&mut *regs);
    session::run(
        &mut ctrl,
        clock,
        &mut ep0,
        &mut storage,
        &mut dogs,
        Setup {
            regions: &REGIONS,
            identity: &IDENTITY,
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
