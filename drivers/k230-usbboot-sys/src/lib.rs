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

pub mod b2;
pub mod dwc2;
pub mod dwc2_legacy;
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
pub mod image;
pub mod sdhci;

use lemnos_k230_usbboot_stage::sysctl;
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

/// The register access of the stage image: the watchdogs (read and write), the first UART (read and write,
/// the block only), the SoC reset (its one value, written only), the system timer's three enables (their
/// values, written only), and the pad block (read only). A write into the pad block is refused here too (the stage's
/// `PadGuard` refuses it first). An address outside these windows reads as zero and is never written.
pub struct Mmio;

impl RegAccess for Mmio {
    fn read32(&mut self, addr: usize) -> u32 {
        if !addr.is_multiple_of(4) || !(in_window(addr) || in_pad_block(addr) || in_uart(addr)) {
            return 0;
        }
        // SAFETY: the address is aligned and lies in a watchdog, UART or pad register window; reading these
        // registers has no effect the stage does not expect (status and pad words).
        unsafe { read32(addr) }
    }

    fn write32(&mut self, addr: usize, value: u32) {
        if addr == sysctl::SOC_GLB_RST && value == sysctl::SOC_RESET_VALUE {
            // The SoC reset, and only its documented value: the FL1 report takes it through this bus
            // (`fl1::report::Uart`), so a bus that dropped it would leave the board to the watchdog.
            // SAFETY: `SOC_GLB_RST` is the boot system controller's aligned reset register
            // (`sysctl_boot.h:47`); the value is the documented reset, and nothing after it runs.
            unsafe { write32(addr, value) };
            return;
        }
        if sysctl::STC_ENABLES.contains(&(addr, value)) {
            // The system timer's enables, exactly the three (register, value) pairs the vendor SPL writes
            // before it relies on `time` (`sysctl::STC_ENABLES`, `cpu.c:73-79` at k230_sdk 7e302f7).
            // SAFETY: each address is an aligned register of the system timer block at `0x9110_8000`
            // (`platform.h:98`), outside the pad block; the values are the documented enables.
            unsafe { write32(addr, value) };
            return;
        }
        if !addr.is_multiple_of(4) || !(in_window(addr) || in_uart(addr)) || in_pad_block(addr) {
            return;
        }
        // SAFETY: the address is aligned and lies in a watchdog or UART register window (never the pad block);
        // the callers write only the documented arming, feeding and line-setup values.
        unsafe { write32(addr, value) }
    }
}

/// The pad block (`board.h:165-166`, 2 KiB at `0x91105000`): read only from the stage's side.
fn in_pad_block(addr: usize) -> bool {
    (0x9110_5000..0x9110_5800).contains(&addr)
}

/// The first UART's block (`k230.dtsi:84-92`, `serial0`).
fn in_uart(addr: usize) -> bool {
    (0x9140_0000..0x9140_0400).contains(&addr)
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
