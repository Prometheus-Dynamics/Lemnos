// SPDX-License-Identifier: GPL-2.0
//! What a register-job profile is (lemnos_regjob.h): the part of a block's register map the engine
//! (rs/regjob.rs) needs, and the checks and register sequences that are the block's own. The profiles
//! are rs/regjob_ve.rs and rs/regjob_isp.rs.

use crate::bindings as lb;
use core::ffi::CStr;
use kernel::{
    bindings,
    error::{code::*, Result},
};

/// LEMNOS_REGJOB_MAX_ADDRS of lemnos_regjob.h: the most slots a profile has.
pub(crate) const MAX_ADDRS: usize = lb::LEMNOS_REGJOB_MAX_ADDRS as usize;

/// Registers a caller may set: `count` registers from `offset`, only the bits in `mask`.
pub(crate) struct Plain {
    pub(crate) offset: u32,
    pub(crate) count: u32,
    pub(crate) mask: u32,
}

/// A register that takes an address: written only by the module, from a held buffer, at an address
/// aligned to `align` (a power of two).
pub(crate) struct Slot {
    pub(crate) reg: u32,
    pub(crate) align: u32,
}

/// The block's register window, as the engine and a profile read and write it. Every access is
/// inside the window: an offset outside it, or off a register boundary, is never made (a read answers
/// 0), so a profile's accesses are safe whatever its offsets.
pub(crate) struct Regs {
    base: *mut u8,
    window: u32,
}

impl Regs {
    /// # Safety
    /// `base` is the block's mapped register window (or, in the test harness, ordinary memory), at
    /// least `window` bytes, and stays mapped for as long as the Regs is used.
    pub(crate) unsafe fn new(base: *mut u8, window: u32) -> Self {
        Self { base, window }
    }

    fn inside(&self, offset: u32) -> bool {
        offset % 4 == 0 && self.window >= 4 && offset <= self.window - 4
    }

    pub(crate) fn rd(&self, offset: u32) -> u32 {
        if !self.inside(offset) {
            return 0;
        }
        // SAFETY: inside the mapped window (new's contract).
        let reg = unsafe { self.base.add(offset as usize) };
        // SAFETY: a register of the mapped window, on a register boundary.
        unsafe { bindings::readl(reg.cast()) }
    }

    pub(crate) fn wr(&self, offset: u32, value: u32) {
        if !self.inside(offset) {
            return;
        }
        // SAFETY: as rd.
        let reg = unsafe { self.base.add(offset as usize) };
        // SAFETY: as rd.
        unsafe { bindings::writel(value, reg.cast()) }
    }
}

/// By slot, in the profile's order: the checked device address and length of a job.
#[derive(Clone, Copy)]
pub(crate) struct Addrs {
    pub(crate) addr: [u64; MAX_ADDRS],
    pub(crate) bytes: [u64; MAX_ADDRS],
}

impl Addrs {
    pub(crate) const ZERO: Self = Self { addr: [0; MAX_ADDRS], bytes: [0; MAX_ADDRS] };

    pub(crate) fn addr(&self, slot: usize) -> u64 {
        self.addr[slot]
    }

    pub(crate) fn bytes(&self, slot: usize) -> u64 {
        self.bytes[slot]
    }
}

/// The kernel's copy of a job, as a profile's check sees it: its writes, and its checked slots.
pub(crate) struct Ctx<'a> {
    pub(crate) w: &'a [lb::lemnos_regjob_write],
    pub(crate) a: &'a Addrs,
}

impl Ctx<'_> {
    /// The one value the job writes to `offset`, for a geometry check: EINVAL when the job doesn't write
    /// it or writes it more than once.
    pub(crate) fn value(&self, offset: u32) -> Result<u32> {
        let mut found = None;
        for w in self.w.iter().filter(|w| w.offset == offset) {
            if found.is_some() {
                return Err(EINVAL);
            }
            found = Some(w.value);
        }
        found.ok_or(EINVAL)
    }

    pub(crate) fn addr(&self, slot: usize) -> u64 {
        self.a.addr(slot)
    }

    pub(crate) fn bytes(&self, slot: usize) -> u64 {
        self.a.bytes(slot)
    }
}

/// A block's profile.
pub(crate) struct Profile {
    /// LEMNOS_REGJOB_PROFILE_*.
    pub(crate) id: u32,
    pub(crate) name: &'static CStr,
    /// Bytes of the register window jobs and the module use.
    pub(crate) window: u32,
    /// What the frames role may write.
    pub(crate) plain: &'static [Plain],
    /// A write changes only the bits of the writer's mask; the others keep their value (read, merged,
    /// written back under the engine's lock). Off: the bits outside the mask are written as 0.
    pub(crate) keep_bits: bool,
    /// What the configuration role may write; empty: the profile has no such role.
    pub(crate) config: &'static [Plain],
    pub(crate) slots: &'static [Slot],
    pub(crate) start_reg: u32,
    /// Highest address + bytes a slot's register can hold (1 << 32 for a 32-bit register).
    pub(crate) addr_limit: u64,
    /// The block keeps using its addresses after a job completes (a capture path): it takes a job's
    /// addresses only when `latch` switches it to them.
    pub(crate) continuous: bool,
    /// Which of the device's interrupts completes a job.
    pub(crate) irq_index: u32,
    /// Whether a value may start a job.
    pub(crate) start_ok: fn(u32) -> bool,
    /// Fills `need[slot]`: the bytes the block will reach from each slot's address with this job's
    /// registers; an error refuses the job.
    pub(crate) check: fn(&Ctx<'_>, &mut [u64; MAX_ADDRS]) -> Result,
    /// Writes the registers the module derives from the checked slots (ends, sizes).
    pub(crate) derive: fn(&Regs, &Ctx<'_>),
    /// Before the start: clear and enable the completion interrupt.
    pub(crate) arm: fn(&Regs),
    /// In the handler: the status when the job completed, None when it is not this block's.
    pub(crate) irq: fn(&Regs) -> Option<u32>,
    /// Continuous profiles: switches the block to the job's addresses, at once on an idle block or in
    /// the handler at the end of a frame, and answers LEMNOS_REGJOB_STATUS_UNLATCHED when the block
    /// didn't take them. Called with the engine's interrupt lock held.
    pub(crate) latch: Option<fn(&Regs, &Addrs) -> u32>,
    /// Interrupts off.
    pub(crate) quiet: fn(&Regs),
    /// After the reset line was pulsed (and at probe): the block's own reset sequence.
    pub(crate) reset_regs: fn(&Regs),
    /// Configuration role: which of the device's interrupts carries its events.
    pub(crate) event_irq_index: u32,
    /// In the handler: the status read and cleared, 0 when nothing of the events' is pending.
    pub(crate) event: Option<fn(&Regs) -> u32>,
    /// The events' interrupt on (a configuration session is open) or off. Interrupt lock held.
    pub(crate) event_arm: Option<fn(&Regs, bool)>,
}
