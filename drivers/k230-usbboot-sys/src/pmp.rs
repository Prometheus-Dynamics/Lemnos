//! The PMP read (FLASH-EXTENT): `pmpcfg0` and `pmpaddr0`, read through a trap vector of our own.
//!
//! A read of a PMP CSR on a core without PMP is an illegal-instruction trap, and the boot ROM's trap handler is not
//! known. So, for the one window of the two reads, `mtvec` points at [`lemnos_pmp_handler`]: it records the first
//! trap's `mcause`, steps `mepc` past the trapping instruction (4 bytes, both reads are uncompressed), and returns.
//! `mtvec` is restored at once. Nothing else runs inside the window: no card or USB I/O, no stack, and machine
//! interrupts are off while it is open (`mstatus.MIE` is saved and put back).
//!
//! Registers in the window: the handler uses only `t0` to `t3`; the window keeps its values in `a0` to `a4`, which
//! the handler does not touch. The trap record is `lemnos_pmp_trap`: word 0 is 1 once a trap has been recorded,
//! word 1 is that trap's `mcause`.
//!
//! The mode: the window writes `mtvec` and `mstatus`, which only machine mode may do. The image's handover is in
//! machine mode (`testing/board-runs/K230-USB.md`, the trap capture's mode). No instruction tells machine mode from
//! supervisor mode without trapping, so that is the basis, and the entry says so in its line.
//!
//! Features `pmp-naive` (the reads with no vector of our own) and `pmp-broken` (the handler without the `mepc` step)
//! exist only for the qemu rehearsal (`drivers/k230-usbboot-image/pmp-rehearsal`), which must show they fail.

/// The trap record: word 0 is 1 once a trap was recorded, word 1 is its `mcause`.
#[unsafe(no_mangle)]
pub static mut lemnos_pmp_trap: [u32; 2] = [0; 2];

/// The trap vector's tail, shared by both variants: record the first trap, then return.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
macro_rules! handler_tail {
    () => {
        "csrr t1, mcause\n\
         la t2, lemnos_pmp_trap\n\
         lw t3, 0(t2)\n\
         bnez t3, 1f\n\
         sw t1, 4(t2)\n\
         li t3, 1\n\
         sw t3, 0(t2)\n\
         1:\n\
         mret"
    };
}

// The vector: the header, then the step past the trapping read, then the shared tail.
#[cfg(all(
    target_arch = "riscv64",
    target_os = "none",
    not(feature = "pmp-broken")
))]
core::arch::global_asm!(
    ".section .text.lemnos_pmp_handler, \"ax\"",
    ".balign 4",
    ".global lemnos_pmp_handler",
    "lemnos_pmp_handler:",
    "csrr t0, mepc",
    "addi t0, t0, 4",
    "csrw mepc, t0",
    handler_tail!(),
);

// The broken variant (rehearsal only): the same vector without the step, so the trapping read runs again.
#[cfg(all(target_arch = "riscv64", target_os = "none", feature = "pmp-broken"))]
core::arch::global_asm!(
    ".section .text.lemnos_pmp_handler, \"ax\"",
    ".balign 4",
    ".global lemnos_pmp_handler",
    "lemnos_pmp_handler:",
    handler_tail!(),
);

/// The address of the record's word `i`.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
fn trap_word(i: usize) -> *mut u32 {
    // Only the static's address is taken here; the caller reads or writes through the pointer, in its own block.
    let base = (&raw mut lemnos_pmp_trap).cast::<u32>();
    base.wrapping_add(i)
}

/// Reads `pmpcfg0` and `pmpaddr0`, under the trap vector above when the build has it, and says what happened.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
pub fn probe() -> lemnos_k230_usbboot_stage::pmp::Outcome {
    use lemnos_k230_usbboot_stage::pmp::Outcome;
    // SAFETY: the record is this image's static; it is cleared here before the window opens.
    unsafe {
        core::ptr::write_volatile(trap_word(0), 0);
    }
    // SAFETY: as above, for the second word.
    unsafe {
        core::ptr::write_volatile(trap_word(1), 0);
    }
    let (cfg, addr): (u32, u32);
    #[cfg(not(feature = "pmp-naive"))]
    // SAFETY: the window only reads the two PMP CSRs and writes mtvec and mstatus, and restores both before it closes.
    // The vector it installs is `lemnos_pmp_handler`, which steps past the trapping read and returns. The handler
    // writes only `lemnos_pmp_trap`, and touches only t0 to t3. No stack or memory outside the record is used.
    unsafe {
        core::arch::asm!(
            "csrrci a3, mstatus, 8",
            "csrr a2, mtvec",
            "la a4, lemnos_pmp_handler",
            "csrw mtvec, a4",
            "csrr a0, pmpcfg0",
            "csrr a1, pmpaddr0",
            "csrw mtvec, a2",
            "andi a3, a3, 8",
            "csrs mstatus, a3",
            out("a0") cfg,
            out("a1") addr,
            out("a2") _,
            out("a3") _,
            out("a4") _,
            options(nostack),
        );
    }
    #[cfg(feature = "pmp-naive")]
    // SAFETY: a plain read of the two CSRs; nothing is written. A core without PMP traps here, with no vector of ours.
    unsafe {
        core::arch::asm!("csrr {0}, pmpcfg0", "csrr {1}, pmpaddr0", out(reg) cfg, out(reg) addr, options(nostack, nomem));
    }
    // SAFETY: the record is read after the window closed; volatile, as the handler wrote it.
    // SAFETY: as above; the window has closed, and the handler's writes are complete when it returned.
    let trapped = unsafe { core::ptr::read_volatile(trap_word(0)) };
    // SAFETY: as above, for the second word.
    let mcause = unsafe { core::ptr::read_volatile(trap_word(1)) };
    if trapped != 0 {
        Outcome::Trapped { mcause }
    } else {
        Outcome::Read { cfg, addr }
    }
}
