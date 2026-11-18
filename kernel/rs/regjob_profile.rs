// SPDX-License-Identifier: GPL-2.0
//! What a register-job profile is (lemnos_regjob.h): the part of a block's register map the engine
//! needs, and the checks and register sequences that are the block's own. The profiles are Rust
//! (rs/regjob_ve.rs, rs/regjob_isp.rs); the engine is still C in this stage (lemnos_regjob.c), so each
//! profile is also exported as the engine's `struct lrj_profile` (lemnos_regjob_priv.h), through
//! `c_profile!`, whose functions call the profile's. That table and its trampolines go when the engine
//! is ported.

use crate::bindings as lb;
use core::ffi::CStr;
use kernel::error::{code::*, Result};

/// LEMNOS_REGJOB_MAX_ADDRS of lemnos_regjob.h: the most slots a profile has.
pub(crate) const MAX_ADDRS: usize = lb::LEMNOS_REGJOB_MAX_ADDRS as usize;

/// Registers a caller may set: `count` registers from `offset`, only the bits in `mask`.
/// The layout of `struct lrj_plain`.
#[repr(C)]
pub(crate) struct Plain {
    pub(crate) offset: u32,
    pub(crate) count: u32,
    pub(crate) mask: u32,
}

/// A register that takes an address: written only by the module, from a held buffer, at an address
/// aligned to `align` (a power of two). The layout of `struct lrj_slot`.
#[repr(C)]
pub(crate) struct Slot {
    pub(crate) reg: u32,
    pub(crate) align: u32,
}

const _: () = assert!(core::mem::size_of::<Plain>() == core::mem::size_of::<lb::lrj_plain>());
const _: () = assert!(core::mem::size_of::<Slot>() == core::mem::size_of::<lb::lrj_slot>());

/// The block's registers, as a profile reads and writes them.
pub(crate) struct Regs(*mut lb::lem_rj);

impl Regs {
    pub(crate) fn rd(&self, offset: u32) -> u32 {
        // SAFETY: the engine's block, live while it calls the profile; offsets are the profile's own,
        // inside its window.
        unsafe { lb::lrj_rd(self.0, offset) }
    }

    pub(crate) fn wr(&self, offset: u32, value: u32) {
        // SAFETY: as rd.
        unsafe { lb::lrj_wr(self.0, offset, value) }
    }
}

/// The kernel's copy of a job, as a profile's check sees it: its writes, and by slot, in the profile's
/// order, the checked device address and length.
pub(crate) struct Ctx<'a> {
    w: &'a [lb::lemnos_regjob_write],
    addr: &'a [u64; MAX_ADDRS],
    bytes: &'a [u64; MAX_ADDRS],
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
        self.addr[slot]
    }

    pub(crate) fn bytes(&self, slot: usize) -> u64 {
        self.bytes[slot]
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
    pub(crate) check: fn(&Ctx<'_>, &mut [u64]) -> Result,
    /// Writes the registers the module derives from the checked slots (ends, sizes).
    pub(crate) derive: fn(&Regs, &Ctx<'_>),
    /// Before the start: clear and enable the completion interrupt.
    pub(crate) arm: fn(&Regs),
    /// In the handler: the status when the job completed, None when it is not this block's.
    pub(crate) irq: fn(&Regs) -> Option<u32>,
    /// Continuous profiles: switches the block to the job's addresses, at once on an idle block or in
    /// the handler at the end of a frame, and answers LEMNOS_REGJOB_STATUS_UNLATCHED when the block
    /// didn't take them. Called with the engine's interrupt lock held.
    pub(crate) latch: Option<fn(&Regs, &Ctx<'_>) -> u32>,
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

/// `struct lrj_profile`, for the C engine. Its pointers are to statics and functions only.
#[repr(transparent)]
pub(crate) struct CProfile(pub(crate) lb::lrj_profile);

// SAFETY: the table is read-only, and holds pointers to statics and functions only.
unsafe impl Sync for CProfile {}

/// The engine's job copy as a profile's Ctx.
///
/// # Safety
/// `c` is the engine's `struct lrj_ctx`, whose `w` holds `nw` writes, live for the call.
pub(crate) unsafe fn ctx<'a>(c: *const lb::lrj_ctx) -> Ctx<'a> {
    // SAFETY: the caller's contract.
    let c = unsafe { &*c };
    let w = if c.nw == 0 {
        &[][..]
    } else {
        // SAFETY: the caller's contract: `w` holds `nw` writes.
        unsafe { core::slice::from_raw_parts(c.w, c.nw as usize) }
    };
    Ctx { w, addr: &c.addr, bytes: &c.bytes }
}

/// The engine's block as a profile's Regs.
pub(crate) fn regs(rj: *mut lb::lem_rj) -> Regs {
    Regs(rj)
}

/// Exports `$p` (a `const` Profile) as the engine's `struct lrj_profile` named `$sym`.
macro_rules! c_profile {
    ($sym:ident, $p:path) => {
        const _: () = {
            use crate::bindings as lb;
            use crate::regjob_profile::{ctx, regs, CProfile, MAX_ADDRS};
            use kernel::ffi::c_int;

            unsafe extern "C" fn start_ok(v: u32) -> bool {
                ($p.start_ok)(v)
            }
            /// # Safety
            /// The engine's job copy, and room for MAX_ADDRS needs.
            unsafe extern "C" fn check(c: *const lb::lrj_ctx, need: *mut u64) -> c_int {
                // SAFETY: the engine passes its job copy.
                let c = unsafe { ctx(c) };
                // SAFETY: the engine's need array has MAX_ADDRS entries.
                let need = unsafe { core::slice::from_raw_parts_mut(need, MAX_ADDRS) };
                match ($p.check)(&c, need) {
                    Ok(()) => 0,
                    Err(e) => e.to_errno(),
                }
            }
            /// # Safety
            /// The engine's block and job copy.
            unsafe extern "C" fn derive(rj: *mut lb::lem_rj, c: *const lb::lrj_ctx) {
                // SAFETY: the engine passes its job copy.
                ($p.derive)(&regs(rj), &unsafe { ctx(c) })
            }
            unsafe extern "C" fn arm(rj: *mut lb::lem_rj) {
                ($p.arm)(&regs(rj))
            }
            /// # Safety
            /// The engine's block, and where the status goes.
            unsafe extern "C" fn irq(rj: *mut lb::lem_rj, status: *mut u32) -> c_int {
                match ($p.irq)(&regs(rj)) {
                    Some(s) => {
                        // SAFETY: the engine's status out-parameter.
                        unsafe { *status = s };
                        1
                    }
                    None => 0,
                }
            }
            /// # Safety
            /// The engine's block and job copy.
            unsafe extern "C" fn latch(rj: *mut lb::lem_rj, c: *const lb::lrj_ctx) -> u32 {
                match $p.latch {
                    // SAFETY: the engine passes its job copy.
                    Some(f) => f(&regs(rj), &unsafe { ctx(c) }),
                    None => 0,
                }
            }
            unsafe extern "C" fn quiet(rj: *mut lb::lem_rj) {
                ($p.quiet)(&regs(rj))
            }
            unsafe extern "C" fn reset_regs(rj: *mut lb::lem_rj) {
                ($p.reset_regs)(&regs(rj))
            }
            unsafe extern "C" fn event(rj: *mut lb::lem_rj) -> u32 {
                match $p.event {
                    Some(f) => f(&regs(rj)),
                    None => 0,
                }
            }
            unsafe extern "C" fn event_arm(rj: *mut lb::lem_rj, on: bool) {
                if let Some(f) = $p.event_arm {
                    f(&regs(rj), on)
                }
            }

            #[no_mangle]
            static $sym: CProfile = CProfile(lb::lrj_profile {
                id: $p.id,
                name: $p.name.as_ptr().cast(),
                window: $p.window,
                plain: $p.plain.as_ptr().cast(),
                nplain: $p.plain.len() as u32,
                keep_bits: $p.keep_bits,
                config: $p.config.as_ptr().cast(),
                nconfig: $p.config.len() as u32,
                slots: $p.slots.as_ptr().cast(),
                nslots: $p.slots.len() as u32,
                start_reg: $p.start_reg,
                addr_limit: $p.addr_limit,
                continuous: $p.continuous,
                irq_index: $p.irq_index,
                start_ok: Some(start_ok),
                check: Some(check),
                derive: Some(derive),
                arm: Some(arm),
                irq: Some(irq),
                latch: Some(latch),
                quiet: Some(quiet),
                reset_regs: Some(reset_regs),
                event_irq_index: $p.event_irq_index,
                event: Some(event),
                event_arm: Some(event_arm),
            });
        };
    };
}
pub(crate) use c_profile;
