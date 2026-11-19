//! The B2 scratch build's run: the FL1 stage's own USB device path (the board-proven B2 path: the same
//! [`Dwc2`] driver, FL1 device and session as [`crate::b2`]) over the card, reachable only inside the scratch
//! range (`lemnos_k230_usbboot_stage::fl1::scratch`). The image's entry calls [`scratch`] then [`end`] with the
//! board's buses; the PC rehearsal (`testing/crates/k230-usbboot`, `b2sim`) calls the same two over register
//! models of the controller, the SD controller and its card, the watchdogs and the console. Nothing here is
//! unsafe and nothing here names an address: the buses do.
//!
//! The order, after the entry armed both watchdogs (before any of this runs):
//!
//! 1. the banner on the console;
//! 2. the timebase started and checked, then the watchdog clock measured, as the check build does; either
//!    failing ends the run before the card or USB is touched;
//! 3. the shared entry: arm at the longest setting, read back, lower to the smallest period covering
//!    [`RUN_BUDGET_US`] at the measured clock (the check build's setting). The dogs are kicked only by the
//!    session's feed after a verified write round: a write the storage took whole and its read-back equal
//!    (`fl1::scratch::stage_scratch`, `VerifiedFeed`, `Watched`). A read round, a refusal, the idle wait and the
//!    USB poll never feed;
//! 4. the card, before USB: the SD pads must be on the SD function, the host must have named a controller,
//!    the card must start and be the card of record (its CID and size), and the data clock must start. Any
//!    of these failing ends the run with the card untouched by a write and USB never pulled up;
//! 5. the storage: [`Scratch`] over [`Sectors`] over [`Retried`]: a request outside the scratch range is
//!    refused whole before the card, and the card storage holds the declared range and the protected sectors
//!    besides (`lemnos_k230_usbboot_stage::bounds`);
//! 6. the controller's core reset ([`Dwc2`]), the FL1 device;
//! 7. the session, to its END, its idle limit or a failure;
//! 8. [`end`]: the card's lines and the record on the console, the cause, the SoC reset.

use core::cell::Cell;

use crate::b2::{self, Console, Known, hex8};
use crate::dwc2::{Dwc2, Dwc2Regs, Stats};
use crate::sdhci::{SdhciHost, SdhciRegs};
use lemnos_flash_stage::guard::Cause;
use lemnos_flash_stage::wait::Clock;
use lemnos_k230_usbboot_stage::bounds::CARD_DECLARED;
use lemnos_k230_usbboot_stage::fl1::check::{self, MAX_TOP, RUN_BUDGET_US};
use lemnos_k230_usbboot_stage::fl1::run::Profile;
use lemnos_k230_usbboot_stage::fl1::scratch::{
    Progress, SCRATCH_IDENTITY, Scratch, Watched, card_of_record, stage_scratch,
};
use lemnos_k230_usbboot_stage::fl1::storage::{Retried, Sectors};
use lemnos_k230_usbboot_stage::fl1::usb::Device;
use lemnos_k230_usbboot_stage::pads;
use lemnos_k230_usbboot_stage::sd::Card;
use lemnos_k230_usbboot_stage::uart;
use lemnos_k230_usbboot_stage::usb_dev::UsbHw;
use lemnos_k230_usbboot_stage::wdt::{RegAccess, period_us};

/// The banner, the first line after the entry's own.
pub const BANNER: &str = "lemnos stage: scratch (B2 card build: the USB device path, the card only inside 1048576..1050623; watchdog armed before this line)";

/// The line printed when an SD pad is not on the SD function: the card is not touched.
pub const PADS_LINE: &str =
    "b2 card: an SD pad is not on the SD function; no card, no USB bring-up";
/// The line printed when the host named no SD controller.
pub const NO_CONTROLLER_LINE: &str = "b2 card: no SD controller given; no card, no USB bring-up";
/// The line printed when the controller or the card does not start.
pub const NO_CARD_LINE: &str = "b2 card: the card did not start; no USB bring-up";
/// The line printed when the card is not the card of record: nothing is written to it.
pub const NOT_THE_CARD_LINE: &str =
    "b2 card: not the card of record; nothing written, no USB bring-up";
/// The line printed when the data clock does not start.
pub const NO_DATA_CLOCK_LINE: &str = "b2 card: the data clock did not start; no USB bring-up";
/// The line printed once the card is the card of record and the storage is set up.
pub const CARD_READY_LINE: &str =
    "b2 card: the card of record, storage limited to 1048576..1050623";

/// The setup steps the run numbers in its cause (`Cause::Setup`), after the check build's (1, 4, 5).
pub mod step {
    /// An SD pad is not on the SD function.
    pub const PADS: u8 = 6;
    /// No SD controller was given.
    pub const NO_CONTROLLER: u8 = 7;
    /// The controller or the card did not start.
    pub const NO_CARD: u8 = 8;
    /// The card is not the card of record.
    pub const NOT_THE_CARD: u8 = 9;
    /// The data clock did not start.
    pub const NO_DATA_CLOCK: u8 = 10;
}

/// What the run saw of the card.
pub struct CardSeen {
    /// The CID the card gave, once it started.
    pub cid: Cell<Option<[u8; 16]>>,
    /// The card's size in blocks, once it started.
    pub blocks: Cell<u64>,
    /// Failed block tries, each followed by a recovery and a new try (`Retried`).
    pub retries: Cell<u32>,
}

impl CardSeen {
    /// Nothing seen yet.
    pub const fn new() -> Self {
        Self {
            cid: Cell::new(None),
            blocks: Cell::new(0),
            retries: Cell::new(0),
        }
    }
}

impl Default for CardSeen {
    fn default() -> Self {
        Self::new()
    }
}

/// The run's buffers, each borrowed for the whole run.
pub struct Buffers<'a> {
    /// The session's frame buffer (`serve::BUFFER_LEN`).
    pub buf: &'a mut [u8],
    /// The session's read-back scratch (`serve::BUFFER_LEN`).
    pub scratch: &'a mut [u8],
    /// The bulk OUT buffer ([`b2::RX_LEN`]).
    pub rx: &'a mut [u8],
}

/// Runs steps 1 to 7 and returns why it stopped. It never resets: [`end`] does. `sd` is the SD controller
/// the host named, or none.
#[allow(clippy::too_many_arguments)]
pub fn scratch<R, D, S, K>(
    regs: &mut R,
    dwc2: D,
    sd: Option<S>,
    clock: &mut K,
    bufs: Buffers<'_>,
    stats: &Stats,
    known: &mut Known,
    card_seen: &CardSeen,
) -> Cause
where
    R: RegAccess,
    D: Dwc2Regs,
    S: SdhciRegs,
    K: Clock + Copy,
{
    let mut console = Console { dead: false };
    uart::clear_programmable_thre(regs);
    uart::configure_if_unset(regs);
    console.line(regs, &[BANNER]);
    check::enable_timebase(regs);
    match check::timebase_runs(regs, clock) {
        Ok(cross) => known.cross = Some(cross),
        Err(_) => {
            console.line(regs, &[b2::TIMEBASE_STOPPED_LINE]);
            return Cause::Setup { step: 5 };
        }
    }
    let measured = match check::measure(regs, clock) {
        Ok(measured) => measured,
        Err(_) => {
            console.line(regs, &[b2::NO_CLOCK_LINE]);
            return Cause::Setup { step: 4 };
        }
    };
    known.measured = Some(measured);
    let clock_hz = check::period_clock_hz(&measured);
    known.top = (0..=MAX_TOP)
        .find(|&top| period_us(top, clock_hz) >= RUN_BUDGET_US)
        .map_or(0xff, u32::from);
    let known: &Known = known;
    let profile = Profile {
        identity: &SCRATCH_IDENTITY,
        budget_us: RUN_BUDGET_US,
        clock_hz,
        max_top: MAX_TOP,
        feeds: true,
        rounds: Some(&known.rounds),
    };
    let Buffers { buf, scratch, rx } = bufs;
    let progress = Progress::new();
    let outcome = stage_scratch(
        regs,
        clock,
        buf,
        scratch,
        profile,
        &progress,
        |regs, clock| {
            let mut refuse = |regs: &mut R, line: &str, step: u8| {
                console.line(regs, &[line]);
                Cause::Setup { step }
            };
            if pads::sd_pads_on_sd_function(&pads::dump(regs)).is_err() {
                return Err(refuse(regs, PADS_LINE, step::PADS));
            }
            let Some(sd) = sd else {
                return Err(refuse(regs, NO_CONTROLLER_LINE, step::NO_CONTROLLER));
            };
            let card = SdhciHost::start(sd).and_then(Card::init);
            let Ok(mut card) = card else {
                return Err(refuse(regs, NO_CARD_LINE, step::NO_CARD));
            };
            card_seen.cid.set(Some(card.cid()));
            card_seen.blocks.set(card.blocks());
            if !card_of_record(&card.cid(), card.blocks()) {
                return Err(refuse(regs, NOT_THE_CARD_LINE, step::NOT_THE_CARD));
            }
            if card.host_mut().data_clock().is_err() {
                return Err(refuse(regs, NO_DATA_CLOCK_LINE, step::NO_DATA_CLOCK));
            }
            let storage = Watched::new(
                Scratch::new(Sectors::new(
                    Retried::new(card, &card_seen.retries),
                    *clock,
                    CARD_DECLARED,
                )),
                &progress,
            );
            console.line(regs, &[CARD_READY_LINE]);
            let mut hw = Dwc2::with(dwc2, rx, stats);
            hw.core_reset().map_err(|_| Cause::Setup { step: 1 })?;
            Ok((Device::new(hw, *clock), storage))
        },
    );
    match outcome {
        Ok(kind) => Cause::Ended(kind),
        Err(cause) => cause,
    }
}

/// Step 8: the card's lines (`b2 card-cid0` to `card-cid3`, `card-blocks-lo`, `card-retries`), then the check
/// build's end: the record as `b2 <name> <hex>` lines, `lemnos stage: end`, the cause and the SoC reset. Does
/// not return.
pub fn end<R: RegAccess>(
    regs: &mut R,
    stats: &Stats,
    known: &Known,
    card_seen: &CardSeen,
    now_ticks: u64,
    cause: Cause,
) -> ! {
    let mut console = Console { dead: false };
    if let Some(cid) = card_seen.cid.get() {
        for (i, name) in ["card-cid0", "card-cid1", "card-cid2", "card-cid3"]
            .iter()
            .enumerate()
        {
            let word =
                u32::from_be_bytes([cid[4 * i], cid[4 * i + 1], cid[4 * i + 2], cid[4 * i + 3]]);
            let digits = hex8(word);
            console.line(
                regs,
                &[
                    "b2 ",
                    name,
                    " ",
                    core::str::from_utf8(&digits).unwrap_or("????????"),
                ],
            );
        }
    }
    for (name, value) in [
        ("card-blocks-lo", card_seen.blocks.get() as u32),
        ("card-retries", card_seen.retries.get()),
    ] {
        let digits = hex8(value);
        console.line(
            regs,
            &[
                "b2 ",
                name,
                " ",
                core::str::from_utf8(&digits).unwrap_or("????????"),
            ],
        );
    }
    b2::end(regs, stats, known, now_ticks, cause)
}
