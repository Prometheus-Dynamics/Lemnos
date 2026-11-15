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
