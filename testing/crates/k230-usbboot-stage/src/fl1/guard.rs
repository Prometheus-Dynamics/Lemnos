//! The two watchdogs as the shared entry contract needs them. `arm` writes the arming in the same
//! order as [`crate::wdt::arm`] (TORR, kick, enable, for each); `armed` is the readback; the period
//! is taken at the lowest clock, so a stall is never reported later than the period the board can
//! give. Feeding kicks both counters.

use crate::wdt::{
    BASES, CCVR, CLOCK_HZ_MIN, COUNTER_READS, CR, CR_ENABLE, CRR, KICK, RegAccess, TOP, TORR, torr,
};
use lemnos_flash_stage::guard::{Guard, Watchdog};

/// The two watchdogs of the K230, driven through a register bus.
pub struct Dogs<R: RegAccess> {
    regs: R,
}

impl<R: RegAccess> Dogs<R> {
    /// Wraps the bus. It arms nothing: [`Guard::arm`] does, and the entry calls it first.
    pub fn new(regs: R) -> Self {
        Self { regs }
    }
}

impl<R: RegAccess> Watchdog for Dogs<R> {
    fn feed(&mut self) {
        for base in BASES {
            self.regs.write32(base + CRR, KICK);
        }
    }
}

impl<R: RegAccess> Guard for Dogs<R> {
    fn arm(&mut self) {
        for base in BASES {
            self.regs.write32(base + TORR, torr(TOP));
            self.regs.write32(base + CRR, KICK);
            self.regs.write32(base + CR, CR_ENABLE);
        }
    }

    fn armed(&mut self) -> bool {
        for base in BASES {
            if self.regs.read32(base + CR) & CR_ENABLE == 0 {
                return false;
            }
            let first = self.regs.read32(base + CCVR);
            let moved = (0..COUNTER_READS).any(|_| self.regs.read32(base + CCVR) != first);
            if !moved {
                return false;
            }
        }
        true
    }

    fn max_top(&self) -> u8 {
        TOP
    }

    fn period_us(&self, top: u8) -> u64 {
        crate::wdt::period_us(top, CLOCK_HZ_MIN)
    }

    fn set_top(&mut self, top: u8) {
        for base in BASES {
            self.regs.write32(base + TORR, torr(top));
            self.regs.write32(base + CRR, KICK);
        }
    }
}

/// A borrowed bus is a bus, so the entry can arm through one borrow and feed through another.
impl<T: RegAccess + ?Sized> RegAccess for &mut T {
    fn read32(&mut self, addr: usize) -> u32 {
        (**self).read32(addr)
    }

    fn write32(&mut self, addr: usize, value: u32) {
        (**self).write32(addr, value);
    }
}
