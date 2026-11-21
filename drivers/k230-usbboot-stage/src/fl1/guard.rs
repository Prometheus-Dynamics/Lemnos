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
    /// The clock the periods are computed at.
    clock_hz: u64,
    /// The longest setting armed first.
    max_top: u8,
}

impl<R: RegAccess> Dogs<R> {
    /// Wraps the bus. It arms nothing: [`Guard::arm`] does, and the entry calls it first. The periods are
    /// taken at the lowest clock the design accepts, and the longest setting is [`TOP`].
    pub fn new(regs: R) -> Self {
        Self {
            regs,
            clock_hz: CLOCK_HZ_MIN,
            max_top: TOP,
        }
    }

    /// Wraps the bus with a clock the caller measured (`clock_hz`, rounded up by the caller so a period is
    /// never taken longer than it is) and a longest setting of `max_top` (at most 15, the block's last).
    pub fn with_clock(regs: R, clock_hz: u64, max_top: u8) -> Self {
        Self {
            regs,
            clock_hz: clock_hz.max(1),
            max_top: max_top.min(15),
        }
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
            self.regs.write32(base + TORR, torr(self.max_top));
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
        self.max_top
    }

    fn period_us(&self, top: u8) -> u64 {
        crate::wdt::period_us(top, self.clock_hz)
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
