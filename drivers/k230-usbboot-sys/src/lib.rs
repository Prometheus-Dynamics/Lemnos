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

/// The boot ROM's jump target. It arms both watchdogs (TORR `0xbb`, the kick `0x76`, CR bit 0), reads
/// them back (CR enabled, CCVR moving), and spins with a deadline and no feed. Any failure, and a
/// spin that outlives its deadline, takes the SoC software reset. No stack is used.
///
/// Register facts and their sources are in `lemnos_k230_usbboot_stage::{wdt, sysctl}`.
///
/// # Safety
///
/// Only the boot ROM may jump here, with the stage loaded at its load address and the watchdogs'
/// and the boot system controller's registers mapped. It never returns.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lemnos_k230_usbboot_entry() -> ! {
    core::arch::naked_asm!(
        // Arm wdt0 then wdt1: TORR, the kick, then CR enable.
        "li t0, 0x91106000",
        "li t1, 0xbb",
        "sw t1, 0x04(t0)",
        "li t1, 0x76",
        "sw t1, 0x0c(t0)",
        "li t1, 1",
        "sw t1, 0x00(t0)",
        "li t0, 0x91106800",
        "li t1, 0xbb",
        "sw t1, 0x04(t0)",
        "li t1, 0x76",
        "sw t1, 0x0c(t0)",
        "li t1, 1",
        "sw t1, 0x00(t0)",
        // Readback, wdt0: CR enabled, and CCVR moves within 16 reads.
        "li t0, 0x91106000",
        "lw t2, 0x00(t0)",
        "andi t2, t2, 1",
        "beqz t2, 9f",
        "lw t2, 0x08(t0)",
        "li t3, 16",
        "1:",
        "lw t4, 0x08(t0)",
        "bne t4, t2, 2f",
        "addi t3, t3, -1",
        "bnez t3, 1b",
        "j 9f",
        "2:",
        // Readback, wdt1, the same.
        "li t0, 0x91106800",
        "lw t2, 0x00(t0)",
        "andi t2, t2, 1",
        "beqz t2, 9f",
        "lw t2, 0x08(t0)",
        "li t3, 16",
        "3:",
        "lw t4, 0x08(t0)",
        "bne t4, t2, 4f",
        "addi t3, t3, -1",
        "bnez t3, 3b",
        "j 9f",
        "4:",
        // Spin with no feed, bounded by the core's time CSR (deadline in ticks, 1 MHz assumed).
        "li t5, 0x10000000",
        "rdtime t4",
        "5:",
        "rdtime t6",
        "sub t6, t6, t4",
        "bltu t6, t5, 5b",
        // The deadline passed: take the SoC reset.
        "9:",
        "li t0, 0x91102060",
        "li t1, 0x10001",
        "sw t1, 0x00(t0)",
        "6:",
        "j 6b",
    )
}

/// Probe image, run 1 of the board plan: `wdt0` (TOP 11, 2^27 cycles) and `wdt1` (TOP 10, 2^26
/// cycles) both armed and read back, then the deadline spin, then the SoC reset. Three return paths.
/// `wdt1`'s period is half `wdt0`'s, so the elapsed time names the path that returned.
///
/// # Safety
///
/// As [`lemnos_k230_usbboot_entry`]: only the boot ROM may jump here. It never returns.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lemnos_probe_all() -> ! {
    core::arch::naked_asm!(
        "li t0, 0x91106000",
        "li t1, 0xbb",
        "sw t1, 0x04(t0)",
        "li t1, 0x76",
        "sw t1, 0x0c(t0)",
        "li t1, 1",
        "sw t1, 0x00(t0)",
        "li t0, 0x91106800",
        "li t1, 0xaa",
        "sw t1, 0x04(t0)",
        "li t1, 0x76",
        "sw t1, 0x0c(t0)",
        "li t1, 1",
        "sw t1, 0x00(t0)",
        "li t0, 0x91106000",
        "lw t2, 0x00(t0)",
        "andi t2, t2, 1",
        "beqz t2, 9f",
        "lw t2, 0x08(t0)",
        "li t3, 16",
        "1:",
        "lw t4, 0x08(t0)",
        "bne t4, t2, 2f",
        "addi t3, t3, -1",
        "bnez t3, 1b",
        "j 9f",
        "2:",
        "li t0, 0x91106800",
        "lw t2, 0x00(t0)",
        "andi t2, t2, 1",
        "beqz t2, 9f",
        "lw t2, 0x08(t0)",
        "li t3, 16",
        "3:",
        "lw t4, 0x08(t0)",
        "bne t4, t2, 4f",
        "addi t3, t3, -1",
        "bnez t3, 3b",
        "j 9f",
        "4:",
        "li t5, 0x10000000",
        "rdtime t4",
        "5:",
        "rdtime t6",
        "sub t6, t6, t4",
        "bltu t6, t5, 5b",
        "9:",
        "li t0, 0x91102060",
        "li t1, 0x10001",
        "sw t1, 0x00(t0)",
        "6:",
        "j 6b",
    )
}

/// Probe image, run 2 of the board plan: `wdt0` only (TOP 11), read back, then the deadline spin,
/// then the SoC reset.
///
/// # Safety
///
/// As [`lemnos_k230_usbboot_entry`]. It never returns.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lemnos_probe_wdt0() -> ! {
    core::arch::naked_asm!(
        "li t0, 0x91106000",
        "li t1, 0xbb",
        "sw t1, 0x04(t0)",
        "li t1, 0x76",
        "sw t1, 0x0c(t0)",
        "li t1, 1",
        "sw t1, 0x00(t0)",
        "lw t2, 0x00(t0)",
        "andi t2, t2, 1",
        "beqz t2, 9f",
        "lw t2, 0x08(t0)",
        "li t3, 16",
        "1:",
        "lw t4, 0x08(t0)",
        "bne t4, t2, 2f",
        "addi t3, t3, -1",
        "bnez t3, 1b",
        "j 9f",
        "2:",
        "li t5, 0x10000000",
        "rdtime t4",
        "5:",
        "rdtime t6",
        "sub t6, t6, t4",
        "bltu t6, t5, 5b",
        "9:",
        "li t0, 0x91102060",
        "li t1, 0x10001",
        "sw t1, 0x00(t0)",
        "6:",
        "j 6b",
    )
}

/// Probe image, run 3 of the board plan: `wdt1` only (TOP 10), read back, then the deadline spin,
/// then the SoC reset.
///
/// # Safety
///
/// As [`lemnos_k230_usbboot_entry`]. It never returns.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lemnos_probe_wdt1() -> ! {
    core::arch::naked_asm!(
        "li t0, 0x91106800",
        "li t1, 0xaa",
        "sw t1, 0x04(t0)",
        "li t1, 0x76",
        "sw t1, 0x0c(t0)",
        "li t1, 1",
        "sw t1, 0x00(t0)",
        "lw t2, 0x00(t0)",
        "andi t2, t2, 1",
        "beqz t2, 9f",
        "lw t2, 0x08(t0)",
        "li t3, 16",
        "1:",
        "lw t4, 0x08(t0)",
        "bne t4, t2, 2f",
        "addi t3, t3, -1",
        "bnez t3, 1b",
        "j 9f",
        "2:",
        "li t5, 0x10000000",
        "rdtime t4",
        "5:",
        "rdtime t6",
        "sub t6, t6, t4",
        "bltu t6, t5, 5b",
        "9:",
        "li t0, 0x91102060",
        "li t1, 0x10001",
        "sw t1, 0x00(t0)",
        "6:",
        "j 6b",
    )
}

/// Probe image, run 4 of the board plan: no watchdog armed; the deadline spin, then the SoC reset.
///
/// # Safety
///
/// As [`lemnos_k230_usbboot_entry`]. It never returns.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lemnos_probe_spin() -> ! {
    core::arch::naked_asm!(
        "li t5, 0x10000000",
        "rdtime t4",
        "5:",
        "rdtime t6",
        "sub t6, t6, t4",
        "bltu t6, t5, 5b",
        "li t0, 0x91102060",
        "li t1, 0x10001",
        "sw t1, 0x00(t0)",
        "6:",
        "j 6b",
    )
}

/// Probe image, run 5 of the board plan: no watchdog armed, and the SoC reset taken at once.
///
/// # Safety
///
/// As [`lemnos_k230_usbboot_entry`]. It never returns.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lemnos_probe_soc() -> ! {
    core::arch::naked_asm!(
        "li t0, 0x91102060",
        "li t1, 0x10001",
        "sw t1, 0x00(t0)",
        "6:",
        "j 6b",
    )
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
