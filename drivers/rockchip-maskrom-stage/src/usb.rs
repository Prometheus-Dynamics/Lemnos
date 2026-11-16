//! The DWC3 device controller, as the device side drives it through [`lemnos_flash_stage::usb::Controller`].
//!
//! The controller's own sequences are here: the event buffer (core.c:553-575), DEVTEN (core.h:501-512), the
//! control endpoints (gadget.c:2965-2990), the SETUP TRB re-armed after each setup (ep0.c:280-293), the control
//! data and status TRBs (ep0.c:36, 65, 1072-1082), the bulk pair with its transfer resources (gadget.c:925-948),
//! and the device-address write (ep0.c:598-626, DCFG at core.h:440-446). Every wait is a deadline on the stage's
//! clock. Register access is through [`Bus`]; the controller's memory is through [`DmaMem`] and [`EventMem`].
//!
//! What is not here, and is the bench's to show: a bus reset's own reset of the DWC3 state beyond the events
//! (the stage clears its own arming), the isochronous and stream paths (not used), and the status TRB type on
//! each direction, which follows mainline's two-stage control transfers and is a board check.

use lemnos_flash_stage::usb::{Controller, Event, Speed, UsbError};
use lemnos_flash_stage::wait::{Clock, wait_for};
use lemnos_flash_wire::timing::TRANSFER_BUDGET_US;
use lemnos_flash_wire::usb::{BULK_MAX_PACKET, EP0_MAX_PACKET};

use crate::dwc3link::{Bus, DmaMem, IN_SLOT, OUT_SLOT};
use crate::io::{
    EP_TYPE_BULK, EP_TYPE_CONTROL, IoFault, Trb, configure_endpoint, end_transfer, set_stall,
    start_control_endpoints, start_transfer, until,
};
use crate::regs::dwc3;
use crate::wdt::LONGEST_PERIOD_US;

/// The control endpoints (DWC3 endpoint numbers): OUT is 0, IN is 1.
pub const EP0_OUT: usize = 0;
pub const EP0_IN: usize = 1;
/// The bulk pair (DWC3 endpoint numbers, the same as the bulk endpoints of the wire): OUT 2, IN 3.
pub const BULK_OUT: usize = 2;
pub const BULK_IN: usize = 3;
/// The TRB slots: the bulk OUT and IN of [`crate::dwc3link`], then the SETUP, the control data and the control
/// status TRBs.
pub const SETUP_SLOT: usize = 2;
pub const EP0_DATA_SLOT: usize = 3;
pub const EP0_STATUS_SLOT: usize = 4;

/// The event buffer's size in bytes: 64 events of four bytes.
pub const EVENT_BYTES: usize = 256;

/// TRBCTL values (core.h:874-881).
const TRBCTL_SETUP: u32 = 2;
const TRBCTL_STATUS2: u32 = 3;
const TRBCTL_DATA: u32 = 5;

/// The controller's own memory: the event buffer it writes, the setup packet it writes, and the control data
/// buffer the CPU writes. The -sys crate implements it with volatile access and cache maintenance.
pub trait EventMem {
    /// The runtime address of the event buffer.
    fn event_addr(&self) -> u64;
    /// The event word at byte offset `at` (a multiple of four): invalidated first, since the controller wrote it.
    fn event_word(&mut self, at: usize) -> u32;
    /// The runtime address of the eight-byte setup buffer.
    fn setup_addr(&self) -> u64;
    /// The eight setup bytes the controller wrote (invalidated first).
    fn setup_bytes(&mut self) -> [u8; 8];
    /// The runtime address of the control data buffer.
    fn ep0_addr(&self) -> u64;
    /// The control data buffer, at most one control packet long. [`EventMem::clean_ep0`] makes the CPU's writes visible.
    fn ep0(&mut self) -> &mut [u8];
    /// Makes the CPU's writes to the control data buffer visible to the controller.
    fn clean_ep0(&mut self);
}

fn map(fault: IoFault) -> UsbError {
    match fault {
        IoFault::Timeout => UsbError::Timeout,
        _ => UsbError::Failed,
    }
}

/// The DWC3 device side over `base`, with its DMA memory `M`.
pub struct Dwc3Dev<'b, 'a, C: Clock, M: DmaMem + EventMem> {
    bus: &'b Bus<'a, C>,
    dma: M,
    base: usize,
    event_at: usize,
    setup_armed: bool,
    bulk_len: Option<usize>,
}

impl<'b, 'a, C: Clock, M: DmaMem + EventMem> Dwc3Dev<'b, 'a, C, M> {
    /// Brings the device side up, after the core and device soft resets: the event buffer, the device events,
    /// high speed at address 0, and the control endpoints. The run bit is set by [`Dwc3Dev::run`], last.
    pub fn start(bus: &'b Bus<'a, C>, dma: M, base: usize) -> Result<Self, UsbError> {
        let event_addr = dma.event_addr();
        bus.with(|regs, clock| -> Result<(), UsbError> {
            regs.write32(base + dwc3::GEVNTADRLO, event_addr as u32);
            regs.write32(base + dwc3::GEVNTADRHI, (event_addr >> 32) as u32);
            regs.write32(base + dwc3::GEVNTSIZ, EVENT_BYTES as u32);
            regs.write32(base + dwc3::GEVNTCOUNT, 0);
            regs.write32(
                base + dwc3::DEVTEN,
                dwc3::DEVTEN_DISCONNEVTEN | dwc3::DEVTEN_USBRSTEN | dwc3::DEVTEN_CONNECTDONEEN,
            );
            regs.write32(base + dwc3::DCFG, dwc3::DCFG_HIGHSPEED);
            start_control_endpoints(regs, clock, base).map_err(map)?;
            configure_endpoint(
                regs,
                clock,
                base,
                EP0_OUT,
                EP_TYPE_CONTROL,
                u16::from(EP0_MAX_PACKET),
                0,
            )
            .map_err(map)?;
            configure_endpoint(
                regs,
                clock,
                base,
                EP0_IN,
                EP_TYPE_CONTROL,
                u16::from(EP0_MAX_PACKET),
                0,
            )
            .map_err(map)?;
            Ok(())
        })?;
        Ok(Self {
            bus,
            dma,
            base,
            event_at: 0,
            setup_armed: false,
            bulk_len: None,
        })
    }

    /// Sets the run bit (DCTL.RUN_STOP, core.h:456), the last step of bring-up.
    pub fn run(bus: &Bus<'_, C>, base: usize) {
        bus.with(|regs, _| {
            let value = regs.read32(base + dwc3::DCTL);
            regs.write32(base + dwc3::DCTL, value | dwc3::DCTL_RUN_STOP);
        });
    }

    /// The DMA memory (the tests read what the controller was given through it).
    pub fn dma(&self) -> &M {
        &self.dma
    }

    /// The deadline of a phase that starts now: `budget_us` from now. Every command and every wait of the phase is bounded
    /// by it, so a phase's arm counts against the phase (ruling R3-B1).
    fn phase_deadline(&mut self, budget_us: u64) -> u64 {
        self.bus
            .with(|_, clock| clock.now_us())
            .saturating_add(budget_us)
    }

    fn arm_setup(&mut self, deadline_us: u64) -> Result<(), UsbError> {
        let addr = self.dma.setup_addr();
        self.dma
            .write_trb(SETUP_SLOT, Trb::control(addr, 8, TRBCTL_SETUP));
        self.dma.clean_for_device();
        self.issue(EP0_OUT, SETUP_SLOT, deadline_us)?;
        self.setup_armed = true;
        Ok(())
    }

    /// Starts the transfer on `ep` with the TRB in `slot`; the start command is bounded by `deadline_us`.
    fn issue(&mut self, ep: usize, slot: usize, deadline_us: u64) -> Result<(), UsbError> {
        let trb = self.dma.trb_addr(slot);
        let base = self.base;
        self.bus
            .with(|regs, clock| start_transfer(regs, clock, base, ep, trb, deadline_us))
            .map_err(map)
    }

    /// Waits until the controller hands TRB `slot` back, and returns its remaining byte count, or until `deadline_us`.
    fn await_trb(&mut self, slot: usize, deadline_us: u64) -> Result<u32, UsbError> {
        let dma = &mut self.dma;
        self.bus.with(|_, clock| {
            let budget = until(clock, deadline_us).map_err(map)?;
            wait_for(clock, budget, LONGEST_PERIOD_US, || {
                let trb = dma.read_trb(slot);
                (!trb.owned_by_controller()).then_some(trb.size & 0x00FF_FFFF)
            })
            .map_err(|_| UsbError::Timeout)
        })
    }

    /// One control data packet or zero-length packet on EP0 IN, then its status.
    fn control_in(&mut self, data: &[u8]) -> Result<(), UsbError> {
        if data.len() > usize::from(EP0_MAX_PACKET) {
            return Err(UsbError::Failed);
        }
        self.dma.ep0()[..data.len()].copy_from_slice(data);
        self.dma.clean_ep0();
        let addr = self.dma.ep0_addr();
        self.dma.write_trb(
            EP0_DATA_SLOT,
            Trb::control(addr, data.len() as u32, TRBCTL_DATA),
        );
        self.dma.clean_for_device();
        let deadline = self.phase_deadline(TRANSFER_BUDGET_US);
        self.issue(EP0_IN, EP0_DATA_SLOT, deadline)?;
        self.await_trb(EP0_DATA_SLOT, deadline).map(|_| ())
    }

    /// One zero-length status transfer on `ep`.
    fn status(&mut self, ep: usize) -> Result<(), UsbError> {
        self.dma.write_trb(
            EP0_STATUS_SLOT,
            Trb::control(self.dma.ep0_addr(), 0, TRBCTL_STATUS2),
        );
        self.dma.clean_for_device();
        let deadline = self.phase_deadline(TRANSFER_BUDGET_US);
        self.issue(ep, EP0_STATUS_SLOT, deadline)?;
        self.await_trb(EP0_STATUS_SLOT, deadline).map(|_| ())
    }

    fn setup_completed(&mut self) -> bool {
        !self.dma.read_trb(SETUP_SLOT).owned_by_controller()
    }
}

impl<C: Clock, M: DmaMem + EventMem> Controller for Dwc3Dev<'_, '_, C, M> {
    fn poll(&mut self, deadline_us: u64) -> Event {
        if !self.setup_armed && self.arm_setup(deadline_us).is_err() {
            return Event::None;
        }
        let base = self.base;
        let count = self
            .bus
            .with(|regs, _| regs.read32(base + dwc3::GEVNTCOUNT))
            & 0xFFFF;
        if count < 4 {
            return Event::None;
        }
        let at = self.event_at;
        let word = self.dma.event_word(at);
        self.event_at = (at + 4) % EVENT_BYTES;
        // Acknowledge the one event: the count is in bytes.
        self.bus
            .with(|regs, _| regs.write32(base + dwc3::GEVNTCOUNT, 4));
        if word & 1 == 1 {
            // A device event: the type is bits 8 to 11.
            return match (word >> 8) & 0xF {
                t if t == dwc3::DEVT_USBRST => {
                    // A bus reset ends whatever is armed on the bulk pair (its TRB is abandoned), and the setup TRB
                    // is armed again afterwards. Bench check: the end-transfer command's resource id (zero here).
                    if self.bulk_len.is_some() {
                        let deadline = self.phase_deadline(TRANSFER_BUDGET_US);
                        self.bus.with(|regs, clock| {
                            let _ = end_transfer(regs, clock, base, BULK_OUT, deadline);
                        });
                    }
                    self.setup_armed = false;
                    self.bulk_len = None;
                    self.bus.with(|regs, _| {
                        let dalepena = regs.read32(base + dwc3::DALEPENA);
                        regs.write32(base + dwc3::DALEPENA, dalepena & !0b1100);
                    });
                    Event::BusReset
                }
                t if t == dwc3::DEVT_DISCONNECT => Event::Disconnect,
                _ => Event::None,
            };
        }
        // An endpoint event: the number is bits 1 to 5, the event bits 6 to 9. Only the setup TRB's own
        // completion is a SETUP. Other completions on EP0 (the data and status stages) are the ones the
        // transfers in progress already awaited; the setup TRB is still owned while they are queued.
        let ep = ((word >> 1) & 0x1F) as usize;
        let event = (word >> 6) & 0xF;
        if ep == EP0_OUT
            && event == dwc3::DEPEVT_XFERCOMPLETE
            && self.setup_armed
            && self.setup_completed()
        {
            self.setup_armed = false;
            return Event::Setup(self.dma.setup_bytes());
        }
        Event::None
    }

    fn speed(&mut self) -> Speed {
        // DSTS.CONNECTSPD (bits 0 to 2, core.h:537-542): 0 is high speed, 1 full speed, 4 and 5 super speed.
        let base = self.base;
        let dsts = self.bus.with(|regs, _| regs.read32(base + dwc3::DSTS));
        match dsts & 0x7 {
            0 => Speed::High,
            1 => Speed::Full,
            4 | 5 => Speed::Super,
            _ => Speed::Unknown,
        }
    }

    fn address_before_status(&self) -> bool {
        // DWC3 writes the address (DCFG.DEVADDR) in the set-address handler, before the status stage is queued
        // (mainline ep0.c:598-626, DCFG at core.h:440-446). Bench check: the controller takes it from the next token.
        true
    }

    fn set_address(&mut self, address: u8) -> Result<(), UsbError> {
        let base = self.base;
        self.bus.with(|regs, _| {
            let dcfg = regs.read32(base + dwc3::DCFG) & !dwc3::DCFG_DEVADDR_MASK;
            regs.write32(
                base + dwc3::DCFG,
                dcfg | (u32::from(address) << dwc3::DCFG_DEVADDR_SHIFT),
            );
        });
        Ok(())
    }

    fn set_configured(&mut self, on: bool) -> Result<(), UsbError> {
        let base = self.base;
        let packet = BULK_MAX_PACKET;
        self.bus.with(|regs, clock| -> Result<(), UsbError> {
            if on {
                configure_endpoint(regs, clock, base, BULK_OUT, EP_TYPE_BULK, packet, 0)
                    .map_err(map)?;
                configure_endpoint(regs, clock, base, BULK_IN, EP_TYPE_BULK, packet, 0)
                    .map_err(map)?;
            } else {
                let dalepena = regs.read32(base + dwc3::DALEPENA);
                regs.write32(base + dwc3::DALEPENA, dalepena & !0b1100);
            }
            Ok(())
        })?;
        self.bulk_len = None;
        Ok(())
    }

    fn ep0_in(&mut self, data: &[u8]) -> Result<(), UsbError> {
        self.control_in(data)
    }

    fn ep0_in_status(&mut self) -> Result<(), UsbError> {
        self.status(EP0_IN)
    }

    fn ep0_out_status(&mut self) -> Result<(), UsbError> {
        self.status(EP0_OUT)
    }

    fn ep0_stall(&mut self) {
        let base = self.base;
        self.bus.with(|regs, clock| {
            let _ = set_stall(regs, clock, base, EP0_OUT);
            let _ = set_stall(regs, clock, base, EP0_IN);
        });
    }

    fn bulk_arm(&mut self, len: usize, deadline_us: u64) -> Result<(), UsbError> {
        if len == 0 || len > self.dma.rx().len() {
            return Err(UsbError::Failed);
        }
        let addr = self.dma.rx_addr();
        self.dma.write_trb(OUT_SLOT, Trb::normal(addr, len as u32));
        self.dma.clean_for_device();
        self.issue(BULK_OUT, OUT_SLOT, deadline_us)?;
        self.bulk_len = Some(len);
        Ok(())
    }

    fn bulk_poll(&mut self, out: &mut [u8]) -> Result<Option<usize>, UsbError> {
        let Some(len) = self.bulk_len else {
            return Err(UsbError::Failed);
        };
        let trb = self.dma.read_trb(OUT_SLOT);
        if trb.owned_by_controller() {
            return Ok(None);
        }
        let remaining = (trb.size & 0x00FF_FFFF) as usize;
        self.dma.invalidate_rx();
        let n = len - remaining;
        let n_copy = n.min(out.len());
        out[..n_copy].copy_from_slice(&self.dma.rx()[..n_copy]);
        self.bulk_len = None;
        Ok(Some(n))
    }

    fn bulk_send(&mut self, data: &[u8], budget_us: u64) -> Result<(), UsbError> {
        let deadline = self.phase_deadline(budget_us);
        if data.is_empty() || data.len() > self.dma.tx().len() {
            return Err(UsbError::Failed);
        }
        self.dma.tx()[..data.len()].copy_from_slice(data);
        let addr = self.dma.tx_addr();
        self.dma
            .write_trb(IN_SLOT, Trb::normal(addr, data.len() as u32));
        self.dma.clean_for_device();
        self.issue(BULK_IN, IN_SLOT, deadline)?;
        if self.await_trb(IN_SLOT, deadline)? != 0 {
            return Err(UsbError::Failed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    /// A deadline the tests never reach: every pass below completes before it.
    const TEST_DEADLINE: u64 = 1_000_000;

    use super::*;
    use crate::bind::RegAccess;
    use crate::regs::dwc3;
    use lemnos_flash_stage::wait::IterClock;
    use std::collections::HashMap;

    /// A register file: reads return what was written, except that a command's active bit clears at once.
    /// Every write is logged in order.
    #[derive(Default)]
    struct Regs {
        values: HashMap<usize, u32>,
        log: Vec<(usize, u32)>,
    }

    impl RegAccess for Regs {
        fn read32(&mut self, addr: usize) -> u32 {
            let value = self.values.get(&addr).copied().unwrap_or(0);
            value & !dwc3::DEPCMD_CMDACT
        }

        fn write32(&mut self, addr: usize, value: u32) {
            self.values.insert(addr, value);
            self.log.push((addr, value));
        }
    }

    /// The DMA memory: TRBs that the controller hands back at once, and buffers the tests fill.
    struct Mem {
        trbs: [Trb; 8],
        events: [u32; EVENT_BYTES / 4],
        setup: [u8; 8],
        ep0: Vec<u8>,
        rx: Vec<u8>,
        tx: Vec<u8>,
    }

    impl Mem {
        fn new() -> Self {
            Self {
                trbs: [Trb::normal(0, 0); 8],
                events: [0; EVENT_BYTES / 4],
                setup: [0; 8],
                ep0: vec![0; 64],
                rx: vec![0; 4096],
                tx: vec![0; 4096],
            }
        }
    }

    impl DmaMem for Mem {
        fn rx_addr(&self) -> u64 {
            0x2000
        }
        fn tx_addr(&self) -> u64 {
            0x3000
        }
        fn trb_addr(&self, slot: usize) -> u64 {
            0x1000 + (slot as u64) * 16
        }
        fn write_trb(&mut self, slot: usize, trb: Trb) {
            self.trbs[slot] = trb;
        }
        fn read_trb(&mut self, slot: usize) -> Trb {
            // The controller has completed every transfer by the time the CPU looks.
            let mut trb = self.trbs[slot];
            trb.ctrl &= !TRB_HWO_BIT;
            trb
        }
        fn rx(&mut self) -> &mut [u8] {
            &mut self.rx
        }
        fn tx(&mut self) -> &mut [u8] {
            &mut self.tx
        }
        fn clean_for_device(&mut self) {}
        fn invalidate_rx(&mut self) {}
    }

    const TRB_HWO_BIT: u32 = 1;

    impl EventMem for Mem {
        fn event_addr(&self) -> u64 {
            0x4000
        }
        fn event_word(&mut self, at: usize) -> u32 {
            self.events[at / 4]
        }
        fn setup_addr(&self) -> u64 {
            0x5000
        }
        fn setup_bytes(&mut self) -> [u8; 8] {
            self.setup
        }
        fn ep0_addr(&self) -> u64 {
            0x6000
        }
        fn ep0(&mut self) -> &mut [u8] {
            &mut self.ep0
        }
        fn clean_ep0(&mut self) {}
    }

    /// The clock and the memory a device side needs for a test.
    fn started(_regs: &mut Regs) -> (IterClock, Mem) {
        (IterClock::new(), Mem::new())
    }

    /// The position of the first write matching `pred`, in the log.
    fn first(log: &[(usize, u32)], pred: impl Fn(usize, u32) -> bool) -> Option<usize> {
        log.iter().position(|&(a, v)| pred(a, v))
    }

    /// The absolute address of a DWC3 register at `offset` on the test block.
    fn reg(offset: usize) -> usize {
        DWC3_BASE_FOR_TESTS + offset
    }

    fn depcmd_write(ep: usize, command: u32) -> impl Fn(usize, u32) -> bool {
        move |addr, value| addr == reg(dwc3::depcmd(ep)) && value & 0xF == command
    }

    #[test]
    fn bring_up_writes_the_event_buffer_and_device_events_before_the_control_endpoints() {
        let mut regs = Regs::default();
        let (mut clock, mem) = started(&mut regs);
        let bus = Bus::new(&mut regs, &mut clock);
        let dev = Dwc3Dev::start(&bus, mem, DWC3_BASE_FOR_TESTS).unwrap();
        drop(dev);
        let log = &regs.log;
        let events = first(log, |a, _| a == reg(dwc3::GEVNTADRLO)).unwrap();
        let devten = first(log, |a, _| a == reg(dwc3::DEVTEN)).unwrap();
        let start_cfg = first(log, depcmd_write(0, dwc3::DEPCMD_DEPSTARTCFG)).unwrap();
        assert!(
            events < devten && devten < start_cfg,
            "events, then device events, then the endpoints"
        );
    }

    #[test]
    fn each_endpoint_gets_its_resource_after_its_configuration_and_before_it_is_enabled() {
        let mut regs = Regs::default();
        let (mut clock, mem) = started(&mut regs);
        let bus = Bus::new(&mut regs, &mut clock);
        let mut dev = Dwc3Dev::start(&bus, mem, DWC3_BASE_FOR_TESTS).unwrap();
        dev.set_configured(true).unwrap();
        drop(dev);
        let log = &regs.log;
        for ep in [BULK_OUT, BULK_IN] {
            let cfg = first(log, depcmd_write(ep, dwc3::DEPCMD_SETEPCONFIG)).unwrap();
            let res = first(log, depcmd_write(ep, dwc3::DEPCMD_SETTRANSFRESOURCE)).unwrap();
            assert!(cfg < res, "endpoint {ep}: configured before its resource");
            assert_eq!(
                log[res - 1],
                (reg(dwc3::depcmdpar0(ep)), 1),
                "one transfer resource"
            );
        }
        let enabled = log
            .iter()
            .rev()
            .find(|(a, _)| *a == reg(dwc3::DALEPENA))
            .unwrap()
            .1;
        assert_eq!(
            enabled & 0b1100,
            0b1100,
            "both bulk endpoints are enabled in DALEPENA"
        );
    }

    #[test]
    fn a_transfer_on_a_bulk_endpoint_is_started_only_after_its_resource() {
        let mut regs = Regs::default();
        let (mut clock, mem) = started(&mut regs);
        let bus = Bus::new(&mut regs, &mut clock);
        let mut dev = Dwc3Dev::start(&bus, mem, DWC3_BASE_FOR_TESTS).unwrap();
        dev.set_configured(true).unwrap();
        dev.bulk_arm(12, TEST_DEADLINE).unwrap();
        drop(dev);
        let log = &regs.log;
        let res = first(log, depcmd_write(BULK_OUT, dwc3::DEPCMD_SETTRANSFRESOURCE)).unwrap();
        let start = first(log, depcmd_write(BULK_OUT, dwc3::DEPCMD_STARTTRANSFER)).unwrap();
        assert!(res < start);
    }

    #[test]
    fn a_bus_reset_event_is_decoded_and_the_setup_is_armed_again_after_it() {
        let mut regs = Regs::default();
        let (mut clock, mut mem) = started(&mut regs);
        // A device event: bit 0 set, type USB reset (1) in bits 8 to 11.
        mem.events[0] = 1 | (dwc3::DEVT_USBRST << 8);
        let bus = Bus::new(&mut regs, &mut clock);
        let mut dev = Dwc3Dev::start(&bus, mem, DWC3_BASE_FOR_TESTS).unwrap();
        // The count the controller reports: one event, four bytes.
        bus.with(|regs, _| regs.write32(DWC3_BASE_FOR_TESTS + dwc3::GEVNTCOUNT, 4));
        assert_eq!(dev.poll(TEST_DEADLINE), Event::BusReset);
        drop(dev);
        assert!(
            regs.log
                .iter()
                .any(|(a, v)| *a == reg(dwc3::DCFG) && *v == dwc3::DCFG_HIGHSPEED),
            "the start wrote DCFG"
        );
    }

    #[test]
    fn a_bus_reset_with_a_bulk_transfer_armed_ends_that_transfer_first() {
        let mut regs = Regs::default();
        let (mut clock, mut mem) = started(&mut regs);
        mem.events[0] = 1 | (dwc3::DEVT_USBRST << 8);
        let bus = Bus::new(&mut regs, &mut clock);
        let mut dev = Dwc3Dev::start(&bus, mem, DWC3_BASE_FOR_TESTS).unwrap();
        dev.set_configured(true).unwrap();
        dev.bulk_arm(12, TEST_DEADLINE).unwrap();
        bus.with(|regs, _| regs.write32(reg(dwc3::GEVNTCOUNT), 4));
        assert_eq!(dev.poll(TEST_DEADLINE), Event::BusReset);
        drop(dev);
        let log = &regs.log;
        let armed = first(log, depcmd_write(BULK_OUT, dwc3::DEPCMD_STARTTRANSFER)).unwrap();
        let ended = first(log, depcmd_write(BULK_OUT, dwc3::DEPCMD_ENDTRANSFER)).unwrap();
        assert!(
            armed < ended,
            "the transfer was started, then ended by the reset"
        );
    }

    #[test]
    fn a_setup_completion_on_control_out_is_read_as_the_setup_packet() {
        let mut regs = Regs::default();
        let (mut clock, mut mem) = started(&mut regs);
        // An endpoint event: bit 0 clear, endpoint 0 in bits 1 to 5, XferComplete (1) in bits 6 to 9.
        mem.events[0] = 1 << 6;
        mem.setup = [0x80, 6, 0, 1, 0, 0, 18, 0];
        let bus = Bus::new(&mut regs, &mut clock);
        let mut dev = Dwc3Dev::start(&bus, mem, DWC3_BASE_FOR_TESTS).unwrap();
        bus.with(|regs, _| regs.write32(DWC3_BASE_FOR_TESTS + dwc3::GEVNTCOUNT, 4));
        assert_eq!(
            dev.poll(TEST_DEADLINE),
            Event::Setup([0x80, 6, 0, 1, 0, 0, 18, 0])
        );
        drop(dev);
        assert!(
            regs.log
                .iter()
                .any(|(a, _)| *a == reg(dwc3::depcmd(EP0_OUT))),
            "the setup TRB was started"
        );
    }

    /// The controller base the tests use: the DWC3 block's address in the board's map. The fake register file
    /// ignores what it is, so any value works; this is the board's value, for the reader's reference.
    const DWC3_BASE_FOR_TESTS: usize = crate::bind::DWC3;
}
