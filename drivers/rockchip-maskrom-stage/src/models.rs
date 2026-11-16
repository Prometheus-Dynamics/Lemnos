//! Models of the controllers for the host tests. They answer the same registers as the hardware and
//! fail the same ways: a card that is absent, a wrong voltage answer, a card that never leaves busy,
//! a response or data CRC error, a timeout, and an endpoint command that never completes. A test names
//! the fault it wants; nothing here is the board.

use crate::bind::RegAccess;
use crate::io::BLOCK;
use crate::regs::{dw_mmc, dwc3};

/// The SD opcodes the model answers (sd.h, mmc.h).
const CMD_SEND_OP_COND_2: u32 = 2;
const CMD_SEND_RCA: u32 = 3;
const CMD_SELECT: u32 = 7;
const CMD_SEND_IF_COND: u32 = 8;
const CMD_APP_CMD: u32 = 55;
const CMD_APP_OP_COND: u32 = 41;
const CMD_SET_BUS_WIDTH: u32 = 6;
const CMD_READ_SINGLE: u32 = 17;
const CMD_WRITE_BLOCK: u32 = 24;
const CMD_INDEX_MASK: u32 = 0x3f;
const RESP_LONG: u32 = 1 << 7;
const RESP_CRC: u32 = dw_mmc::CMD_RESP_CRC;
const RESP_EXP: u32 = dw_mmc::CMD_RESP_EXP;

/// What the card or controller does with one command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmdFault {
    /// The command succeeds.
    None,
    /// No response (RTO).
    CommandTimeout,
    /// The response has a CRC error (RCRC).
    CommandCrc,
    /// The data phase times out (DRTO).
    DataTimeout,
    /// The data phase has a CRC error (DCRC).
    DataCrc,
    /// The card holds DATA_BUSY forever: it answers busy and never finishes.
    Busy,
}

/// An SD card behind the SD host's registers, with `blocks` 512-byte blocks.
pub struct SdCard {
    base: usize,
    pub blocks: Vec<[u8; BLOCK]>,
    /// The fault for each data command, in order. Commands after the list run clean.
    pub script: Vec<CmdFault>,
    /// No card in the slot: every command with a response times out.
    pub absent: bool,
    /// The card answers CMD8 with the wrong voltage bit.
    pub wrong_voltage: bool,
    /// The card stays busy (OCR bit 31 clear) through every ACMD41.
    pub never_ready: bool,
    /// A response CRC error on this command opcode (the first command with it).
    pub crc_on: Option<u32>,
    rintsts: u32,
    ctype: u32,
    pending_data: Option<u32>,
    busy: bool,
    arg: u32,
    cmd: u32,
    resp: [u32; 4],
    app: bool,
    ready_polls: u32,
    fifo_word: usize,
    write_buf: [u8; BLOCK],
    /// Commands seen so far.
    pub commands: u32,
    /// Opcodes seen, in order (the clock update is recorded as 0x80).
    pub opcodes: Vec<u32>,
}

impl SdCard {
    pub fn new(base: usize, blocks: usize) -> Self {
        Self {
            base,
            blocks: vec![[0u8; BLOCK]; blocks],
            script: Vec::new(),
            absent: false,
            wrong_voltage: false,
            never_ready: false,
            crc_on: None,
            rintsts: 0,
            ctype: 0,
            pending_data: None,
            busy: false,
            arg: 0,
            cmd: 0,
            resp: [0; 4],
            app: false,
            ready_polls: 0,
            fifo_word: 0,
            write_buf: [0; BLOCK],
            commands: 0,
            opcodes: Vec::new(),
        }
    }

    fn on_command(&mut self, value: u32) {
        let clock_update = value & dw_mmc::CMD_UPD_CLK != 0
            && value & CMD_INDEX_MASK == 0
            && value & RESP_EXP == 0;
        if clock_update {
            // The start bit clears at once; no card command.
            self.cmd = value & !dw_mmc::CMD_START;
            self.opcodes.push(0x80);
            return;
        }
        self.commands += 1;
        self.cmd = value;
        let index = value & CMD_INDEX_MASK;
        self.opcodes.push(index);
        let fault = if self.commands as usize <= self.script.len() {
            self.script[self.commands as usize - 1]
        } else {
            CmdFault::None
        };
        self.fifo_word = 0;
        let was_app = self.app;
        self.app = index == CMD_APP_CMD;

        if self.absent {
            if value & RESP_EXP != 0 {
                self.rintsts |= dw_mmc::INT_RTO_BIT;
            } else {
                self.rintsts |= dw_mmc::INT_CMD_DONE;
            }
            return;
        }
        if Some(index) == self.crc_on && !was_app {
            self.rintsts |= dw_mmc::INT_RCRC;
            return;
        }
        if fault == CmdFault::CommandTimeout {
            self.rintsts |= dw_mmc::INT_RTO_BIT;
            return;
        }
        if fault == CmdFault::CommandCrc {
            self.rintsts |= dw_mmc::INT_RCRC;
            return;
        }

        self.resp = [0; 4];
        match (index, was_app) {
            (CMD_SEND_IF_COND, false) => {
                // R7: the pattern echoes; the voltage bit is the host's offer, unless the card refuses it.
                let echo = self.arg & 0xFFF;
                self.resp[0] = if self.wrong_voltage {
                    echo & !0x100
                } else {
                    echo
                };
            }
            (CMD_APP_CMD, _) => self.resp[0] = 0x20,
            (CMD_APP_OP_COND, true) => {
                // R3: OCR, busy until the card is ready (bit 31 set when ready).
                self.ready_polls += 1;
                let ready = !self.never_ready && self.ready_polls >= 2;
                self.resp[0] = if ready {
                    0x80FF_8000 | (self.arg & (1 << 30))
                } else {
                    0x00FF_8000
                };
            }
            (CMD_SEND_OP_COND_2, false) => {
                self.resp = [0x1234_5678, 0x9abc_def0, 0x1357_9bdf, 0x2468_ace0]
            }
            (CMD_SEND_RCA, false) => self.resp[0] = 0x1234 << 16,
            (CMD_SELECT, false) | (CMD_SET_BUS_WIDTH, true) => self.resp[0] = 0x900,
            _ => {}
        }
        // A command that answers with long response data has its four words.
        let _ = RESP_LONG;
        let _ = RESP_CRC;
        if matches!(index, CMD_READ_SINGLE | CMD_WRITE_BLOCK) && !was_app {
            self.busy = matches!(fault, CmdFault::Busy);
            self.rintsts |= dw_mmc::INT_CMD_DONE;
            self.pending_data = Some(match fault {
                CmdFault::DataTimeout => dw_mmc::INT_DRTO,
                CmdFault::DataCrc => dw_mmc::INT_DCRC,
                _ => dw_mmc::INT_DATA_OVER,
            });
            return;
        }
        if matches!(fault, CmdFault::Busy) {
            self.busy = true;
        }
        self.rintsts |= dw_mmc::INT_CMD_DONE;
    }
}

impl RegAccess for SdCard {
    fn read32(&mut self, addr: usize) -> u32 {
        let offset = addr.wrapping_sub(self.base);
        match offset {
            dw_mmc::RINTSTS => self.rintsts,
            dw_mmc::STATUS => {
                if self.busy {
                    dw_mmc::STATUS_DATA_BUSY
                } else {
                    0
                }
            }
            dw_mmc::CMD => self.cmd & !dw_mmc::CMD_START,
            dw_mmc::CTYPE => self.ctype,
            o if (dw_mmc::RESP0..dw_mmc::RESP0 + 16).contains(&o) => {
                self.resp[(o - dw_mmc::RESP0) / 4]
            }
            dw_mmc::DATA => {
                let index = self.cmd & CMD_INDEX_MASK;
                let block = if index == CMD_READ_SINGLE {
                    self.blocks.get(self.arg as usize).copied()
                } else {
                    None
                };
                let word = self.fifo_word;
                self.fifo_word += 1;
                match block {
                    Some(block) if word < BLOCK / 4 => u32::from_le_bytes([
                        block[4 * word],
                        block[4 * word + 1],
                        block[4 * word + 2],
                        block[4 * word + 3],
                    ]),
                    _ => 0,
                }
            }
            _ => 0,
        }
    }

    fn write32(&mut self, addr: usize, value: u32) {
        match addr.wrapping_sub(self.base) {
            dw_mmc::CMDARG => self.arg = value,
            dw_mmc::CTYPE => self.ctype = value,
            dw_mmc::CMD => self.on_command(value),
            dw_mmc::RINTSTS => {
                self.rintsts &= !value;
                // The data phase's status appears once the command-done status is cleared.
                if self.rintsts & dw_mmc::INT_CMD_DONE == 0
                    && let Some(bits) = self.pending_data.take()
                {
                    if self.cmd & CMD_INDEX_MASK == CMD_WRITE_BLOCK {
                        self.pending_data = Some(bits);
                    } else {
                        self.rintsts |= bits;
                    }
                }
            }
            dw_mmc::DATA => {
                let word = self.fifo_word;
                self.fifo_word += 1;
                if word < BLOCK / 4 {
                    self.write_buf[4 * word..4 * word + 4].copy_from_slice(&value.to_le_bytes());
                }
                if self.fifo_word == BLOCK / 4 && self.cmd & CMD_INDEX_MASK == CMD_WRITE_BLOCK {
                    let fault_free = self.pending_data == Some(dw_mmc::INT_DATA_OVER);
                    if fault_free && let Some(block) = self.blocks.get_mut(self.arg as usize) {
                        *block = self.write_buf;
                    }
                    if let Some(bits) = self.pending_data.take() {
                        self.rintsts |= bits;
                    }
                }
            }
            _ => {}
        }
    }
}

/// An endpoint controller behind DEPCMD and DALEPENA. A command completes at once unless its
/// endpoint is stuck, in which case CMDACT stays set and the wait times out.
pub struct DwcEndpoints {
    base: usize,
    /// Endpoints whose commands never complete.
    pub stuck: Vec<usize>,
    pub depcmd: [u32; 16],
    pub depcmdpar0: [u32; 16],
    pub depcmdpar1: [u32; 16],
    pub dalepena: u32,
    /// Commands written, as (endpoint, command) in order.
    pub commands: Vec<(usize, u32)>,
}

impl DwcEndpoints {
    pub fn new(base: usize) -> Self {
        Self {
            base,
            stuck: Vec::new(),
            depcmd: [0; 16],
            depcmdpar0: [0; 16],
            depcmdpar1: [0; 16],
            dalepena: 0,
            commands: Vec::new(),
        }
    }

    /// The endpoint and the register at `offset`, if `offset` is one of an endpoint's registers.
    fn endpoint_of(offset: usize) -> Option<(usize, usize)> {
        let start = dwc3::DEP_BASE_START;
        let end = start + 16 * dwc3::DEP_STRIDE;
        if !(start..end).contains(&offset) {
            return None;
        }
        Some((
            (offset - start) / dwc3::DEP_STRIDE,
            (offset - start) % dwc3::DEP_STRIDE,
        ))
    }
}

impl RegAccess for DwcEndpoints {
    fn read32(&mut self, addr: usize) -> u32 {
        let offset = addr.wrapping_sub(self.base);
        if offset == dwc3::DALEPENA {
            return self.dalepena;
        }
        match Self::endpoint_of(offset) {
            Some((ep, 0x0c)) => self.depcmd[ep],
            Some((ep, 0x08)) => self.depcmdpar0[ep],
            Some((ep, 0x04)) => self.depcmdpar1[ep],
            _ => 0,
        }
    }

    fn write32(&mut self, addr: usize, value: u32) {
        let offset = addr.wrapping_sub(self.base);
        if offset == dwc3::DALEPENA {
            self.dalepena = value;
            return;
        }
        match Self::endpoint_of(offset) {
            Some((ep, 0x0c)) => {
                self.commands.push((ep, value & 0x0f));
                if value & dwc3::DEPCMD_CMDACT != 0 && self.stuck.contains(&ep) {
                    self.depcmd[ep] = value;
                } else {
                    self.depcmd[ep] = value & !dwc3::DEPCMD_CMDACT;
                }
            }
            Some((ep, 0x08)) => self.depcmdpar0[ep] = value,
            Some((ep, 0x04)) => self.depcmdpar1[ep] = value,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::{
        IoFault, configure_bulk_endpoint, sd_read_block, sd_write_block, start_transfer,
    };
    use crate::sd::{Card, InitError, sd_init};
    use lemnos_flash_stage::wait::IterClock;

    const SD: usize = 0xfe2b_0000;
    /// One second: the bound the tests give a single wait.
    const IO_WAIT_US: u64 = 1_000_000;

    /// A block's deadline: one bounded wait from now, the same bound the block had before storage budgets existed.
    fn until_in(clock: &mut IterClock) -> u64 {
        use lemnos_flash_stage::wait::Clock;
        clock.now_us() + IO_WAIT_US
    }
    const DWC: usize = 0xfcc0_0000;

    fn card_with(blocks: usize) -> SdCard {
        let mut card = SdCard::new(SD, blocks);
        card.blocks[3] = [0x5A; BLOCK];
        card
    }

    #[test]
    fn a_clean_read_returns_the_block() {
        let mut card = card_with(8);
        let mut clock = IterClock::new();
        assert_eq!(
            {
                let d = until_in(&mut clock);
                sd_read_block(&mut card, &mut clock, SD, 3, d)
            },
            Ok([0x5A; BLOCK])
        );
    }

    #[test]
    fn a_command_with_no_response_times_out_as_a_command_timeout() {
        let mut card = card_with(8);
        card.script = vec![CmdFault::CommandTimeout];
        let mut clock = IterClock::new();
        assert_eq!(
            {
                let d = until_in(&mut clock);
                sd_read_block(&mut card, &mut clock, SD, 3, d)
            },
            Err(IoFault::CommandTimeout)
        );
    }

    #[test]
    fn a_data_phase_timeout_and_a_data_crc_error_are_reported_as_such() {
        let mut clock = IterClock::new();
        let mut card = card_with(8);
        card.script = vec![CmdFault::DataTimeout];
        assert_eq!(
            {
                let d = until_in(&mut clock);
                sd_read_block(&mut card, &mut clock, SD, 3, d)
            },
            Err(IoFault::DataTimeout)
        );
        let mut card = card_with(8);
        card.script = vec![CmdFault::DataCrc];
        assert_eq!(
            {
                let d = until_in(&mut clock);
                sd_read_block(&mut card, &mut clock, SD, 3, d)
            },
            Err(IoFault::DataCrc)
        );
    }

    #[test]
    fn a_card_that_answers_busy_and_never_finishes_is_a_card_busy() {
        let mut card = card_with(8);
        card.script = vec![CmdFault::Busy];
        let mut clock = IterClock::new();
        assert_eq!(
            {
                let d = until_in(&mut clock);
                sd_read_block(&mut card, &mut clock, SD, 3, d)
            },
            Err(IoFault::CardBusy)
        );
    }

    #[test]
    fn a_write_lands_in_the_card_and_a_read_gets_it_back() {
        let mut card = card_with(8);
        let mut clock = IterClock::new();
        {
            let d = until_in(&mut clock);
            sd_write_block(&mut card, &mut clock, SD, 5, &[0xC3; BLOCK], d)
        }
        .unwrap();
        assert_eq!(card.blocks[5], [0xC3; BLOCK]);
        assert_eq!(
            {
                let d = until_in(&mut clock);
                sd_read_block(&mut card, &mut clock, SD, 5, d)
            },
            Ok([0xC3; BLOCK])
        );
    }

    #[test]
    fn a_write_whose_data_phase_fails_does_not_reach_the_card() {
        let mut card = card_with(8);
        card.script = vec![CmdFault::DataCrc];
        let mut clock = IterClock::new();
        assert_eq!(
            {
                let d = until_in(&mut clock);
                sd_write_block(&mut card, &mut clock, SD, 5, &[0xC3; BLOCK], d)
            },
            Err(IoFault::DataCrc)
        );
        assert_eq!(card.blocks[5], [0u8; BLOCK], "nothing was stored");
    }

    #[test]
    fn a_stuck_endpoint_command_is_a_timeout_and_a_clean_one_completes() {
        let mut dwc = DwcEndpoints::new(DWC);
        dwc.stuck = vec![2];
        let mut clock = IterClock::new();
        assert_eq!(
            configure_bulk_endpoint(&mut dwc, &mut clock, DWC, 2, 512, 1, 1_000_000),
            Err(IoFault::Timeout)
        );
        assert_eq!(
            // The stuck command used the first second, so the clean one has its own second after it.
            configure_bulk_endpoint(&mut dwc, &mut clock, DWC, 3, 512, 1, 2_000_000),
            Ok(())
        );
        assert_eq!(dwc.dalepena & (1 << 3), 1 << 3, "endpoint 3 enabled");
        assert_eq!(
            dwc.dalepena & (1 << 2),
            0,
            "the stuck endpoint was not enabled"
        );
    }

    #[test]
    fn a_transfer_start_writes_the_trb_address_high_word_first() {
        let mut dwc = DwcEndpoints::new(DWC);
        let mut clock = IterClock::new();
        let deadline = 1_000_000;
        start_transfer(&mut dwc, &mut clock, DWC, 3, 0x1_2345_6780, deadline).unwrap();
        assert_eq!(dwc.depcmdpar0[3], 0x1);
        assert_eq!(dwc.depcmdpar1[3], 0x2345_6780);
        assert_eq!(
            dwc.commands,
            vec![(3, crate::regs::dwc3::DEPCMD_STARTTRANSFER)]
        );
    }

    #[test]
    fn the_identification_sequence_brings_a_card_to_transfer_state_in_4_bit() {
        let mut card = card_with(8);
        let mut clock = IterClock::new();
        let got = sd_init(&mut card, &mut clock, SD, 1_000_000).unwrap();
        assert_eq!(
            got,
            Card {
                rca: 0x1234,
                high_capacity: true
            }
        );
        assert_eq!(
            card.opcodes[..4],
            [0x80, 0, 8, 55],
            "clock update, go idle, interface condition, app command"
        );
        assert_eq!(card.read32(SD + dw_mmc::CTYPE), 1, "4-bit bus");
    }

    #[test]
    fn no_card_in_the_slot_is_a_no_response_at_the_first_command_that_answers() {
        let mut card = card_with(8);
        card.absent = true;
        let mut clock = IterClock::new();
        assert_eq!(
            sd_init(&mut card, &mut clock, SD, 1_000_000),
            Err(InitError::NoResponse)
        );
    }

    #[test]
    fn a_card_that_refuses_the_voltage_is_a_wrong_voltage() {
        let mut card = card_with(8);
        card.wrong_voltage = true;
        let mut clock = IterClock::new();
        assert_eq!(
            sd_init(&mut card, &mut clock, SD, 1_000_000),
            Err(InitError::WrongVoltage)
        );
    }

    #[test]
    fn a_card_that_never_leaves_busy_is_never_ready_after_the_bounded_polls() {
        let mut card = card_with(8);
        card.never_ready = true;
        let mut clock = IterClock::new();
        assert_eq!(
            sd_init(&mut card, &mut clock, SD, 1_000_000),
            Err(InitError::NeverReady)
        );
        let polls = card.opcodes.iter().filter(|op| **op == 41).count();
        assert_eq!(
            polls as u32,
            crate::sd::ACMD41_POLLS,
            "every poll was made, then the bound stopped it"
        );
    }

    #[test]
    fn a_response_crc_error_on_the_interface_condition_is_a_response_crc() {
        let mut card = card_with(8);
        card.crc_on = Some(8);
        let mut clock = IterClock::new();
        assert_eq!(
            sd_init(&mut card, &mut clock, SD, 1_000_000),
            Err(InitError::ResponseCrc)
        );
    }

    #[test]
    fn a_reset_that_never_clears_stops_the_init_before_any_card_command() {
        struct Stuck;
        impl RegAccess for Stuck {
            fn read32(&mut self, _addr: usize) -> u32 {
                crate::regs::dw_mmc::CTRL_RESET
            }
            fn write32(&mut self, _addr: usize, _value: u32) {}
        }
        let mut clock = IterClock::new();
        assert_eq!(
            sd_init(&mut Stuck, &mut clock, SD, 1_000_000),
            Err(InitError::Timeout)
        );
    }
}
