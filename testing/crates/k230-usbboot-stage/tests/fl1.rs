//! The K230 FL1 stage's logic, tested in simulation: the entry's order and its arming, the report, the
//! storage adapter, the device controller over a scripted host, and the rehearsal of the board script
//! (a host that enumerates the device, identifies it, and ends the session) against the same code the
//! board runs. The watchdog is the register model in `wdt::Model`, which has a fault for each way the
//! block can disagree with its arming; a stand-in that did not fail would not prove the entry.

use lemnos_flash_stage::guard::{Cause, StartError};
use lemnos_flash_stage::link::{IoError, Storage};
use lemnos_flash_stage::serve::{BUFFER_LEN, ServeError};
use lemnos_flash_stage::usb::{Controller, Event, UsbError};
use lemnos_flash_stage::wait::Clock;
use lemnos_flash_wire::frame::{Request, ResetKind};
use lemnos_flash_wire::identity::IDENTITY_LEN;
use lemnos_k230_usbboot_stage::fl1::IDENTITY;
use lemnos_k230_usbboot_stage::fl1::report::{Uart, describe};
use lemnos_k230_usbboot_stage::fl1::run::stage;
use lemnos_k230_usbboot_stage::fl1::storage::{Blocks, Sectors};
use lemnos_k230_usbboot_stage::fl1::usb::{BULK_MAX, Device};
use lemnos_k230_usbboot_stage::sd::BLOCK;
use lemnos_k230_usbboot_stage::sysctl::{SOC_GLB_RST, SOC_RESET_VALUE};
use lemnos_k230_usbboot_stage::usb_dev::{
    BULK_IN, BULK_OUT, EP0, Event as HwEvent, HwError, UsbHw,
};
use lemnos_k230_usbboot_stage::wdt::{BASES, CLOCK_HZ_MIN, CRR, Fault, Model, RegAccess};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

/// One microsecond per clock read would be too slow for a 30 s idle deadline; a millisecond per read
/// reaches the deadline in some thousands of polls.
const STEP_US: u64 = 1_000;

/// A clock shared by every holder, so the session and the adapters read one time.
#[derive(Clone)]
struct Tick(Rc<Cell<u64>>);

impl Tick {
    fn new() -> Self {
        Self(Rc::new(Cell::new(0)))
    }
}

impl Clock for Tick {
    fn now_us(&mut self) -> u64 {
        let now = self.0.get() + STEP_US;
        self.0.set(now);
        now
    }
}

/// Two watchdogs on one bus, routed by base.
struct Bus {
    a: Model,
    b: Model,
    writes: Rc<RefCell<Vec<(usize, u32)>>>,
}

impl Bus {
    fn new(fault: Fault) -> Self {
        Self {
            a: Model::new(BASES[0], CLOCK_HZ_MIN, fault),
            b: Model::new(BASES[1], CLOCK_HZ_MIN, fault),
            writes: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

impl RegAccess for Bus {
    fn read32(&mut self, addr: usize) -> u32 {
        if addr >= BASES[1] {
            self.b.read32(addr)
        } else {
            self.a.read32(addr)
        }
    }

    fn write32(&mut self, addr: usize, value: u32) {
        self.writes.borrow_mut().push((addr, value));
        if addr >= BASES[1] {
            self.b.write32(addr, value)
        } else {
            self.a.write32(addr, value)
        }
    }
}

/// The host's side of the device controller: a script of what the host does next, and what the
/// device writes back.
#[derive(Clone)]
enum Step {
    Setup([u8; 8]),
    Out(u8, Vec<u8>),
}

/// What the device did, read by the test after the stage has run (the host moves into the stage).
#[derive(Default)]
struct Log {
    writes: Vec<(u8, Vec<u8>)>,
    addresses: Vec<u8>,
    stalls: u32,
}

#[derive(Default)]
struct Host {
    script: VecDeque<Step>,
    frame: Vec<u8>,
    log: Rc<RefCell<Log>>,
}

impl UsbHw for Host {
    fn core_reset(&mut self) -> Result<(), HwError> {
        Ok(())
    }

    fn poll(&mut self) -> HwEvent {
        match self.script.pop_front() {
            None => HwEvent::Idle,
            Some(Step::Setup(setup)) => HwEvent::Setup(setup),
            Some(Step::Out(ep, bytes)) => {
                let len = bytes.len();
                self.frame = bytes;
                HwEvent::Out { ep, len }
            }
        }
    }

    fn read_out(&mut self, _ep: u8, buf: &mut [u8]) -> Result<usize, HwError> {
        let n = self.frame.len().min(buf.len());
        buf[..n].copy_from_slice(&self.frame[..n]);
        Ok(n)
    }

    fn write_in(&mut self, ep: u8, data: &[u8]) -> Result<(), HwError> {
        self.log.borrow_mut().writes.push((ep, data.to_vec()));
        Ok(())
    }

    fn in_ready(&self, _ep: u8) -> bool {
        true
    }

    fn stall(&mut self, _ep: u8) {
        self.log.borrow_mut().stalls += 1;
    }

    fn set_address(&mut self, address: u8) {
        self.log.borrow_mut().addresses.push(address);
    }
}

const GET_DEVICE: [u8; 8] = [0x80, 6, 0, 1, 0, 0, 18, 0];
const SET_ADDRESS_5: [u8; 8] = [0, 5, 5, 0, 0, 0, 0, 0];
const SET_CONFIGURATION_1: [u8; 8] = [0, 9, 1, 0, 0, 0, 0, 0];

fn identify_frame() -> Vec<u8> {
    Request::identify().encode().to_vec()
}

fn end_frame() -> Vec<u8> {
    Request::end(ResetKind::Normal).encode().to_vec()
}

/// A RAM card of `sectors` blocks, with an optional sector that fails.
struct Ram {
    blocks: Vec<[u8; BLOCK]>,
    fail_at: Option<u64>,
}

impl Ram {
    fn new(sectors: usize) -> Self {
        Self {
            blocks: vec![[0; BLOCK]; sectors],
            fail_at: None,
        }
    }
}

impl Blocks for Ram {
    fn read_block(&mut self, lba: u64, out: &mut [u8; BLOCK]) -> Result<(), IoError> {
        if self.fail_at == Some(lba) {
            return Err(IoError::Failed);
        }
        *out = *self.blocks.get(lba as usize).ok_or(IoError::Failed)?;
        Ok(())
    }

    fn write_block(&mut self, lba: u64, data: &[u8; BLOCK]) -> Result<(), IoError> {
        if self.fail_at == Some(lba) {
            return Err(IoError::Failed);
        }
        *self.blocks.get_mut(lba as usize).ok_or(IoError::Failed)? = *data;
        Ok(())
    }
}

type Dev = Device<Host, Tick>;
type Store = Sectors<Ram, Tick>;

/// Runs the entry against `host` and a healthy or faulty watchdog bus, and reports whether the
/// bring-up ran. The bring-up is the only place the controllers are made.
fn run_stage(fault: Fault, host: Host, clock: &mut Tick) -> (Result<ResetKind, Cause>, bool, Bus) {
    let mut regs = Bus::new(fault);
    let mut buf = vec![0u8; BUFFER_LEN];
    let mut scratch = vec![0u8; BUFFER_LEN];
    let brought_up = Cell::new(false);
    let host = RefCell::new(Some(host));
    let outcome = stage(&mut regs, clock, &mut buf, &mut scratch, |_, clock| {
        brought_up.set(true);
        let host = host.borrow_mut().take().expect("bring-up runs once");
        let dev: Dev = Device::new(host, clock.clone());
        let store: Store = Sectors::new(Ram::new(64), clock.clone());
        Ok((dev, store))
    });
    (outcome, brought_up.get(), regs)
}

#[test]
fn a_healthy_stage_identifies_then_ends_and_the_identity_is_the_wires() {
    // The rehearsal of the board script in simulation: enumerate, identify, end.
    let log = Rc::new(RefCell::new(Log::default()));
    let host = Host {
        script: VecDeque::from(vec![
            Step::Setup(GET_DEVICE),
            Step::Out(EP0, Vec::new()),
            Step::Setup(SET_ADDRESS_5),
            Step::Setup(SET_CONFIGURATION_1),
            Step::Out(BULK_OUT, identify_frame()),
            Step::Out(BULK_OUT, end_frame()),
        ]),
        frame: Vec::new(),
        log: log.clone(),
    };
    let mut clock = Tick::new();
    let (outcome, brought_up, _) = run_stage(Fault::None, host, &mut clock);
    assert_eq!(outcome, Ok(ResetKind::Normal));
    assert!(brought_up);
    let log = log.borrow();
    assert_eq!(
        log.addresses,
        vec![5],
        "SET_ADDRESS 5 reaches the controller once"
    );
    let identity = log
        .writes
        .iter()
        .find(|(ep, _)| *ep == BULK_IN)
        .expect("the identify is answered on the bulk IN endpoint");
    assert_eq!(
        identity.1,
        IDENTITY.to_vec(),
        "the answer is the stage's identity record"
    );
    let (_, end_reply) = log.writes.last().expect("the end is answered");
    assert_eq!(end_reply, &vec![0u8], "END is answered with status ok");
}

#[test]
fn a_watchdog_whose_control_register_ignores_writes_stops_before_any_bring_up() {
    // The stand-in fails as the block does: CR writes are dropped, so the arming does not take.
    let mut clock = Tick::new();
    let (outcome, brought_up, _) = run_stage(Fault::CrIgnored, Host::default(), &mut clock);
    assert_eq!(outcome.err(), Some(Cause::Start(StartError::NotArmed)));
    assert!(
        !brought_up,
        "no USB or SD bring-up may run before the arming reads back"
    );
}

#[test]
fn a_watchdog_whose_counter_is_frozen_stops_before_any_bring_up() {
    let mut clock = Tick::new();
    let (outcome, brought_up, _) = run_stage(Fault::CounterStuck, Host::default(), &mut clock);
    assert_eq!(outcome.err(), Some(Cause::Start(StartError::NotArmed)));
    assert!(!brought_up);
}

#[test]
fn a_host_that_never_configures_the_device_ends_at_the_enumeration_limit() {
    // Idle forever: the session's first deadline is 30 s from the arm, and the stage reports it as
    // a bring-up stop, which the entry resets on.
    let mut clock = Tick::new();
    let (outcome, brought_up, _) = run_stage(Fault::None, Host::default(), &mut clock);
    assert!(brought_up);
    assert_eq!(outcome.err(), Some(Cause::Setup { step: 2 }));
}

#[test]
fn an_identified_host_that_goes_quiet_ends_at_the_idle_limit_and_the_report_resets_last() {
    // The return path in simulation: the session ends by its own idle outcome (not a watchdog
    // reset), the report prints the cause, and the SoC reset is the last write to the bus.
    let host = Host {
        script: VecDeque::from(vec![
            Step::Setup(GET_DEVICE),
            Step::Out(EP0, Vec::new()),
            Step::Setup(SET_ADDRESS_5),
            Step::Setup(SET_CONFIGURATION_1),
            Step::Out(BULK_OUT, identify_frame()),
        ]),
        ..Host::default()
    };
    let mut clock = Tick::new();
    let (outcome, brought_up, mut regs) = run_stage(Fault::None, host, &mut clock);
    assert!(brought_up);
    assert_eq!(outcome.err(), Some(Cause::Serve(ServeError::Idle)));
    let mut report = Uart::new(&mut regs);
    lemnos_flash_stage::guard::Report::cause(&mut report, outcome.err().unwrap());
    let last = *regs.writes.borrow().last().expect("the report writes");
    assert_eq!(last, (SOC_GLB_RST, SOC_RESET_VALUE));
}

#[test]
fn a_status_wait_past_its_deadline_is_a_timeout_not_a_hang() {
    let mut clock = Tick::new();
    let mut dev: Dev = Device::new(Host::default(), clock.clone());
    let deadline = clock.now_us() + 5 * STEP_US;
    assert_eq!(dev.ep0_out_status(deadline), Err(UsbError::Timeout));
}

#[test]
fn a_setup_during_a_status_wait_aborts_the_request_and_is_kept_for_the_next_poll() {
    let mut clock = Tick::new();
    let host = Host {
        script: VecDeque::from(vec![Step::Setup(GET_DEVICE)]),
        ..Host::default()
    };
    let mut dev: Dev = Device::new(host, clock.clone());
    let deadline = clock.now_us() + 1_000 * STEP_US;
    assert_eq!(dev.ep0_out_status(deadline), Err(UsbError::Reset));
    assert_eq!(dev.poll(deadline), Event::Setup(GET_DEVICE));
}

#[test]
fn a_bulk_frame_is_taken_once_with_its_length_and_an_oversize_frame_is_refused() {
    let mut clock = Tick::new();
    let host = Host {
        script: VecDeque::from(vec![
            Step::Out(BULK_OUT, vec![1, 2, 3, 4, 5]),
            Step::Out(BULK_OUT, vec![0; 9]),
        ]),
        ..Host::default()
    };
    let mut dev: Dev = Device::new(host, clock.clone());
    let deadline = clock.now_us() + 1_000 * STEP_US;
    dev.bulk_arm(8, deadline).unwrap();
    assert_eq!(dev.poll(deadline), Event::None);
    let mut out = [0u8; 8];
    assert_eq!(dev.bulk_poll(&mut out), Ok(Some(5)));
    assert_eq!(&out[..5], &[1, 2, 3, 4, 5]);
    assert_eq!(dev.bulk_poll(&mut out), Ok(None), "a frame is taken once");
    assert_eq!(dev.poll(deadline), Event::None);
    assert_eq!(
        dev.bulk_poll(&mut out[..8]),
        Err(UsbError::Failed),
        "9 bytes into 8 is refused"
    );
}

#[test]
fn an_arm_longer_than_the_frame_cap_is_refused_before_the_hardware_is_asked() {
    let mut clock = Tick::new();
    let mut dev: Dev = Device::new(Host::default(), clock.clone());
    let deadline = clock.now_us() + 1_000 * STEP_US;
    assert_eq!(dev.bulk_arm(BULK_MAX + 1, deadline), Err(UsbError::Failed));
    assert!(dev.bulk_arm(BULK_MAX, deadline).is_ok());
}

#[test]
fn a_stall_is_taken_by_the_hardware_and_the_speed_is_high() {
    let mut clock = Tick::new();
    let mut dev: Dev = Device::new(Host::default(), clock.clone());
    let deadline = clock.now_us() + 1_000 * STEP_US;
    dev.ep0_stall(deadline).unwrap();
    assert_eq!(dev.speed(), lemnos_flash_stage::usb::Speed::High);
}

/// A card that writes and reads back by sector, with the budget checked before each.
#[test]
fn sectors_are_written_and_read_back_by_lba() {
    let clock = Tick::new();
    let mut store: Store = Sectors::new(Ram::new(64), clock.clone());
    let data: Vec<u8> = (0..2 * BLOCK).map(|i| (i % 251) as u8).collect();
    store.write(10, &data, 1_000 * STEP_US).unwrap();
    let mut back = vec![0u8; 2 * BLOCK];
    store.read(10, &mut back, 1_000 * STEP_US).unwrap();
    assert_eq!(back, data);
}

#[test]
fn a_write_that_is_not_whole_sectors_is_refused_and_writes_nothing() {
    let clock = Tick::new();
    let mut store: Store = Sectors::new(Ram::new(64), clock.clone());
    assert_eq!(
        store.write(10, &[1; BLOCK + 1], 1_000 * STEP_US),
        Err(IoError::Failed)
    );
    let mut back = vec![0u8; BLOCK];
    store.read(10, &mut back, 1_000 * STEP_US).unwrap();
    assert!(back.iter().all(|&b| b == 0));
}

#[test]
fn a_budget_spent_before_the_first_sector_is_a_storage_timeout_and_writes_nothing() {
    let clock = Tick::new();
    let mut store: Store = Sectors::new(Ram::new(64), clock.clone());
    assert_eq!(store.write(10, &[7; BLOCK], STEP_US), Err(IoError::Timeout));
    let mut back = vec![0u8; BLOCK];
    store.read(10, &mut back, 1_000 * STEP_US).unwrap();
    assert!(
        back.iter().all(|&b| b == 0),
        "nothing is written before the budget check passes"
    );
}

#[test]
fn a_card_fault_part_way_is_a_storage_failure_with_the_earlier_sectors_written() {
    let clock = Tick::new();
    let mut ram = Ram::new(64);
    ram.fail_at = Some(11);
    let mut store: Store = Sectors::new(ram, clock.clone());
    let data = vec![9u8; 2 * BLOCK];
    assert_eq!(
        store.write(10, &data, 1_000 * STEP_US),
        Err(IoError::Failed)
    );
    let mut back = vec![0u8; BLOCK];
    store.read(10, &mut back, 1_000 * STEP_US).unwrap();
    assert!(
        back.iter().all(|&b| b == 9),
        "sector 10 was written before sector 11 failed"
    );
}

#[test]
fn the_report_prints_the_cause_then_resets_and_feeds_nothing() {
    let regs = Bus::new(Fault::None);
    let writes = regs.writes.clone();
    let mut report = Uart::new(regs);
    lemnos_flash_stage::guard::Report::cause(&mut report, Cause::Serve(ServeError::Idle));
    let log = writes.borrow();
    let last = *log.last().expect("the report writes");
    assert_eq!(
        last,
        (SOC_GLB_RST, SOC_RESET_VALUE),
        "the SoC reset is the last write"
    );
    for base in BASES {
        assert!(
            !log.iter().any(|&(addr, _)| addr == base + CRR),
            "no watchdog kick after or during the report"
        );
    }
    assert!(describe(Cause::Serve(ServeError::Idle)).contains("idle"));
    assert_eq!(IDENTITY.len(), IDENTITY_LEN);
}

#[test]
fn the_identity_record_carries_the_k230_regions_and_build() {
    let peer = lemnos_flash_wire::identity::check(&IDENTITY).expect("the identity checks");
    assert_eq!(peer.regions(), &lemnos_k230_usbboot_stage::fl1::REGIONS[..]);
}
