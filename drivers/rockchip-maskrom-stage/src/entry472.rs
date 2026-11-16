//! The 472 loader's session: what it does from the moment the boot ROM runs it.
//!
//! Order: arm the watchdog and read the arming back, then lower the period to cover the session budget (the
//! shared entry contract). Reset the DWC3 core and device; bring up the SD card; bring up the device side (the
//! event buffer, the device events, the control endpoints) and set the run bit. Then the device is enumerated by
//! the host, and the bulk pair serves requests until an END. A bus reset or an unconfiguration during the session
//! sends the device back to enumeration: the host starts again, and nothing else is lost but the chunk in
//! flight. An idle host or a dead link ends the session, and the watchdog's reset is the exit.
//!
//! Every failure returns without a feed, so the watchdog's reset is the exit.

use crate::bind::{DWC3, Dog, RegAccess, SDMMC0, dwc3_start_device};
use crate::dwc3link::{Bus, DmaMem, SdStorage, WdtFeed};
use crate::sd::{InitError, sd_init};
use crate::usb::{Dwc3Dev, EventMem};
pub use lemnos_flash_stage::guard::park;
use lemnos_flash_stage::guard::{Cause, Report, StartError, finish, start};
use lemnos_flash_stage::region::{Class, Region};
use lemnos_flash_stage::serve::ServeError;
use lemnos_flash_stage::session::{self as shared, SessionStop, Setup};
use lemnos_flash_stage::usb::{EnumError, Ep0, UsbError};
use lemnos_flash_stage::wait::{Clock, WaitError};
use lemnos_flash_wire::frame::ResetKind;
use lemnos_flash_wire::identity::{IDENTITY_LEN, identity_record};
use lemnos_flash_wire::timing::{INIT_BUDGET_US, SESSION_BUDGET_US};

/// The stage's build id: the decimal value of `LEMNOS_ROCKCHIP_STAGE_BUILD_ID` when the build sets it, zero
/// when it does not (zero means unset). The bench builder sets it from the image's inputs.
pub const BUILD_ID: u64 = parse_build_id(option_env!("LEMNOS_ROCKCHIP_STAGE_BUILD_ID"));

const fn parse_build_id(text: Option<&str>) -> u64 {
    match text {
        None => 0,
        Some(text) => {
            let bytes = text.as_bytes();
            let mut value = 0u64;
            let mut i = 0;
            while i < bytes.len() {
                assert!(
                    bytes[i] >= b'0' && bytes[i] <= b'9',
                    "LEMNOS_ROCKCHIP_STAGE_BUILD_ID is decimal"
                );
                value = value * 10 + (bytes[i] - b'0') as u64;
                i += 1;
            }
            value
        }
    }
}

/// The card's regions, as this stage polices them. The boot record (sector 64, `RKNS`) is a header; the boot area
/// is the whole loader area before the uboot-env partition, sectors 64 up to 32768 (MR1.md, "Board plan"). The
/// host's own image length is a different number: it sets how much of the area a plan writes, not what the stage
/// protects, which is all of it.
pub const REGIONS: [Region; 2] = [
    Region {
        first: 64,
        end: 32_768,
        class: Class::BootArea,
    },
    Region {
        first: 64,
        end: 65,
        class: Class::Header,
    },
];

/// The identity record this stage answers an identify with.
pub const IDENTITY: [u8; IDENTITY_LEN] = identity_record(BUILD_ID, &REGIONS);

/// Where the session stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The entry contract's start failed.
    Start(StartError),
    /// The DWC3 core or device would not reset, or the device side would not come up.
    Dwc3(UsbError),
    /// The host did not configure the device within the enumeration limit, or it was disconnected.
    Enumerate(EnumError),
    /// The SD card would not come up.
    SdHost(InitError),
    /// The serve loop ended without an END.
    Serve(ServeError),
}

/// Maps a bounded wait's failure to the device side's error.
fn wait_error(_: WaitError) -> UsbError {
    UsbError::Timeout
}

/// The stage's clock, read through the bus so the device side and the link share one timer.
struct BusClock<'b, 'a, C: Clock> {
    bus: &'b Bus<'a, C>,
}

impl<C: Clock> Clock for BusClock<'_, '_, C> {
    fn now_us(&mut self) -> u64 {
        self.bus.with(|_, clock| clock.now_us())
    }
}

/// Runs the session to its END, or to a stop. Returns the reset kind the END asked for.
///
/// `dma` is the device's DMA memory; `buf` and `scratch` each hold a full chunk (see `serve::BUFFER_LEN`).
pub fn session<C: Clock, M: DmaMem + EventMem>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    dma: M,
    buf: &mut [u8],
    scratch: &mut [u8],
) -> Result<ResetKind, Stop> {
    // The session's clock starts at the arm: the watchdog is armed here, before the DWC3 and SD bring-up below, so the
    // first idle deadline counts from this instant (timing.rs, "One clock").
    let armed_at_us = clock.now_us();
    // The entry contract's first step: arm, read the arming back, lower the period. Nothing else runs first.
    start(&mut Dog { regs }, SESSION_BUDGET_US).map_err(Stop::Start)?;
    // Every bring-up wait takes the init deadline: the device is visible no later than `INIT_BUDGET_US` after the arm
    // (timing.rs, "The stage's init budget").
    let init_deadline_us = armed_at_us.saturating_add(INIT_BUDGET_US);
    dwc3_start_device(regs, clock, DWC3, init_deadline_us)
        .map_err(|e| Stop::Dwc3(wait_error(e)))?;
    let card = sd_init(regs, clock, SDMMC0, init_deadline_us).map_err(Stop::SdHost)?;
    let bus = Bus::new(regs, clock);
    let mut dev = Dwc3Dev::start(&bus, dma, DWC3, init_deadline_us).map_err(Stop::Dwc3)?;
    Dwc3Dev::<C, M>::run(&bus, DWC3);
    let mut storage = SdStorage::new(&bus, SDMMC0, card);
    let mut wdt = WdtFeed::new(&bus);
    let mut time = BusClock { bus: &bus };
    let mut ep0 = Ep0::new();
    // The shared session: enumerate, serve, and enumerate again after a bus reset (flash-stage's `session::run`).
    shared::run(
        &mut dev,
        &mut time,
        &mut ep0,
        &mut storage,
        &mut wdt,
        Setup {
            regions: &REGIONS,
            identity: &IDENTITY,
            buf,
            scratch,
            armed_at_us,
        },
    )
    .map_err(|stop| match stop {
        SessionStop::Enumerate(e) => Stop::Enumerate(e),
        SessionStop::Serve(e) => Stop::Serve(e),
    })
}

/// The chip's report: the cause is kept where a reset does not clear it, and this stage has no such place
/// yet (MR1.md: the boot-mode handoff is not written). So the report is a no-op, and the order is kept.
struct NoChannel;

impl Report for NoChannel {
    fn cause(&mut self, _cause: Cause) {}
}

/// The cause a stop reports as.
fn cause_of(stop: Stop) -> Cause {
    match stop {
        Stop::Start(e) => Cause::Start(e),
        Stop::Dwc3(_) => Cause::Setup { step: 1 },
        Stop::Enumerate(_) => Cause::Setup { step: 2 },
        Stop::SdHost(_) => Cause::Setup { step: 3 },
        Stop::Serve(e) => Cause::Serve(e),
    }
}

/// The loader's run, called by the -sys crate's entry. Returns only through [`park`], which the reset ends.
pub fn run<C: Clock, M: DmaMem + EventMem>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    dma: M,
    buf: &mut [u8],
    scratch: &mut [u8],
) -> ! {
    let cause = match session(regs, clock, dma, buf, scratch) {
        Ok(kind) => Cause::Ended(kind),
        Err(stop) => cause_of(stop),
    };
    finish(&mut NoChannel, cause)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_id_parses_as_decimal_and_zero_means_unset() {
        assert_eq!(parse_build_id(None), 0);
        assert_eq!(parse_build_id(Some("0")), 0);
        assert_eq!(parse_build_id(Some("4096")), 4096);
    }

    #[test]
    fn the_identity_publishes_the_stage_regions_and_build_id() {
        let peer = lemnos_flash_wire::identity::check(&IDENTITY).unwrap();
        assert_eq!(peer.regions(), &REGIONS[..]);
        assert_eq!(peer.build_id, BUILD_ID);
    }

    // The session budget is below the longest period the watchdog offers (checked at compile time).
    const _: () = assert!(SESSION_BUDGET_US < crate::wdt::LONGEST_PERIOD_US);
}
