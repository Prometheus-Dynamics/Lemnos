//! The rehearsal's program: the image's PMP probe, then the line the image prints, then the step that follows it
//! (`identify`, which the stage answers next). It runs as the image's entry would, in machine mode, on the qemu virt
//! machine. The UART is the virt machine's 16550 at 0x10000000; the exit is its test finisher at 0x100000.
#![no_std]
#![no_main]

use core::arch::global_asm;
use core::ptr::write_volatile;

use lemnos_k230_usbboot_stage::pmp::{LINE_LEN, put_line};

global_asm!(
    ".section .text.start, \"ax\"",
    ".global _start",
    "_start:",
    "la sp, stack_top",
    "call rehearsal_main",
    "1:",
    "wfi",
    "j 1b",
    // The harness's own vector, set before the probe: a trap in the harness itself parks here.
    ".balign 4",
    ".global harness_vec",
    "harness_vec:",
    "wfi",
    "j harness_vec",
);

unsafe extern "C" {
    static harness_vec: u8;
}

const UART: *mut u8 = 0x1000_0000 as *mut u8;
const FINISHER: *mut u32 = 0x10_0000 as *mut u32;

fn put(bytes: &[u8]) {
    for &b in bytes {
        // SAFETY: the virt machine's UART transmit register; a write is a byte out.
        unsafe { write_volatile(UART, b) };
    }
}

fn read_mtvec() -> u64 {
    let v: u64;
    // SAFETY: a read of mtvec, which machine mode may do; it changes nothing.
    unsafe { core::arch::asm!("csrr {0}, mtvec", out(reg) v, options(nomem, nostack)) };
    v
}

fn put_hex(v: u64) {
    for k in (0..16).rev() {
        let nibble = ((v >> (4 * k)) & 0xf) as u8;
        put(&[b"0123456789abcdef"[nibble as usize]]);
    }
}

#[unsafe(no_mangle)]
extern "C" fn rehearsal_main() -> ! {
    // The harness's own vector is installed, so the restore has a value to be checked against.
    // SAFETY: machine mode writes mtvec; the vector is the harness's own parking loop above.
    unsafe {
        core::arch::asm!("csrw mtvec, {0}", in(reg) &raw const harness_vec, options(nomem, nostack))
    };
    let before = read_mtvec();
    let outcome = lemnos_driver_k230_usbboot_sys::pmp::probe();
    let after = read_mtvec();
    let mut line = [0u8; LINE_LEN];
    let n = put_line(outcome, &mut line);
    put(&line[..n]);
    put(b"\n");
    // The vector the window installed is put back: the same value before and after.
    put(b"mtvec: before=");
    put_hex(before);
    put(b" after=");
    put_hex(after);
    put(b"\n");
    // The step after the read: the stage answers identify.
    put(b"identify: K230\n");
    // SAFETY: the virt machine's test finisher; writing the pass code exits the emulator.
    unsafe { write_volatile(FINISHER, 0x5555) };
    loop {}
}
