//! The probe: what the image does for a return-path run (the stage image in a probe mode). It samples the
//! watchdogs and the time CSR as telemetry while a host listens, takes the deadline or the SoC reset
//! when its time is up, and never waits for a host: a record that cannot be sent is dropped after a
//! grace time, and the reset is taken all the same.
//!
//! Written as a step function ([`Probe::tick`]) so the simulator and the tests run exactly what the
//! image runs. The image calls it in a loop with the time CSR and the watchdogs' registers.
//!
//! The arming is not here: the image's entry arms the watchdogs its mode names, reads them back, and
//! only then calls the probe. A probe never feeds a watchdog (there is no feed in a probe), so each
//! watchdog's own period, and the spin's deadline, are the only ways the board returns.

use crate::format::PACKET;
use crate::image::{SOC_AT_ONCE, WDT0, WDT1};
use crate::stage::NoReply;
use crate::stage::Reboot;
use crate::telemetry::{self, PATH_DEADLINE, PATH_SOC};
use crate::usb_dev::{BulkEndpoints, Device, Step, UsbHw};
use crate::wdt::{self, RegAccess};

/// The time between samples, in time-CSR ticks (1 MHz assumed; the host measures the real rate).
pub const SAMPLE_TICKS: u64 = 250_000;
/// How long a "taking" record gets to reach a listening host before the reset is taken anyway.
pub const GRACE_TICKS: u64 = 200_000;
/// The spin's deadline, in ticks: the same bound the entry's spin uses.
pub const SPIN_DEADLINE: u64 = wdt::SPIN_DEADLINE_TICKS;

/// The time CSR, as the image reads it.
pub trait Clock {
    /// The time CSR's value, in ticks.
    fn ticks(&self) -> u64;
}

/// What one tick asks the image to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Keep going.
    Continue,
    /// Take the SoC reset now. The path is the one named in the last telemetry record.
    Reset(u8),
}

const SLOTS: usize = 4;

/// The records waiting for a host to read them.
struct Queue {
    slots: [[u8; PACKET]; SLOTS],
    head: usize,
    len: usize,
}

impl Queue {
    fn new() -> Self {
        Self {
            slots: [[0; PACKET]; SLOTS],
            head: 0,
            len: 0,
        }
    }

    /// Queues a record. A full queue drops it: a host that does not read is not waited for.
    fn push(&mut self, packet: [u8; PACKET]) {
        if self.len < SLOTS {
            let at = (self.head + self.len) % SLOTS;
            self.slots[at] = packet;
            self.len += 1;
        }
    }

    fn pop(&mut self) -> Option<[u8; PACKET]> {
        if self.len == 0 {
            return None;
        }
        let packet = self.slots[self.head];
        self.head = (self.head + 1) % SLOTS;
        self.len -= 1;
        Some(packet)
    }
}

/// The bulk side of a probe: it sends queued records when the host takes them, and takes no commands.
struct ProbeBulk<'a> {
    queue: &'a mut Queue,
}

impl BulkEndpoints for ProbeBulk<'_> {
    fn on_out(&mut self, _message: &[u8]) -> Result<(), Reboot> {
        Ok(())
    }

    fn on_in(&mut self, out: &mut [u8]) -> Result<usize, NoReply> {
        match self.queue.pop() {
            Some(packet) => {
                out[..PACKET].copy_from_slice(&packet);
                Ok(PACKET)
            }
            None => Err(NoReply),
        }
    }

    fn on_reset(&mut self) {
        // The records are the host's to read; a bus reset does not take them back.
    }
}

/// One probe run, from the first tick to the reset.
pub struct Probe<H: UsbHw> {
    dev: Option<Device<H>>,
    queue: Queue,
    flags: u32,
    started: Option<u64>,
    last_sample: Option<u64>,
    /// The path being taken, and the tick its record was queued.
    pending: Option<(u8, u64)>,
}

impl<H: UsbHw> Probe<H> {
    /// A probe in the mode `flags` (see [`crate::image`]). The device comes up if it can; a probe whose
    /// device does not come up still runs its path, with no telemetry.
    pub fn new(hw: H, flags: u32) -> Self {
        Self {
            dev: Device::start(hw).ok(),
            queue: Queue::new(),
            flags,
            started: None,
            last_sample: None,
            pending: None,
        }
    }

    /// Queues the boot record first: what the ROM latched as the boot mode, read on the board.
    pub fn with_boot(mut self, ctl: u32) -> Self {
        self.queue.push(telemetry::boot(ctl));
        self
    }

    /// Whether the device came up (telemetry is possible).
    pub fn has_device(&self) -> bool {
        self.dev.is_some()
    }

    /// The hardware, for a test or the simulator to look at.
    pub fn hw(&self) -> Option<&H> {
        self.dev.as_ref().map(Device::hw)
    }

    /// The hardware, mutably.
    pub fn hw_mut(&mut self) -> Option<&mut H> {
        self.dev.as_mut().map(Device::hw_mut)
    }

    fn take(&mut self, path: u8, now: u64) {
        self.queue.push(telemetry::taking(path, now));
        self.pending = Some((path, now));
    }

    /// One step: sample if due, start the path if its time has come, move the device, and say whether
    /// the image resets now.
    pub fn tick<R: RegAccess, C: Clock>(&mut self, regs: &mut R, clock: &C) -> Action {
        let now = clock.ticks();
        let started = *self.started.get_or_insert(now);
        if self.pending.is_none() {
            if self.flags & SOC_AT_ONCE != 0 {
                self.take(PATH_SOC, now);
            } else if now.wrapping_sub(started) >= SPIN_DEADLINE {
                self.take(PATH_DEADLINE, now);
            } else if self
                .last_sample
                .is_none_or(|last| now.wrapping_sub(last) >= SAMPLE_TICKS)
            {
                self.last_sample = Some(now);
                let cr = [
                    regs.read32(wdt::BASE0 + wdt::CR),
                    regs.read32(wdt::BASE1 + wdt::CR),
                ];
                let ccvr = [
                    regs.read32(wdt::BASE0 + wdt::CCVR),
                    regs.read32(wdt::BASE1 + wdt::CCVR),
                ];
                self.queue.push(telemetry::sample(now, cr, ccvr));
            }
        }
        if let Some(dev) = self.dev.as_mut() {
            let mut bulk = ProbeBulk {
                queue: &mut self.queue,
            };
            // A device error is not a reason to wait: the path below is taken regardless.
            let _: Result<Step, _> = dev.step(&mut bulk);
        }
        if let Some((path, queued_at)) = self.pending {
            let sent = self.queue.len == 0;
            if sent || now.wrapping_sub(queued_at) >= GRACE_TICKS {
                return Action::Reset(path);
            }
        }
        Action::Continue
    }

    /// Whether this probe arms `wdt0` / `wdt1` (the mode names them), for the tests' harness.
    pub fn arms(flags: u32) -> (bool, bool) {
        (flags & WDT0 != 0, flags & WDT1 != 0)
    }
}
