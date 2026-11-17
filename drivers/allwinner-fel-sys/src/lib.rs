//! The Allwinner FEL thunk: the small routine the boot ROM's FEL loop runs with FEL_RUN. It arms the
//! watchdog first, saves the ROM's registers, swaps each table buffer with its backup, calls the SPL,
//! swaps the buffers back, disarms the watchdog and returns into the ROM's FEL loop.
//!
//! One thunk per target, each a `#[unsafe(naked)]` function whose body is only `naked_asm!`. The
//! data the host fills (the swap table, the SPL entry, the watchdog register and its values) sits in the
//! same section, after the code, so the built section is the blob the host writes. The offsets of that
//! data are exported as symbols, and `scripts/check-blob.sh` checks them against
//! `lemnos-allwinner-fel-blob`; the bytes are checked against the pinned blob there too.
//!
//! The thunk's loops are bounded: the swap table is walked at most eight entries, a size that is not a
//! whole number of words stops it, and each of those stops is a spin with no feed, so the watchdog's reset
//! is the exit. The watchdog is the only timer, and the SPL's own run is bounded by the period the host
//! arms, not by this code.
//!
//! This crate is `no_std` and holds only the naked routines. It is compiled only for the two targets; on the
//! host it is empty, and its blob is checked by `scripts/check-blob.sh`.

#![cfg_attr(not(test), no_std)]

#[cfg(all(target_arch = "arm", target_os = "none"))]
mod armv7 {
    use core::arch::naked_asm;

    /// The ARMv7-A thunk (`armv7a-none-eabi`, ARM state). Registers: r0-r3 and r12 scratch between the
    /// save and the restore; r4-r11, sp and lr are saved in `lemnos_fel_save`.
    ///
    /// # Safety
    ///
    /// Only the boot ROM's FEL loop may call this, through FEL_RUN, with the blob at the thunk address and
    /// the data of `lemnos-allwinner-fel-blob` filled. Nothing else is valid to call.
    #[unsafe(naked)]
    #[unsafe(no_mangle)]
    #[unsafe(link_section = ".text.lemnos_fel_thunk")]
    pub unsafe extern "C" fn lemnos_fel_thunk() {
        naked_asm!(
            ".arm",
            ".syntax unified",
            // Watchdog first: write the arm value to the watchdog's register, before anything else.
            "adr r0, lemnos_fel_wdt_addr",
            "ldr r0, [r0]",
            "adr r1, lemnos_fel_wdt_arm",
            "ldr r1, [r1]",
            "str r1, [r0]",
            // Save the ROM's callee-saved registers, sp and lr.
            "adr r0, lemnos_fel_save",
            "stmia r0!, {{r4-r11}}",
            "str sp, [r0], #4",
            "str lr, [r0], #4",
            "mov r1, #0",
            "adr r2, lemnos_fel_flag",
            "str r1, [r2]",
            // One pass swaps every table buffer. The table holds eight entries at most, and a zero size ends it.
            ".Lswap_pass:",
            "adr r4, lemnos_fel_table",
            "mov r5, #8",
            ".Lnext_entry:",
            "cmp r5, #0",
            "beq .Lspin",
            "sub r5, r5, #1",
            "ldr r0, [r4], #4",
            "ldr r1, [r4], #4",
            "ldr r2, [r4], #4",
            "cmp r2, #0",
            "beq .Lpass_end",
            "tst r2, #3",
            "bne .Lspin",
            ".Lswap_word:",
            "ldr r3, [r0]",
            "ldr r12, [r1]",
            "str r12, [r0], #4",
            "str r3, [r1], #4",
            "subs r2, r2, #4",
            "bne .Lswap_word",
            "b .Lnext_entry",
            // The first pass is followed by the SPL call, the second pass swaps back and restores.
            ".Lpass_end:",
            "adr r2, lemnos_fel_flag",
            "ldr r1, [r2]",
            "cmp r1, #0",
            "bne .Lrestore",
            "mov r1, #1",
            "str r1, [r2]",
            "adr r0, lemnos_fel_entry",
            "ldr r0, [r0]",
            "blx r0",
            "b .Lswap_pass",
            // Disarm the watchdog, so the ROM's FEL loop runs with the watchdog as the ROM left it, then return.
            ".Lrestore:",
            "adr r0, lemnos_fel_wdt_addr",
            "ldr r0, [r0]",
            "adr r1, lemnos_fel_wdt_disarm",
            "ldr r1, [r1]",
            "str r1, [r0]",
            "adr r0, lemnos_fel_save",
            "ldmia r0!, {{r4-r11}}",
            "ldr sp, [r0], #4",
            "ldr lr, [r0], #4",
            "bx lr",
            // A failed check ends here: no feed, so the watchdog resets the board.
            ".Lspin:",
            "b .Lspin",
            // The data the host fills, in the same section.
            ".balign 4",
            "lemnos_fel_table:",
            ".space 96",
            "lemnos_fel_entry:",
            ".word 0",
            "lemnos_fel_flag:",
            ".word 0",
            "lemnos_fel_wdt_addr:",
            ".word 0",
            "lemnos_fel_wdt_arm:",
            ".word 0",
            "lemnos_fel_wdt_disarm:",
            ".word 0",
            "lemnos_fel_save:",
            ".space 40",
        )
    }
}

#[cfg(all(target_arch = "arm", target_os = "none"))]
pub use armv7::lemnos_fel_thunk;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
mod aarch64 {
    use core::arch::naked_asm;

    /// The AArch64 thunk (`aarch64-unknown-none`). The table, the entry and the watchdog words are
    /// 32-bit addresses, as on the ARMv7 thunk, so one blob format serves both. Callee-saved x19-x30 and sp are
    /// saved in `lemnos_fel_save`; x0-x6 are scratch between the save and the restore.
    ///
    /// # Safety
    ///
    /// Only the boot ROM's FEL loop may call this, through FEL_RUN, with the blob at the thunk address and
    /// the data of `lemnos-allwinner-fel-blob` filled. Nothing else is valid to call.
    #[unsafe(naked)]
    #[unsafe(no_mangle)]
    #[unsafe(link_section = ".text.lemnos_fel_thunk")]
    pub unsafe extern "C" fn lemnos_fel_thunk() {
        naked_asm!(
            // Watchdog first: write the arm value to the watchdog's register, before anything else.
            "adr x0, lemnos_fel_wdt_addr",
            "ldr w0, [x0]",
            "adr x1, lemnos_fel_wdt_arm",
            "ldr w1, [x1]",
            "str w1, [x0]",
            // Save the ROM's callee-saved registers, sp and the link register.
            "adr x0, lemnos_fel_save",
            "stp x19, x20, [x0], #16",
            "stp x21, x22, [x0], #16",
            "stp x23, x24, [x0], #16",
            "stp x25, x26, [x0], #16",
            "stp x27, x28, [x0], #16",
            "stp x29, x30, [x0], #16",
            "mov x1, sp",
            "str x1, [x0]",
            "adr x2, lemnos_fel_flag",
            "str wzr, [x2]",
            // One pass swaps every table buffer. The table holds eight entries at most, and a zero size ends it.
            ".Lswap_pass:",
            "adr x4, lemnos_fel_table",
            "mov x5, #8",
            ".Lnext_entry:",
            "cbz x5, .Lspin",
            "sub x5, x5, #1",
            "ldr w0, [x4], #4",
            "ldr w1, [x4], #4",
            "ldr w2, [x4], #4",
            "cbz w2, .Lpass_end",
            "tst w2, #3",
            "b.ne .Lspin",
            ".Lswap_word:",
            "ldr w3, [x0]",
            "ldr w6, [x1]",
            "str w6, [x0], #4",
            "str w3, [x1], #4",
            "subs w2, w2, #4",
            "b.ne .Lswap_word",
            "b .Lnext_entry",
            // The first pass is followed by the SPL call, the second pass swaps back and restores.
            ".Lpass_end:",
            "adr x2, lemnos_fel_flag",
            "ldr w1, [x2]",
            "cbnz w1, .Lrestore",
            "mov w1, #1",
            "str w1, [x2]",
            "adr x0, lemnos_fel_entry",
            "ldr w0, [x0]",
            "blr x0",
            "b .Lswap_pass",
            // Disarm the watchdog, so the ROM's FEL loop runs with the watchdog as the ROM left it, then return.
            ".Lrestore:",
            "adr x0, lemnos_fel_wdt_addr",
            "ldr w0, [x0]",
            "adr x1, lemnos_fel_wdt_disarm",
            "ldr w1, [x1]",
            "str w1, [x0]",
            "adr x0, lemnos_fel_save",
            "ldp x19, x20, [x0], #16",
            "ldp x21, x22, [x0], #16",
            "ldp x23, x24, [x0], #16",
            "ldp x25, x26, [x0], #16",
            "ldp x27, x28, [x0], #16",
            "ldp x29, x30, [x0], #16",
            "ldr x1, [x0]",
            "mov sp, x1",
            "ret",
            // A failed check ends here: no feed, so the watchdog resets the board.
            ".Lspin:",
            "b .Lspin",
            // The data the host fills, in the same section.
            ".balign 8",
            "lemnos_fel_table:",
            ".space 96",
            "lemnos_fel_entry:",
            ".word 0",
            "lemnos_fel_flag:",
            ".word 0",
            "lemnos_fel_wdt_addr:",
            ".word 0",
            "lemnos_fel_wdt_arm:",
            ".word 0",
            "lemnos_fel_wdt_disarm:",
            ".word 0",
            ".balign 8",
            "lemnos_fel_save:",
            ".space 104",
        )
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub use aarch64::lemnos_fel_thunk;

// The host-side run of the pinned bytes under user-mode emulation (see harness.rs). Built only for the test
// profile on the two Linux ARM targets the emulator runs; on every other host this module does not exist.
#[cfg(all(
    test,
    target_os = "linux",
    any(target_arch = "arm", target_arch = "aarch64")
))]
mod harness;

/// The panic handler of the thunk's staticlib. A panic parks without a feed, so the watchdog the thunk armed
/// resets the board: the same exit as a failed check.
#[cfg(target_os = "none")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
