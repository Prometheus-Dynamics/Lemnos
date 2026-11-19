//! The SoC-wide software reset: the second reset path, which does not depend on either watchdog.
//!
//! Provenance (read, nothing copied), kendryte/k230_sdk 7e302f7:
//! - Base: `SYSCTL_BOOT_BASE_ADDR` `0x91102000` (`sysctl_boot.h:34`, `board.h`; the device tree's
//!   `sysctl_boot@91102000`, `k230.dtsi` lines 438-440).
//! - Register: `soc_glb_rst` at `0x60` (`sysctl_boot.h:47`, `/* 0x60 */`).
//! - Write: `sysctl_boot_reset_soc()` writes `(1 << 0) | (1 << 16)` (`sysctl_boot.c:43-49`, bit 16 is
//!   the write enable for bit 0), then spins. The U-Boot reboot command writes `0x10001` to the same
//!   register (`src/little/uboot/arch/riscv/cpu/k230/cpu.c`, `do_reset`, at `SYSCTL_BOOT_BASE_ADDR+0x60`).
//! - Scope: the register is named "global" reset. Whether it also restarts the boot ROM's download
//!   mode with the medium's header invalid is UNVERIFIED on the board; the bench step 2 checks it.
//!
//! This path does not use the watchdogs. The stage takes it when an arming did not take, and when
//! the spin's deadline passes without a reset.

/// The boot system controller's base.
pub const SYSCTL_BOOT_BASE: usize = 0x9110_2000;
/// `soc_glb_rst`: the SoC-wide reset register.
pub const SOC_GLB_RST: usize = SYSCTL_BOOT_BASE + 0x60;
/// The value that resets the SoC: bit 0 (reset) with bit 16 (its write enable).
pub const SOC_RESET_VALUE: u32 = (1 << 0) | (1 << 16);

/// The system timer block (`SYSCTL_STC_BASE_ADDR`, k230_sdk `src/little/uboot/arch/riscv/cpu/k230/platform.h:98`;
/// `STC_BASE_ADDR`, rt-smart `bsp/maix3/board/board.h:183`).
pub const STC_BASE: usize = 0x9110_8000;
/// The machine timer's clock for CPU0 (`cpu.c:76`, "enable mtimer clk", and rt-smart `c908/clint.c:52`).
pub const STC_MTIMER_CLK_CPU0: usize = STC_BASE + 0x20;
/// The machine timer's clock for CPU1 (`cpu.c:77`, rt-smart `c908/clint.c:53`).
pub const STC_MTIMER_CLK_CPU1: usize = STC_BASE + 0x30;
/// The value both clock registers are written with (`cpu.c:76-77`).
pub const STC_MTIMER_CLK_ON: u32 = 1;
/// The first timer counter's control ("enable stc0", `cpu.c:78-79`), and the value written there. The field
/// layout of `0x69` is not documented in any source read; the value is the vendor SPL's, written as is.
pub const STC0_CTRL: usize = STC_BASE;
/// See [`STC0_CTRL`].
pub const STC0_ENABLE: u32 = 0x69;

/// The writes the vendor's U-Boot SPL makes before it relies on the `time` CSR, in its order
/// (`harts_early_init`, k230_sdk 7e302f7 `src/little/uboot/arch/riscv/cpu/k230/cpu.c:73-79`, called from
/// `start.S:142-143`). The boot ROM leaves the `time` CSR at 0 and not moving (B2 run 1); after these it counts.
pub const STC_ENABLES: [(usize, u32); 3] = [
    (STC_MTIMER_CLK_CPU0, STC_MTIMER_CLK_ON),
    (STC_MTIMER_CLK_CPU1, STC_MTIMER_CLK_ON),
    (STC0_CTRL, STC0_ENABLE),
];
