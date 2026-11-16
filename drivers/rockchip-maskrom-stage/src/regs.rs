//! Register offsets and bits for the two controllers the 472 loader drives, and small models of the
//! bits the loader's state machine depends on. The offsets are read from the local kernel tree
//! (linux-rpi-7.2 at 53679a578); the RK3566 base addresses are not repeated here, they come from
//! the board's device tree at bench time and are "to check on bench".
//!
//! Sources:
//! - DWC3 (USB device controller): `drivers/usb/dwc3/core.h`.
//! - dw_mmc (SD/MMC host): `drivers/mmc/host/dw_mmc.h`.
//!
//! These models are for the sim, not the board. They reproduce the bits the loader reads and writes;
//! they do not model timing, FIFOs or the transfer engine.

/// DWC3 global and device register offsets, from the controller's base.
pub mod dwc3 {
    /// Global control: CORESOFTRESET is bit 11 (self-clearing).
    pub const GCTL: usize = 0xc110;
    /// Device configuration.
    pub const DCFG: usize = 0xc700;
    /// Device control: RUN_STOP is bit 31; CSFTRST is bit 30 (self-clearing).
    pub const DCTL: usize = 0xc704;
    /// Device event enables.
    pub const DEVTEN: usize = 0xc708;
    /// Device status.
    pub const DSTS: usize = 0xc70c;
    /// Device generic command.
    pub const DGCMD: usize = 0xc714;
    /// Endpoint command base: endpoint n's registers are at `DEP_BASE(n)`.
    pub const DEP_BASE_START: usize = 0xc800;
    /// Bytes between endpoints.
    pub const DEP_STRIDE: usize = 0x10;
    /// Offset of the endpoint command register within its endpoint block.
    pub const DEPCMD_OFFSET: usize = 0x0c;
    /// Offset of endpoint n's DEPCMDPAR1 (drivers/usb/dwc3/core.h:169).
    pub const fn depcmdpar1(n: usize) -> usize {
        DEP_BASE_START + n * DEP_STRIDE + 0x04
    }
    /// Offset of endpoint n's DEPCMDPAR0 (drivers/usb/dwc3/core.h:170).
    pub const fn depcmdpar0(n: usize) -> usize {
        DEP_BASE_START + n * DEP_STRIDE + 0x08
    }
    /// DALEPENA: the endpoint enable register (drivers/usb/dwc3/core.h:164); bit n is endpoint n
    /// (`DWC3_DALEPENA_EP(n)` = BIT(n), core.h:600).
    pub const DALEPENA: usize = 0xc720;
    /// DEPCMD command codes (drivers/usb/dwc3/core.h:587, 595, 584).
    /// Event buffer 0 (core.h:147-150): the base address, its size in bytes, and the count of bytes the
    /// controller has written and the CPU has not yet acknowledged.
    pub const GEVNTADRLO: usize = 0xc400;
    pub const GEVNTADRHI: usize = 0xc404;
    pub const GEVNTSIZ: usize = 0xc408;
    pub const GEVNTCOUNT: usize = 0xc40c;
    /// DEVTEN bits (core.h:501-512): disconnect, USB reset, connect done.
    pub const DEVTEN_DISCONNEVTEN: u32 = 1 << 0;
    pub const DEVTEN_USBRSTEN: u32 = 1 << 1;
    pub const DEVTEN_CONNECTDONEEN: u32 = 1 << 2;
    /// DCFG (core.h:440-446): the address field at bit 3, high speed.
    pub const DCFG_DEVADDR_SHIFT: u32 = 3;
    pub const DCFG_DEVADDR_MASK: u32 = 0x7f << 3;
    pub const DCFG_HIGHSPEED: u32 = 0;
    /// Device event words (core.h:61-71): the type is bits 8 to 11 of a word whose bit 0 is set.
    pub const DEVT_DISCONNECT: u32 = 0;
    pub const DEVT_USBRST: u32 = 1;
    /// Endpoint event words: bit 0 clear, the endpoint number in bits 1 to 5, the event in bits 6 to 9.
    pub const DEPEVT_XFERCOMPLETE: u32 = 0x01;
    pub const DEPEVT_XFERNOTREADY: u32 = 0x03;
    /// DEPSTARTCFG (core.h:584), SETTRANSFRESOURCE (core.h:594), SETSTALL (core.h:589).
    pub const DEPCMD_DEPSTARTCFG: u32 = 0x09;
    pub const DEPCMD_SETTRANSFRESOURCE: u32 = 0x02;
    pub const DEPCMD_SETSTALL: u32 = 0x04;
    pub const DEPCMD_ENDTRANSFER: u32 = 0x08;
    pub const DEPCMD_SETEPCONFIG: u32 = 0x01;
    pub const DEPCMD_STARTTRANSFER: u32 = 0x06;
    /// DCTL RUN_STOP (bit 31).
    pub const DCTL_RUN_STOP: u32 = 1 << 31;
    /// DCTL CSFTRST (bit 30).
    pub const DCTL_CSFTRST: u32 = 1 << 30;
    /// GCTL CORESOFTRESET (bit 11).
    pub const GCTL_CORESOFTRESET: u32 = 1 << 11;
    /// DEPCMD CMDACT (bit 10): set while the controller runs an endpoint command.
    pub const DEPCMD_CMDACT: u32 = 1 << 10;

    /// The command register of endpoint `n`.
    pub const fn depcmd(n: usize) -> usize {
        DEP_BASE_START + n * DEP_STRIDE + DEPCMD_OFFSET
    }
}

/// dw_mmc register offsets, from the controller's base.
pub mod dw_mmc {
    /// Control: RESET bit 0, FIFO_RESET bit 1, DMA_RESET bit 2 (all self-clearing).
    pub const CTRL: usize = 0x000;
    /// Clock divider.
    pub const CLKDIV: usize = 0x008;
    /// Clock enable.
    pub const CLKENA: usize = 0x010;
    /// Command argument.
    pub const CMDARG: usize = 0x028;
    /// Command: START is bit 31 (self-clearing once the command is accepted).
    pub const CMD: usize = 0x02c;
    /// Raw interrupt status, write 1 to clear.
    pub const RINTSTS: usize = 0x044;
    /// Status: DATA_BUSY is bit 9.
    pub const STATUS: usize = 0x048;
    /// Byte count of the data transfer.
    pub const BYTCNT: usize = 0x020;
    /// Block size.
    pub const BLKSIZ: usize = 0x01c;
    /// Card type: bit 0 set for a 4-bit bus (dw_mmc.h:284, 366-367).
    pub const CTYPE: usize = 0x018;
    /// Response registers 0 to 3 (dw_mmc.h:290-293).
    pub const RESP0: usize = 0x030;
    /// Internal DMA: bus mode.
    pub const BMOD: usize = 0x080;
    /// Internal DMA: descriptor list base.
    pub const DBADDR: usize = 0x088;
    /// Data FIFO (read and write port), at 0x200 on the controller.
    pub const DATA: usize = 0x200;

    /// CTRL RESET (bit 0).
    pub const CTRL_RESET: u32 = 1 << 0;
    /// CTRL FIFO_RESET (bit 1, self-clearing).
    pub const CTRL_FIFO_RESET: u32 = 1 << 1;
    /// CTRL USE_IDMAC (bit 25).
    pub const CTRL_USE_IDMAC: u32 = 1 << 25;
    /// CMD START (bit 31).
    pub const CMD_START: u32 = 1 << 31;
    /// CMD UPD_CLK (bit 21).
    pub const CMD_UPD_CLK: u32 = 1 << 21;
    /// CMD DAT_WR (bit 10).
    pub const CMD_DAT_WR: u32 = 1 << 10;
    /// CMD DAT_EXP (bit 9).
    pub const CMD_DAT_EXP: u32 = 1 << 9;
    /// INT CMD_DONE (bit 2, dw_mmc.h:384).
    pub const INT_CMD_DONE: u32 = 1 << 2;
    /// INT data CRC error (bit 7, dw_mmc.h:379).
    pub const INT_DCRC: u32 = 1 << 7;
    /// INT response timeout (bit 8, dw_mmc.h:378).
    pub const INT_RTO_BIT: u32 = 1 << 8;
    /// INT response CRC error (bit 6, dw_mmc.h:380).
    pub const INT_RCRC: u32 = 1 << 6;
    /// INT data timeout (bit 9, dw_mmc.h:377).
    pub const INT_DRTO: u32 = 1 << 9;
    /// CMD DAT_WR is bit 10 (dw_mmc.h:399); DAT_EXP bit 9 (400); RESP_CRC bit 8 (401); RESP_EXP bit 6 (403).
    pub const CMD_RESP_EXP: u32 = 1 << 6;
    pub const CMD_RESP_CRC: u32 = 1 << 8;
    /// INT DATA_OVER (bit 3).
    pub const INT_DATA_OVER: u32 = 1 << 3;
    /// INT response timeout (bit 8).
    pub const INT_RTO: u32 = 1 << 8;
    /// STATUS DATA_BUSY (bit 9).
    pub const STATUS_DATA_BUSY: u32 = 1 << 9;
}

/// The DWC3 bits the loader's device-mode bring-up uses, as a model. A command the loader starts
/// stays busy until the test (standing for the controller) completes it, so a stuck command is
/// visible to the bounded wait.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dwc3Model {
    dctl: u32,
    gctl: u32,
    depcmd: [u32; 16],
}

impl Dwc3Model {
    pub const fn new() -> Self {
        Self {
            dctl: 0,
            gctl: 0,
            depcmd: [0; 16],
        }
    }

    /// Writes a register. CSFTRST and CORESOFTRESET self-clear, as the controller does.
    pub fn write(&mut self, offset: usize, value: u32) {
        match offset {
            dwc3::DCTL => {
                self.dctl = value & !dwc3::DCTL_CSFTRST;
            }
            dwc3::GCTL => {
                self.gctl = value & !dwc3::GCTL_CORESOFTRESET;
            }
            off if (dwc3::DEP_BASE_START..dwc3::DEP_BASE_START + 16 * dwc3::DEP_STRIDE)
                .contains(&off)
                && (off - dwc3::DEP_BASE_START) % dwc3::DEP_STRIDE == dwc3::DEPCMD_OFFSET =>
            {
                let n = (off - dwc3::DEP_BASE_START) / dwc3::DEP_STRIDE;
                if value & dwc3::DEPCMD_CMDACT != 0 {
                    self.depcmd[n] = value;
                }
            }
            _ => {}
        }
    }

    /// The device is running (DCTL RUN_STOP set).
    pub fn running(&self) -> bool {
        self.dctl & dwc3::DCTL_RUN_STOP != 0
    }

    /// Endpoint `n` has a command in flight.
    pub fn command_busy(&self, n: usize) -> bool {
        self.depcmd[n] & dwc3::DEPCMD_CMDACT != 0
    }

    /// The controller finishes endpoint `n`'s command (the test stands for the hardware).
    pub fn complete_command(&mut self, n: usize) {
        self.depcmd[n] = 0;
    }
}

/// The dw_mmc bits the loader's data path uses, as a model. A command stays started (START set)
/// until the controller takes it; a stuck command is visible to the bounded wait.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DwMmcModel {
    ctrl: u32,
    cmd: u32,
    rintsts: u32,
    accepted: bool,
}

impl DwMmcModel {
    pub const fn new() -> Self {
        Self {
            ctrl: 0,
            cmd: 0,
            rintsts: 0,
            accepted: false,
        }
    }

    /// Writes a register. RESET self-clears; RINTSTS is write-1-to-clear.
    pub fn write(&mut self, offset: usize, value: u32) {
        match offset {
            dw_mmc::CTRL => self.ctrl = value & !dw_mmc::CTRL_RESET,
            dw_mmc::CMD => {
                self.cmd = value;
                self.accepted = false;
            }
            dw_mmc::RINTSTS => self.rintsts &= !value,
            _ => {}
        }
    }

    /// The command has START set and is not yet taken.
    pub fn command_pending(&self) -> bool {
        self.cmd & dw_mmc::CMD_START != 0 && !self.accepted
    }

    /// The controller takes the command (the test stands for the hardware).
    pub fn accept_command(&mut self) {
        if self.cmd & dw_mmc::CMD_START != 0 {
            self.accepted = true;
            self.cmd &= !dw_mmc::CMD_START;
            self.rintsts |= dw_mmc::INT_CMD_DONE;
        }
    }

    /// The raw interrupt status.
    pub fn rintsts(&self) -> u32 {
        self.rintsts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_command_registers_follow_the_header_layout() {
        // DWC3_DEP_BASE(n) = 0xc800 + n * 0x10; DEPCMD is at +0x0c.
        assert_eq!(dwc3::depcmd(0), 0xc80c);
        assert_eq!(dwc3::depcmd(1), 0xc81c);
    }

    #[test]
    fn dctl_run_stop_is_bit_31_and_csftrst_self_clears() {
        let mut dwc3 = Dwc3Model::new();
        dwc3.write(dwc3::DCTL, dwc3::DCTL_RUN_STOP | dwc3::DCTL_CSFTRST);
        assert!(dwc3.running());
        assert_eq!(
            dwc3.dctl & dwc3::DCTL_CSFTRST,
            0,
            "the reset bit does not stay set"
        );
    }

    #[test]
    fn a_stuck_endpoint_command_stays_busy_until_the_controller_finishes() {
        let mut dwc3 = Dwc3Model::new();
        dwc3.write(dwc3::depcmd(0), dwc3::DEPCMD_CMDACT | 0x06);
        assert!(dwc3.command_busy(0));
        dwc3.complete_command(0);
        assert!(!dwc3.command_busy(0));
    }

    #[test]
    fn a_dw_mmc_command_is_pending_until_the_controller_accepts_it() {
        let mut mmc = DwMmcModel::new();
        mmc.write(
            dw_mmc::CMD,
            dw_mmc::CMD_START | dw_mmc::CMD_DAT_WR | dw_mmc::CMD_DAT_EXP,
        );
        assert!(mmc.command_pending());
        mmc.accept_command();
        assert!(!mmc.command_pending());
        assert_ne!(mmc.rintsts() & dw_mmc::INT_CMD_DONE, 0);
    }

    #[test]
    fn rintsts_is_write_one_to_clear() {
        let mut mmc = DwMmcModel::new();
        mmc.write(dw_mmc::CMD, dw_mmc::CMD_START);
        mmc.accept_command();
        mmc.write(dw_mmc::RINTSTS, dw_mmc::INT_CMD_DONE);
        assert_eq!(mmc.rintsts() & dw_mmc::INT_CMD_DONE, 0);
    }

    #[test]
    fn a_stuck_command_is_caught_by_a_bounded_wait() {
        use crate::wdt::LONGEST_PERIOD_US;
        use lemnos_flash_stage::wait::{Clock, WaitError, wait_for};
        struct Ticking(u64);
        impl Clock for Ticking {
            fn now_us(&mut self) -> u64 {
                self.0 += 1_000;
                self.0
            }
        }
        let mut mmc = DwMmcModel::new();
        mmc.write(dw_mmc::CMD, dw_mmc::CMD_START);
        let mut clock = Ticking(0);
        let result: Result<(), WaitError> = wait_for(&mut clock, 50_000, LONGEST_PERIOD_US, || {
            (!mmc.command_pending()).then_some(())
        });
        assert_eq!(result, Err(WaitError::Timeout { budget_us: 50_000 }));
    }
}
