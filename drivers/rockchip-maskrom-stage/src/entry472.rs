//! The 472 loader's session: what it does from the moment the boot ROM runs it.
//!
//! Order: arm the watchdog (the prologue in `stage471.S` already did, and this does it again so the
//! body is self-contained); set the session period; bring up the DWC3 device controller; configure the
//! bulk OUT endpoint (the host's frames come in) and the bulk IN endpoint (replies go out); bring up the
//! SD card (sdmmc0); then serve requests over the link until an END frame. Every failure returns
//! without a feed, so the watchdog's reset is the exit. An END returns the reset kind it asked for; the
//! reset itself is the watchdog's, and the boot-mode handoff for the maskrom kind is not written yet.

use crate::bind::{DWC3, Dog, RegAccess, SDMMC0, dwc3_start_device};
use crate::dwc3link::{Bus, DmaMem, DwcLink, SdStorage, WdtFeed};
use crate::io::{IoFault, configure_bulk_endpoint};
use crate::sd::{InitError, sd_init};
use lemnos_flash_stage::chunk::ROUND_BUDGET_US;
pub use lemnos_flash_stage::guard::park;
use lemnos_flash_stage::guard::{Cause, Report, StartError, finish, start};
use lemnos_flash_stage::region::{Class, Region};
use lemnos_flash_stage::serve::{ServeError, serve};
use lemnos_flash_stage::wait::Clock;
use lemnos_flash_wire::frame::ResetKind;

/// The bulk OUT endpoint (the host to the loader): DWC3 endpoint number 2 (EP1 OUT).
pub const EP_OUT: usize = 2;
/// The bulk IN endpoint (the loader to the host): DWC3 endpoint number 3 (EP1 IN).
pub const EP_IN: usize = 3;
/// The bulk endpoints' maximum packet size (full speed and high speed bulk: 512 bytes).
pub const MAX_PACKET: u16 = 512;

/// Where the session stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The session period could not be set (above the watchdog's longest period).
    Start(StartError),
    Dwc3(IoFault),
    Endpoint(IoFault),
    SdHost(InitError),
    /// The serve loop ended before an END frame.
    Serve(ServeError),
}

/// Runs the session to its END. Returns the reset kind the END asked for, or the stop. No branch here
/// feeds the watchdog after a failure.
///
/// `dma` is the link's DMA memory; `buf` and `scratch` each hold at least a full frame (128 sectors and
/// their CRC).
pub fn session<C: Clock, M: DmaMem>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    dma: M,
    buf: &mut [u8],
    scratch: &mut [u8],
) -> Result<ResetKind, Stop> {
    // The entry contract's first step: arm, read the arming back, lower the period. Nothing else runs first.
    start(&mut Dog { regs }, ROUND_BUDGET_US).map_err(Stop::Start)?;
    dwc3_start_device(regs, clock, DWC3).map_err(|e| Stop::Dwc3(IoFault::from(e)))?;
    configure_bulk_endpoint(regs, clock, DWC3, EP_OUT, MAX_PACKET, 0).map_err(Stop::Endpoint)?;
    configure_bulk_endpoint(regs, clock, DWC3, EP_IN, MAX_PACKET, 0).map_err(Stop::Endpoint)?;
    let card = sd_init(regs, clock, SDMMC0).map_err(Stop::SdHost)?;
    let bus = Bus::new(regs, clock);
    let mut link = DwcLink::new(&bus, dma, DWC3, EP_OUT, EP_IN);
    let mut storage = SdStorage::new(&bus, SDMMC0, card);
    let mut wdt = WdtFeed::new(&bus);
    serve(&mut link, &mut storage, &mut wdt, &REGIONS, buf, scratch).map_err(Stop::Serve)
}

/// The card's regions, as this stage polices them. The boot record (sector 64, `RKNS`) is a header; the
/// loader area is the boot area, up to the uboot-env partition at sector 32768 (MR1.md, "Board plan").
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
        Stop::Endpoint(_) => Cause::Setup { step: 2 },
        Stop::SdHost(_) => Cause::Setup { step: 3 },
        Stop::Serve(e) => Cause::Serve(e),
    }
}

/// The loader's run, called by the -sys crate's entry. Returns only through [`park`].
pub fn run<C: Clock, M: DmaMem>(
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
    use crate::bind::{WATCHDOG, feed};
    use crate::models::{DwcEndpoints, SdCard, SimDma};
    use crate::regs::dwc3;
    use crate::wdt::{CRR_OFFSET, KICK};
    use lemnos_flash_stage::link::IoError;
    use lemnos_flash_stage::wait::IterClock;
    use lemnos_flash_wire::frame::crc16_ccitt;
    use lemnos_flash_wire::frame::{Op, Request, SECTOR};
    use std::collections::HashMap;

    /// The board's register map for the session: the SD host is an SD card model, the DWC3 endpoint
    /// block is an endpoint model, and everything else (the watchdog, the DWC3 global and device
    /// registers) is a plain register file in which the self-clearing and stuck bits behave as the
    /// controller's do.
    struct Board {
        sd: SdCard,
        dwc: DwcEndpoints,
        other: HashMap<usize, u32>,
        stuck: Vec<(usize, u32)>,
        /// Writes to the watchdog, in order.
        kicks: u32,
        /// Regions in the order their first access happened.
        order: Vec<&'static str>,
    }

    const DEP_START: usize = DWC3 + dwc3::DEP_BASE_START;
    const DEP_END: usize = DEP_START + 16 * dwc3::DEP_STRIDE;

    fn self_clearing(addr: usize) -> u32 {
        if addr == DWC3 + dwc3::GCTL {
            return dwc3::GCTL_CORESOFTRESET;
        }
        if addr == DWC3 + dwc3::DCTL {
            return dwc3::DCTL_CSFTRST;
        }
        0
    }

    impl Board {
        fn new() -> Self {
            Self {
                sd: SdCard::new(SDMMC0, 16),
                dwc: DwcEndpoints::new(DWC3),
                other: HashMap::new(),
                stuck: Vec::new(),
                kicks: 0,
                order: Vec::new(),
            }
        }

        fn note(&mut self, region: &'static str) {
            if !self.order.contains(&region) {
                self.order.push(region);
            }
        }
    }

    impl RegAccess for Board {
        fn read32(&mut self, addr: usize) -> u32 {
            if (SDMMC0..SDMMC0 + 0x4000).contains(&addr) {
                self.note("sd");
                return self.sd.read32(addr);
            }
            if (DEP_START..DEP_END).contains(&addr) || addr == DWC3 + dwc3::DALEPENA {
                self.note("dwc-endpoints");
                return self.dwc.read32(addr);
            }
            if addr == WATCHDOG + crate::wdt::CR_OFFSET {
                self.note("watchdog");
            } else if (DWC3..DWC3 + 0x400000).contains(&addr) {
                self.note("dwc-global");
            }
            let mut value = *self.other.get(&addr).unwrap_or(&0);
            if let Some(bits) = self.stuck.iter().find(|(at, _)| *at == addr) {
                value |= bits.1;
            }
            value
        }

        fn write32(&mut self, addr: usize, value: u32) {
            if (SDMMC0..SDMMC0 + 0x4000).contains(&addr) {
                self.note("sd");
                self.sd.write32(addr, value);
                return;
            }
            if (DEP_START..DEP_END).contains(&addr) || addr == DWC3 + dwc3::DALEPENA {
                self.note("dwc-endpoints");
                self.dwc.write32(addr, value);
                return;
            }
            if addr == WATCHDOG + CRR_OFFSET && value == KICK {
                self.kicks += 1;
            }
            if addr == WATCHDOG + crate::wdt::CR_OFFSET {
                self.note("watchdog");
            } else if (DWC3..DWC3 + 0x400000).contains(&addr) {
                self.note("dwc-global");
            }
            self.other.insert(addr, value & !self_clearing(addr));
        }
    }

    fn frame(op: Op, reserved: u8, count: u16, lba: u32) -> Vec<u8> {
        Request {
            op,
            reserved,
            count,
            lba,
        }
        .encode()
        .to_vec()
    }

    fn with_crc(data: &[u8]) -> Vec<u8> {
        let mut out = data.to_vec();
        out.extend(crc16_ccitt(data).to_be_bytes());
        out
    }

    fn buffers() -> (Vec<u8>, Vec<u8>) {
        (vec![0; 128 * SECTOR + 2], vec![0; 128 * SECTOR])
    }

    #[test]
    fn the_session_arms_the_watchdog_first() {
        let mut board = Board::new();
        let mut clock = IterClock::new();
        let (mut buf, mut scratch) = buffers();
        let _ = session(
            &mut board,
            &mut clock,
            SimDma::new(vec![]),
            &mut buf,
            &mut scratch,
        );
        assert_eq!(board.order.first(), Some(&"watchdog"));
        assert!(
            board.kicks >= 2,
            "the arming kick and the session-period kick"
        );
    }

    #[test]
    fn a_write_and_an_end_run_the_session_to_its_end_with_one_feed_per_verified_chunk() {
        let mut board = Board::new();
        let mut clock = IterClock::new();
        let (mut buf, mut scratch) = buffers();
        let payload = vec![0x5Au8; SECTOR];
        let frames = vec![
            frame(Op::Write, 0, 1, 3),
            with_crc(&payload),
            frame(Op::End, 0, 1, 0),
        ];
        let result = session(
            &mut board,
            &mut clock,
            SimDma::new(frames),
            &mut buf,
            &mut scratch,
        );
        assert_eq!(result, Ok(ResetKind::Normal));
        assert_eq!(board.sd.blocks[3], payload[..]);
        assert_eq!(board.kicks, 3, "arm, period, one verified chunk");
        assert_eq!(
            board.dwc.dalepena & ((1 << EP_OUT) | (1 << EP_IN)),
            (1 << EP_OUT) | (1 << EP_IN)
        );
        assert!(board.sd.commands >= 4, "the card identification ran");
        assert_eq!(
            board.sd.read32(SDMMC0 + crate::regs::dw_mmc::CTYPE),
            1,
            "4-bit bus"
        );
    }

    #[test]
    fn a_bus_reset_mid_transfer_stops_the_session_with_no_feed_after_it() {
        let mut board = Board::new();
        let mut clock = IterClock::new();
        let (mut buf, mut scratch) = buffers();
        let mut sim = SimDma::new(vec![frame(Op::Write, 0, 1, 0), with_crc(&[0u8; SECTOR])]);
        sim.stall_after = Some(1);
        let result = session(&mut board, &mut clock, sim, &mut buf, &mut scratch);
        assert_eq!(result, Err(Stop::Serve(ServeError::Link(IoError::Timeout))));
        assert!(board.sd.blocks[0].iter().all(|&b| b == 0));
        assert_eq!(
            board.kicks, 2,
            "arm and period only; no feed after the reset"
        );
    }

    #[test]
    fn a_stuck_dwc3_reset_stops_the_session_before_any_endpoint_or_card_command() {
        let mut board = Board::new();
        board.stuck = vec![(DWC3 + dwc3::GCTL, dwc3::GCTL_CORESOFTRESET)];
        let mut clock = IterClock::new();
        let (mut buf, mut scratch) = buffers();
        let result = session(
            &mut board,
            &mut clock,
            SimDma::new(vec![]),
            &mut buf,
            &mut scratch,
        );
        assert!(
            matches!(result, Err(Stop::Dwc3(IoFault::Timeout))),
            "{result:?}"
        );
        assert!(board.dwc.commands.is_empty());
        assert_eq!(board.sd.commands, 0);
        assert_eq!(board.kicks, 2, "no feed after the stop");
    }

    #[test]
    fn a_stuck_endpoint_command_stops_the_session_at_the_endpoint() {
        let mut board = Board::new();
        board.dwc.stuck = vec![EP_OUT];
        let mut clock = IterClock::new();
        let (mut buf, mut scratch) = buffers();
        let result = session(
            &mut board,
            &mut clock,
            SimDma::new(vec![]),
            &mut buf,
            &mut scratch,
        );
        assert!(
            matches!(result, Err(Stop::Endpoint(IoFault::Timeout))),
            "{result:?}"
        );
        assert_eq!(
            board.sd.commands, 0,
            "no card command after the endpoint stopped"
        );
    }

    #[test]
    fn no_card_in_the_slot_stops_the_session_at_the_card() {
        let mut board = Board::new();
        board.sd.absent = true;
        let mut clock = IterClock::new();
        let (mut buf, mut scratch) = buffers();
        let result = session(
            &mut board,
            &mut clock,
            SimDma::new(vec![]),
            &mut buf,
            &mut scratch,
        );
        assert!(
            matches!(result, Err(Stop::SdHost(InitError::NoResponse))),
            "{result:?}"
        );
    }

    #[test]
    fn a_session_with_no_host_stops_at_the_first_frame_without_a_feed() {
        let mut board = Board::new();
        let mut clock = IterClock::new();
        let (mut buf, mut scratch) = buffers();
        let result = session(
            &mut board,
            &mut clock,
            SimDma::new(vec![]),
            &mut buf,
            &mut scratch,
        );
        assert!(
            matches!(result, Err(Stop::Serve(ServeError::Link(IoError::Timeout)))),
            "{result:?}"
        );
        assert_eq!(
            board.kicks, 2,
            "the arming kick and the session-period kick, nothing more"
        );
    }

    #[test]
    fn feed_is_the_watchdog_kick() {
        let mut regs = HashMap::<usize, u32>::new();
        struct Log(Vec<(usize, u32)>);
        impl RegAccess for Log {
            fn read32(&mut self, _addr: usize) -> u32 {
                0
            }
            fn write32(&mut self, addr: usize, value: u32) {
                self.0.push((addr, value));
            }
        }
        let mut log = Log(Vec::new());
        feed(&mut log);
        assert_eq!(log.0, vec![(WATCHDOG + CRR_OFFSET, KICK)]);
        regs.clear();
    }
}
