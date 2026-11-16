//! SD card initialisation through the dw_mmc host, from a controller reset: the card identification
//! sequence the kernel's mmc core runs, then 4-bit bus width. Each command is one register sequence
//! over [`RegAccess`]; each wait is bounded.
//!
//! Sources (linux-rpi-7.2 at 53679a578):
//! - Commands and their response types: `include/linux/mmc/sd.h` 8 (CMD8, R7), 41 (ACMD41, R3),
//!   `include/linux/mmc/mmc.h` 1, 2, 3, 6, 7 (CMD2 R2, CMD3 R6, CMD7 R1b); `drivers/mmc/core/sd.c`
//!   around 853 (the CMD8 check pattern and the voltage byte).
//! - Clock enable and clock update: `dw_mmc.c` 278-280 (CLKENA), `dw_mmc.h` 282 (CLKENA), 282/393
//!   (CMD_UPD_CLK, bit 21, dw_mmc.h:393).
//! - Bus width: `dw_mmc.h` 366-367 (CTYPE 4-bit = bit 0, 1-bit = 0), `dw_mmc.c` 1192 (CTYPE write).
//! - Response registers: `dw_mmc.h` 290-293 (RESP0..RESP3).

use crate::bind::{RegAccess, dw_mmc_reset};
use crate::io::{IoFault, sd_command_response};
use crate::regs::dw_mmc;
use lemnos_flash_stage::wait::{Clock, WaitError};

/// Polls of ACMD41 before the card is taken as never ready.
pub const ACMD41_POLLS: u32 = 1_000;
/// The voltage the host offers in CMD8 (2.7-3.6 V, bit 8) with the check pattern 0xAA.
pub const CMD8_ARG: u32 = 0x1AA;
/// The OCR the host offers in ACMD41: the 3.2-3.3 V window and high capacity (HCS, bit 30).
pub const ACMD41_ARG: u32 = 0x40FF_8000;

/// A card that answered the identification sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Card {
    /// The relative card address CMD3 assigned.
    pub rca: u16,
    /// Whether the card is high capacity (CCS, OCR bit 30): then the block address is the LBA.
    pub high_capacity: bool,
}

/// Why SD initialisation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitError {
    /// A command got no response (no card, or the card is not answering).
    NoResponse,
    /// A response had a CRC error.
    ResponseCrc,
    /// The card did not accept the voltage (CMD8 did not echo the pattern with the voltage bit).
    WrongVoltage,
    /// The card stayed busy through every ACMD41 poll.
    NeverReady,
    /// A bounded hardware wait ran out.
    Timeout,
    /// The card gave a response the sequence cannot use (an RCA of zero).
    Protocol,
}

impl From<IoFault> for InitError {
    fn from(fault: IoFault) -> Self {
        match fault {
            IoFault::CommandTimeout => InitError::NoResponse,
            IoFault::CommandCrc => InitError::ResponseCrc,
            IoFault::CardBusy => InitError::NeverReady,
            _ => InitError::Timeout,
        }
    }
}

impl From<WaitError> for InitError {
    fn from(_: WaitError) -> Self {
        InitError::Timeout
    }
}

/// Response flags of the command register (dw_mmc.h:401-403).
const RESP_EXP: u32 = dw_mmc::CMD_RESP_EXP;
const RESP_CRC: u32 = dw_mmc::CMD_RESP_CRC;
const RESP_LONG: u32 = 1 << 7;

/// Brings the card at the SD host `base` up to transfer state: a controller reset, the clock on, the
/// identification commands, the RCA, select, and 4-bit bus width. Returns the card.
pub fn sd_init<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
    deadline_us: u64,
) -> Result<Card, InitError> {
    dw_mmc_reset(regs, clock, base, deadline_us).map_err(|_| InitError::Timeout)?;
    // 1-bit bus and the clock on, then the clock update (dw_mmc.h:366-367, 282, 393).
    regs.write32(base + dw_mmc::CTYPE, 0);
    regs.write32(base + dw_mmc::CLKENA, 1);
    sd_command_response(regs, clock, base, 0, 0, 0, dw_mmc::CMD_UPD_CLK, deadline_us)?;

    // CMD0: go idle (no response).
    sd_command_response(regs, clock, base, 0, 0, 0, 0, deadline_us)?;
    // CMD8: the voltage and the pattern; the card echoes both (R7).
    let r7 = sd_command_response(
        regs,
        clock,
        base,
        8,
        CMD8_ARG,
        RESP_EXP | RESP_CRC,
        0,
        deadline_us,
    )?;
    if r7[0] & 0xFF != 0xAA || (r7[0] >> 8) & 0xF != 1 {
        return Err(InitError::WrongVoltage);
    }
    // ACMD41, polled while the card is busy (R3: no CRC on the response).
    let mut ocr = 0;
    let mut ready = false;
    for _ in 0..ACMD41_POLLS {
        sd_command_response(
            regs,
            clock,
            base,
            55,
            0,
            RESP_EXP | RESP_CRC,
            0,
            deadline_us,
        )?;
        ocr = sd_command_response(
            regs,
            clock,
            base,
            41,
            ACMD41_ARG | (1 << 30),
            RESP_EXP,
            0,
            deadline_us,
        )?[0];
        if ocr & (1 << 31) != 0 {
            ready = true;
            break;
        }
    }
    if !ready {
        return Err(InitError::NeverReady);
    }
    let high_capacity = ocr & (1 << 30) != 0;
    // CMD2: the CID (R2, long, with CRC).
    sd_command_response(
        regs,
        clock,
        base,
        2,
        0,
        RESP_EXP | RESP_LONG | RESP_CRC,
        0,
        deadline_us,
    )?;
    // CMD3: the relative card address (R6).
    let r6 = sd_command_response(regs, clock, base, 3, 0, RESP_EXP | RESP_CRC, 0, deadline_us)?;
    let rca = (r6[0] >> 16) as u16;
    if rca == 0 {
        return Err(InitError::Protocol);
    }
    // CMD7: select the card (R1b).
    sd_command_response(
        regs,
        clock,
        base,
        7,
        u32::from(rca) << 16,
        RESP_EXP | RESP_CRC,
        0,
        deadline_us,
    )?;
    // ACMD6 with argument 2: 4-bit bus; then the host's width follows (dw_mmc.h:367, dw_mmc.c:1338).
    sd_command_response(
        regs,
        clock,
        base,
        55,
        u32::from(rca) << 16,
        RESP_EXP | RESP_CRC,
        0,
        deadline_us,
    )?;
    sd_command_response(regs, clock, base, 6, 2, RESP_EXP | RESP_CRC, 0, deadline_us)?;
    regs.write32(base + dw_mmc::CTYPE, 1);
    Ok(Card { rca, high_capacity })
}
