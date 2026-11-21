// The 471 stage: our own first code on the RK3566, built in-tree. The boot ROM chooses where to copy
// it and jumps to its first instruction, so this code is position-independent: no literal pools, no
// absolute branch targets, MMIO addresses as immediates, the DDR blob reached PC-relatively. If the
// stage never runs, nothing arms the watchdog; if it runs anywhere, it arms the watchdog the same way.
//
// lemnos_stage471_entry: arms the watchdog, then tail-jumps to the DDR blob, which follows this code
//   in the same image (adr reaches it; the bench builder keeps the offset fixed). The ROM's x30 is
//   never overwritten, so the blob returns to the ROM.
// lemnos_arm_and_spin: arms the watchdog and spins with no feed. Used by the bench tests: a reset is
//   certain, whatever the boot ROM did between the stages.
//
// Register facts (provenance in drivers/rockchip-maskrom-stage/src/wdt.rs): base 0xfe600000;
// TORR at 0x04 (TOP_INIT in bits 7:4, TOP in bits 3:0); the counter restart at 0x0c takes the kick
// value 0x76; CR at 0x00 has WDT_EN in bit 0 and RESP_MODE in bit 1, clear for a system reset.
// TOP 15 at 24 MHz is 2^31 / 24e6 s.
//
// Order matters: TORR before the kick (the kick loads the period), the kick before enabling.
// Only x9 and w10 are used; x30 is untouched (no bl). Stage order: arm_watchdog, then clear_boot_mode, then
// spin, return or tail jump (the 471 entries only; the 472 prologue is unchanged).

    .macro arm_watchdog
    movz    x9, #0xfe60, lsl #16         // WDT base 0xfe600000 (immediate, no literal pool)
    movz    w10, #0xff                   // TORR: TOP_INIT = 15, TOP = 15
    str     w10, [x9, #0x04]
    movz    w10, #0x76                   // kick: load the period
    str     w10, [x9, #0x0c]
    movz    w10, #0x01                   // CR: WDT_EN = 1, RESP_MODE = 0 (system reset)
    str     w10, [x9, #0x00]
    .endm

// clear_boot_mode: writes BOOT_NORMAL (0x5242C300) to PMU_GRF OS_REG0 (0xfdc20200) so that the boot ROM's
//   reboot-mode word is no longer the BROM download magic after this stage's reset. PMUGRF base 0xfdc20000
//   plus offset 0x200. Only x9 and w10 are used. Runs after arm_watchdog, so a hang here is still reset.
    .macro clear_boot_mode
    movz    x9, #0xfdc2, lsl #16         // PMUGRF base 0xfdc20000 (immediate)
    movz    w10, #0xc300                 // BOOT_NORMAL = 0x5242C300, low half
    movk    w10, #0x5242, lsl #16        //   high half
    str     w10, [x9, #0x200]            // OS_REG0 at 0xfdc20200
    .endm

    .section .text.lemnos_stage471, "ax"
    .global lemnos_stage471_entry
    .global lemnos_arm_and_spin

lemnos_stage471_entry:
    arm_watchdog
    clear_boot_mode
    adr     x16, lemnos_ddr_blob         // the DDR blob follows this code in the image
    br      x16                          // tail jump: the blob returns to the ROM's x30

lemnos_arm_and_spin:
    arm_watchdog
    clear_boot_mode
1:  b       1b                           // no feed: the watchdog resets the board

// lemnos_arm_and_return: arms the watchdog and returns to the boot ROM (x30 untouched). Used by
//   board step 1b: if the boot ROM keeps the watchdog running, the reset comes without any further
//   image; if it stops the watchdog, the PC's recovery sends the arm-and-spin image next.
    .global lemnos_arm_and_return
lemnos_arm_and_return:
    arm_watchdog
    clear_boot_mode
    ret

// lemnos_loader472_entry: the 472 loader's prologue. It re-arms the watchdog as its first action,
//   then branches PC-relatively to the Rust body (lemnos_loader472_main, in the same image). The
//   prologue uses no absolute address, so its bytes do not depend on the load address (U4).
    .global lemnos_loader472_entry
    .section .text.lemnos_entry, "ax"
    .global lemnos_loader472_entry
lemnos_loader472_entry:
    arm_watchdog                         // first: the watchdog, with PC-relative code only
    adr     x9, __image_base             // runtime load base (__image_base sits at link address 0)
    adr     x10, __rela_start            // the relocation table, PC-relative
    adr     x11, __rela_end
1:  cmp     x10, x11
    b.hs    2f
    ldr     x12, [x10, #8]               // r_info
    cmp     x12, #1027                   // R_AARCH64_RELATIVE: the only kind the image has
    b.ne    4f                           // anything else: park, no feed (the reset is the exit)
    ldr     x12, [x10]                   // r_offset: the slot, relative to the image base
    ldr     x13, [x10, #16]              // r_addend
    add     x12, x12, x9                 // the slot's runtime address
    add     x13, x13, x9                 // B + A
    str     x13, [x12]
    add     x10, x10, #24
    b       1b
2:  adr     x16, lemnos_loader472_main   // the relocated image: enter the Rust body
    br      x16
4:  b       4b
