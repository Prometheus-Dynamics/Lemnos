//! The K230 USB-boot stage's entry, its register access and its SD host: the only unsafe code of
//! the stage. The boot ROM jumps to [`lemnos_k230_usbboot_entry`] with the stage loaded at its load
//! address.
//!
//! The entry is assembly in `naked_asm!`. Its first instructions arm both watchdogs, then read
//! them back: CR must be enabled and CCVR must move. If either readback fails, or the spin runs
//! past its deadline without a reset, the entry takes the second path: the SoC-wide software reset
//! (`soc_glb_rst`, see `lemnos_k230_usbboot_stage::sysctl`), which does not depend on a watchdog.
//! The spin never feeds. The stage's logic (`lemnos-k230-usbboot-stage`) is not started from this
//! image yet: the stage's USB device path is not in the repository (see `docs/flashing.md`).
//!
//! Everything here targets `riscv64` bare metal for the entry. The register access and the SDHCI
//! host build on every target, so the workspace tests them.

#![no_std]

pub mod dwc2;
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
pub mod image;
pub mod sdhci;

use lemnos_k230_usbboot_stage::wdt::{self, RegAccess};

/// Reads the 32-bit register at `addr`.
///
/// # Safety
///
/// `addr` must be a mapped, 4-byte-aligned register that may be read, and whose read has no effect
/// the caller does not expect.
#[inline(always)]
pub unsafe fn read32(addr: usize) -> u32 {
    // SAFETY: the caller guarantees `addr` is a mapped, aligned, readable register (see # Safety).
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

/// Writes `value` to the 32-bit register at `addr`.
///
/// # Safety
///
/// `addr` must be a mapped, 4-byte-aligned register that may be written, and whose write is the one
/// the caller intends.
#[inline(always)]
pub unsafe fn write32(addr: usize, value: u32) {
    // SAFETY: the caller guarantees `addr` is a mapped, aligned, writable register (see # Safety).
    unsafe { core::ptr::write_volatile(addr as *mut u32, value) }
}

/// The watchdogs' registers over MMIO. It reaches only the two watchdogs' register windows, so an
/// address outside them is ignored whatever the caller passes.
pub struct Mmio;

impl RegAccess for Mmio {
    fn read32(&mut self, addr: usize) -> u32 {
        if !in_window(addr) {
            return 0;
        }
        // SAFETY: `in_window` holds, so `addr` is one of the watchdogs' registers, which are mapped
        // and aligned; reading them has no effect the arming does not expect.
        unsafe { read32(addr) }
    }

    fn write32(&mut self, addr: usize, value: u32) {
        if !in_window(addr) {
            return;
        }
        // SAFETY: `in_window` holds, so `addr` is one of the watchdogs' registers, which are mapped
        // and aligned; the callers write only the arming and feeding values.
        unsafe { write32(addr, value) }
    }
}

/// True when `addr` is an aligned register in either watchdog's window.
fn in_window(addr: usize) -> bool {
    addr.is_multiple_of(4)
        && wdt::BASES
            .iter()
            .any(|&base| (base..base + 0x800).contains(&addr))
}

/// The panic handler of the stage image. It parks without feeding the watchdogs, so a panic resets
/// the board through the watchdogs' deadline.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
