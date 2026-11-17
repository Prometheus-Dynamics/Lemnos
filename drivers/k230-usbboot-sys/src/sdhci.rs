//! The SD host: the K230 SDHCI controller's registers under the stage's SD card protocol
//! ([`lemnos_k230_usbboot_stage::sd::SdHost`]), in programmed I/O.
//!
//! Register map: the SDHCI standard offsets, as the SDK's `drv_sdhci.h` names them (`SDHCI_ARGUMENT`
//! 0x08, `SDHCI_COMMAND` 0x0e, `SDHCI_RESPONSE` 0x10, `SDHCI_BUFFER` 0x20, `SDHCI_PRESENT_STATE` 0x24,
//! `SDHCI_POWER_CONTROL` 0x29, `SDHCI_CLOCK_CONTROL` 0x2c, `SDHCI_SOFTWARE_RESET` 0x2f,
//! `SDHCI_INT_STATUS` 0x30). The controller's base is a parameter (`SdhciHost::new`): the boot
//! controller is not known to the stage without the boot mode and the host's parameter.
//!
//! UNVERIFIED on hardware: the clock. The device tree gives the SD clock from a `dummy_sd` clock, so
//! this host does not set a frequency; it enables the internal clock and waits for it to be stable,
//! and the bench step measures what the card sees. The long-response (R2) layout and the data
//! timing are taken from the standard and are UNVERIFIED on the board.
//!
//! Every wait is bounded by [`SPIN`] reads, so a controller that never answers fails the call.

use lemnos_k230_usbboot_stage::sd::{BLOCK, Expect, Reply, SdError, SdHost};

use crate::{read32, write32};

/// The controller's register window.
pub const WINDOW: usize = 0x1000;
/// Reads a wait makes before it gives up.
pub const SPIN: u32 = 1_000_000;

const ARGUMENT: usize = 0x08;
const BLOCK_SIZE: usize = 0x04;
const TRANSFER_MODE: usize = 0x0c;
const RESPONSE: usize = 0x10;
const BUFFER: usize = 0x20;
const PRESENT_STATE: usize = 0x24;
const POWER_CONTROL: usize = 0x29;
const CLOCK_CONTROL: usize = 0x2c;
const SOFTWARE_RESET: usize = 0x2f;
const INT_STATUS: usize = 0x30;
const INT_ENABLE: usize = 0x34;

/// Interrupt status bits: command complete, transfer complete, buffer write ready, buffer read
/// ready, and the error summary.
const INT_CMD_DONE: u32 = 1 << 0;
const INT_XFER_DONE: u32 = 1 << 1;
const INT_WRITE_READY: u32 = 1 << 4;
const INT_READ_READY: u32 = 1 << 5;
const INT_ERROR: u32 = 1 << 15;
/// Present state: command inhibit (bit 0) and data inhibit (bit 1).
const CMD_INHIBIT: u32 = 1 << 0;
const DAT_INHIBIT: u32 = 1 << 1;

/// The SDHCI host over the controller at `base`.
pub struct SdhciHost {
    base: usize,
}

impl SdhciHost {
    /// Resets the controller, powers the bus at 3.3 V, and enables the internal clock. Fails when
    /// the controller does not answer.
    pub fn new(base: usize) -> Result<Self, SdError> {
        let host = Self { base };
        host.set8(SOFTWARE_RESET, 1)?;
        host.wait_clear8(SOFTWARE_RESET, 1)?;
        host.set8(POWER_CONTROL, 0x0f)?;
        host.set16(CLOCK_CONTROL, 0x0001)?;
        host.wait_set16(CLOCK_CONTROL, 0x0002)?;
        host.set16(CLOCK_CONTROL, 0x0005)?;
        host.write(INT_ENABLE, 0x0000_ffff);
        Ok(host)
    }

    fn reg(&self, offset: usize) -> usize {
        self.base + offset
    }

    fn read(&self, offset: usize) -> u32 {
        let addr = self.reg(offset);
        if !in_window(self.base, addr) {
            return 0;
        }
        // SAFETY: `in_window` holds, so `addr` is an aligned register of this controller's window,
        // and reading an SDHCI register has no effect beyond its documented one (status reads).
        unsafe { read32(addr) }
    }

    fn write(&self, offset: usize, value: u32) {
        let addr = self.reg(offset);
        if !in_window(self.base, addr) {
            return;
        }
        // SAFETY: `in_window` holds, so `addr` is an aligned register of this controller's window;
        // the callers write only the documented SDHCI values.
        unsafe { write32(addr, value) }
    }

    /// A byte register: a read-modify-write of its aligned word.
    fn set8(&self, offset: usize, bits: u8) -> Result<(), SdError> {
        let word = offset & !3;
        let shift = (offset & 3) * 8;
        let old = self.read(word);
        self.write(word, (old & !(0xff << shift)) | (u32::from(bits) << shift));
        Ok(())
    }

    fn set16(&self, offset: usize, bits: u16) -> Result<(), SdError> {
        let word = offset & !3;
        let shift = (offset & 2) * 8;
        let old = self.read(word);
        self.write(
            word,
            (old & !(0xffff << shift)) | (u32::from(bits) << shift),
        );
        Ok(())
    }

    fn wait_clear8(&self, offset: usize, bits: u8) -> Result<(), SdError> {
        let word = offset & !3;
        let shift = (offset & 3) * 8;
        for _ in 0..SPIN {
            if (self.read(word) >> shift) as u8 & bits == 0 {
                return Ok(());
            }
        }
        Err(SdError::Timeout(0))
    }

    fn wait_set16(&self, offset: usize, bits: u16) -> Result<(), SdError> {
        let word = offset & !3;
        let shift = (offset & 2) * 8;
        for _ in 0..SPIN {
            if (self.read(word) >> shift) as u16 & bits == bits {
                return Ok(());
            }
        }
        Err(SdError::Timeout(0))
    }

    /// Waits until the interrupt status has `bit`, or reports an error.
    fn wait_int(&self, bit: u32, cmd: u8) -> Result<(), SdError> {
        for _ in 0..SPIN {
            let status = self.read(INT_STATUS);
            if status & INT_ERROR != 0 {
                self.write(INT_STATUS, status);
                return Err(SdError::Reply(cmd, "the controller reported an error"));
            }
            if status & bit != 0 {
                self.write(INT_STATUS, bit);
                return Ok(());
            }
        }
        Err(SdError::Timeout(cmd))
    }

    fn wait_inhibit_clear(&self, mask: u32, cmd: u8) -> Result<(), SdError> {
        for _ in 0..SPIN {
            if self.read(PRESENT_STATE) & mask == 0 {
                return Ok(());
            }
        }
        Err(SdError::Timeout(cmd))
    }
}

/// True when `addr` is an aligned register in the controller's window.
fn in_window(base: usize, addr: usize) -> bool {
    addr.is_multiple_of(4) && addr >= base && addr < base + WINDOW
}

impl SdHost for SdhciHost {
    fn command(&mut self, index: u8, arg: u32, expect: Expect) -> Result<Reply, SdError> {
        self.wait_inhibit_clear(CMD_INHIBIT, index)?;
        // The command register is the high halfword of the word at 0x0c, and the transfer mode is the
        // low halfword: both go in one write, so the data setup is in place when the command starts.
        // Response type (bits 1:0), CRC check (bit 3), index check (bit 4), data present (bit 5).
        // CMD41 (R3) has neither a CRC nor an index.
        let (data, transfer): (bool, u32) = match index {
            17 => (true, 0x12), // single block, block count, read
            24 => (true, 0x02), // single block, block count, write
            _ => (false, 0x00),
        };
        let response: u32 = match expect {
            Expect::None => 0x00,
            Expect::Long => 0x01 | 0x08,
            Expect::Short if index == 41 => 0x02,
            Expect::Short => 0x02 | 0x08 | 0x10,
        } | if data { 0x20 } else { 0x00 };
        self.write(INT_STATUS, 0xffff_ffff);
        self.write(ARGUMENT, arg);
        if data {
            // Block size (low halfword of 0x04) and block count (high halfword of 0x04): one block.
            self.write(BLOCK_SIZE, (1 << 16) | BLOCK as u32);
        }
        let word = (u32::from(index) << 24) | (response << 16) | transfer;
        self.write(TRANSFER_MODE, word);
        self.wait_int(INT_CMD_DONE, index)?;
        Ok(match expect {
            Expect::None => Reply::None,
            Expect::Short => Reply::Short(self.read(RESPONSE)),
            Expect::Long => {
                let mut out = [0u8; 16];
                for i in 0..4 {
                    let word = self.read(RESPONSE + 4 * (3 - i));
                    out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
                }
                Reply::Long(out)
            }
        })
    }

    fn read_block(&mut self, out: &mut [u8; BLOCK]) -> Result<(), SdError> {
        self.wait_int(INT_READ_READY, 17)?;
        for word in 0..BLOCK / 4 {
            let value = self.read(BUFFER);
            out[4 * word..4 * word + 4].copy_from_slice(&value.to_le_bytes());
        }
        self.wait_int(INT_XFER_DONE, 17)?;
        self.wait_inhibit_clear(DAT_INHIBIT, 17)
    }

    fn write_block(&mut self, data: &[u8; BLOCK]) -> Result<(), SdError> {
        self.wait_int(INT_WRITE_READY, 24)?;
        for word in 0..BLOCK / 4 {
            let mut bytes = [0u8; 4];
            bytes.copy_from_slice(&data[4 * word..4 * word + 4]);
            self.write(BUFFER, u32::from_le_bytes(bytes));
        }
        self.wait_int(INT_XFER_DONE, 24)?;
        self.wait_inhibit_clear(DAT_INHIBIT, 24)
    }
}
