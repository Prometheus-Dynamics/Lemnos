//! The probe, run as the image runs it: the step function, the watchdog models, the time CSR and a
//! host that may or may not read the telemetry. Three cases: telemetry present, telemetry absent (the
//! run still returns), and a watchdog that fires earlier than its arithmetic says.

use std::cell::Cell;

use lemnos_k230_usbboot_stage::format::{PACKET, RESULT_OK, reply};
use lemnos_k230_usbboot_stage::image::{SOC_AT_ONCE, WDT0, WDT1};
use lemnos_k230_usbboot_stage::medium::MediumError;
use lemnos_k230_usbboot_stage::probe::{Action, Clock, GRACE_TICKS, Probe, SPIN_DEADLINE};
use lemnos_k230_usbboot_stage::telemetry::{self, PATH_DEADLINE, PATH_SOC, Record};
use lemnos_k230_usbboot_stage::usb_dev::{Event, HwError, UsbHw};
use lemnos_k230_usbboot_stage::wdt::{self, Fault, Model, RegAccess};

/// One tick of the clock is one microsecond (a 1 MHz time CSR).
const STEP_US: u64 = 10_000;
const HZ: u64 = 24_000_000;

struct TimeCsr<'a>(&'a Cell<u64>);

impl Clock for TimeCsr<'_> {
    fn ticks(&self) -> u64 {
        self.0.get()
    }
}

/// The two watchdog models, routed by address.
struct Dogs(Model, Model);

impl RegAccess for Dogs {
    fn read32(&mut self, addr: usize) -> u32 {
        if addr >= wdt::BASE1 {
            self.1.read32(addr)
        } else {
            self.0.read32(addr)
        }
    }

    fn write32(&mut self, addr: usize, value: u32) {
        if addr >= wdt::BASE1 {
            self.1.write32(addr, value)
        } else {
            self.0.write32(addr, value)
        }
    }
}

/// The arming the image's entry does for the flags: TORR, the kick, then enable, per watchdog.
fn arm(dogs: &mut Dogs, flags: u32) {
    if flags & WDT0 != 0 {
        dogs.write32(wdt::BASE0 + wdt::TORR, wdt::torr(11));
        dogs.write32(wdt::BASE0 + wdt::CRR, wdt::KICK);
        dogs.write32(wdt::BASE0 + wdt::CR, wdt::CR_ENABLE);
    }
    if flags & WDT1 != 0 {
        dogs.write32(wdt::BASE1 + wdt::TORR, wdt::torr(10));
        dogs.write32(wdt::BASE1 + wdt::CRR, wdt::KICK);
        dogs.write32(wdt::BASE1 + wdt::CR, wdt::CR_ENABLE);
    }
}

/// A device model whose IN endpoint the host takes from when it listens.
#[derive(Default)]
struct Hw {
    sent: Vec<Vec<u8>>,
    busy: bool,
}

impl UsbHw for Hw {
    fn core_reset(&mut self) -> Result<(), HwError> {
        Ok(())
    }
    fn poll(&mut self) -> Event {
        Event::Idle
    }
    fn read_out(&mut self, _ep: u8, _buf: &mut [u8]) -> Result<usize, HwError> {
        Ok(0)
    }
    fn write_in(&mut self, _ep: u8, data: &[u8]) -> Result<(), HwError> {
        self.sent.push(data.to_vec());
        self.busy = true;
        Ok(())
    }
    fn in_ready(&self, _ep: u8) -> bool {
        !self.busy
    }
    fn stall(&mut self, _ep: u8) {}
    fn set_address(&mut self, _address: u8) {}
}

struct Outcome {
    action: Option<u8>,
    at_ticks: u64,
    records: Vec<Record>,
    /// The watchdog that fired, and the tick it fired at.
    fired: Option<(usize, u64)>,
}

/// Runs a probe in `flags` until it resets, or a watchdog fires first. `listens` says whether the host
/// takes every record it is offered.
fn run(flags: u32, fault: [Fault; 2], listens: bool) -> Outcome {
    let now = Cell::new(0u64);
    let clock = TimeCsr(&now);
    let mut dogs = Dogs(
        Model::new(wdt::BASE0, HZ, fault[0]),
        Model::new(wdt::BASE1, HZ, fault[1]),
    );
    arm(&mut dogs, flags);
    let mut probe = Probe::new(Hw::default(), flags);
    let mut records = Vec::new();
    let mut fired = None;
    let mut taken = 0usize;
    loop {
        now.set(now.get() + STEP_US);
        for (i, dog) in [&mut dogs.0, &mut dogs.1].into_iter().enumerate() {
            if dog.advance_us(STEP_US) && fired.is_none() {
                fired = Some((i, now.get()));
            }
        }
        if let Some(hw) = probe.hw_mut()
            && listens
            && hw.busy
        {
            let packet = hw.sent.last().unwrap().clone();
            records.push(telemetry::decode(&packet).expect("a telemetry record"));
            hw.busy = false;
            taken += 1;
        }
        if fired.is_some() {
            return Outcome {
                action: None,
                at_ticks: now.get(),
                records,
                fired,
            };
        }
        if let Action::Reset(path) = probe.tick(&mut dogs, &clock) {
            // The last record the host takes after the reset decision is still delivered.
            if let Some(hw) = probe.hw_mut()
                && listens
                && hw.busy
            {
                let packet = hw.sent.last().unwrap().clone();
                records.push(telemetry::decode(&packet).expect("a telemetry record"));
            }
            let _ = taken;
            return Outcome {
                action: Some(path),
                at_ticks: now.get(),
                records,
                fired,
            };
        }
    }
}

#[test]
fn with_a_host_listening_the_samples_arrive_and_the_deadline_names_its_path() {
    let outcome = run(0, [Fault::None; 2], true);
    let samples = outcome
        .records
        .iter()
        .filter(|r| matches!(r, Record::Sample { .. }))
        .count();
    assert!(samples >= 2, "{samples} samples");
    assert_eq!(outcome.action, Some(PATH_DEADLINE));
    assert_eq!(
        outcome.records.last(),
        Some(&Record::Taking {
            path: PATH_DEADLINE,
            ticks: outcome
                .records
                .iter()
                .find_map(|r| match r {
                    Record::Taking { ticks, .. } => Some(*ticks),
                    _ => None,
                })
                .unwrap()
        }),
        "the last record names the path it takes"
    );
    // The deadline is the spin's time: the reset comes after it, not before.
    assert!(
        outcome.at_ticks >= SPIN_DEADLINE,
        "reset at {} before the deadline",
        outcome.at_ticks
    );
}

#[test]
fn with_no_host_the_run_still_returns_by_its_path_after_the_grace_time() {
    // Telemetry absent: nothing is read, so nothing is taken. The reset is taken anyway.
    let outcome = run(0, [Fault::None; 2], false);
    assert!(outcome.records.is_empty());
    assert_eq!(outcome.action, Some(PATH_DEADLINE));
    assert!(
        outcome.at_ticks <= SPIN_DEADLINE + GRACE_TICKS + STEP_US * 2,
        "the reset waited for a host: {} ticks",
        outcome.at_ticks
    );
}

#[test]
fn the_soc_at_once_probe_queues_its_record_and_resets_without_a_spin() {
    let outcome = run(SOC_AT_ONCE, [Fault::None; 2], true);
    assert_eq!(outcome.action, Some(PATH_SOC));
    assert!(outcome.at_ticks < SPIN_DEADLINE, "the soc probe spun");
    assert!(
        matches!(outcome.records.last(), Some(Record::Taking { path, .. }) if *path == PATH_SOC)
    );
}

#[test]
fn a_watchdog_that_fires_earlier_than_its_arming_is_seen_in_the_telemetry_against_the_period() {
    // wdt0 fires at three quarters of its period. The reference a host has is the arming: right after
    // the kick, CCVR should read about 2^27 cycles, so the first sample is about one period's worth of
    // cycles less the time elapsed since the arming. A counter that disagrees is the finding.
    let outcome = run(WDT0, [Fault::FiresEarly, Fault::None], true);
    let (index, fired_at) = outcome.fired.expect("wdt0 fires");
    assert_eq!(index, 0);
    let period_us = wdt::period_us(wdt::TOP, HZ);
    assert!(
        fired_at < period_us,
        "fired at {fired_at} us, not early (period {period_us} us)"
    );
    let first = outcome
        .records
        .iter()
        .find_map(|r| match r {
            Record::Sample { ticks, ccvr, .. } => Some((*ticks, ccvr[0])),
            _ => None,
        })
        .expect("a sample before the fire");
    let (first_at, ccvr) = first;
    let expected = wdt::period_cycles(wdt::TOP) - first_at * HZ / 1_000_000;
    assert!(
        u64::from(ccvr) * 10 < expected * 9,
        "the counter reads {ccvr} at {first_at} us; the arming says about {expected}"
    );
    // A healthy counter, the same run without the fault, agrees with the arming.
    let healthy = run(WDT0, [Fault::None; 2], true);
    let (h_at, h_ccvr) = healthy
        .records
        .iter()
        .find_map(|r| match r {
            Record::Sample { ticks, ccvr, .. } => Some((*ticks, ccvr[0])),
            _ => None,
        })
        .unwrap();
    let h_expected = wdt::period_cycles(wdt::TOP) - h_at * HZ / 1_000_000;
    assert!(
        u64::from(h_ccvr) * 100 >= h_expected * 99,
        "{h_ccvr} vs {h_expected}"
    );
}

#[test]
fn the_probe_packets_are_whole_reply_packets() {
    let packet = telemetry::sample(1, [0, 0], [0, 0]);
    assert_eq!(packet.len(), PACKET);
    assert_eq!(packet[2..4], RESULT_OK.to_le_bytes());
    let _ = reply(telemetry::SAMPLE, RESULT_OK, &[]);
    let _: Result<(), MediumError> = Ok(());
}
