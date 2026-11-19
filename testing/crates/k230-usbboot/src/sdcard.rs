//! The rehearsal's SD controller and the card in its slot, for the B2 scratch build (`b2sim`): the scratch build's
//! own SD host (`lemnos_driver_k230_usbboot_sys::sdhci::SdhciHost`) runs over [`SimSdhci`], which holds a
//! [`SimCard`] of the card of record's size and identity.
//!
//! The controller is the stand-in the SD host's own tests use (`sdhci.rs`), as the bench found the board's on
//! 2026-11-18: no command completes until the clock word has what the boot ROM's working state had; a full reset
//! puts the conflict check on and every command then ends as a conflict until the host clears it; a status
//! appears only when its enable is set; a data command whose direction or block size is not set up ends in the
//! data timeout error; a block past the card's end is refused. The card follows the protocol (CMD0, CMD8, ACMD41,
//! CMD2, CMD3, CMD9, CMD10, CMD7, CMD16, CMD17, CMD24, CMD13, CMD12), is busy for a while after each write, and
//! has a knob for each way the write path failed on the bench or can fail on a card.
//!
//! The card is sparse: a sector never written reads as its base content ([`SimCard::base`]): the primary GPT
//! header in sector 1 (valid, its backup at 415,743, as the card of record's), zeros in the scratch range (as the
//! survey found it), and elsewhere a pattern of its sector number, never zero. Every block write is logged.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use lemnos_driver_k230_usbboot_sys::sdhci::SdhciRegs;
use lemnos_k230_usbboot_stage::bounds::CARD_DECLARED;
use lemnos_k230_usbboot_stage::fl1::scratch::{CARD_OF_RECORD_BLOCKS, CARD_OF_RECORD_CID};
use lemnos_k230_usbboot_stage::format::crc32;
use sha2::{Digest, Sha256};

/// A sector's bytes.
pub const BLOCK: usize = 512;
/// The card of record's backup GPT header (its primary header's alternate), and its first entry sector.
pub const BACKUP_GPT_HEADER: u64 = 415_743;
pub const BACKUP_GPT_FIRST: u64 = 415_711;

/// The card in the slot.
#[derive(Clone, Debug)]
pub struct SimCard {
    /// Its CID as the card sends it (128 bits, the CRC byte 0).
    pub cid: [u8; 16],
    /// Its size in blocks.
    pub blocks: u64,
    written: BTreeMap<u64, [u8; BLOCK]>,
    /// Every block write the card took, by sector, in order (a dropped write included).
    pub writes: Vec<u64>,
    /// The card takes the data and keeps its old content (a worn or locked card).
    pub drops_writes: bool,
    /// Writes to this sector fail their data CRC this many times (the bench's once in 14,000 at 25 MHz): the
    /// block is dropped and the card waits for a stop (CMD12) before the next data command.
    pub crc_fails: Option<(u64, u32)>,
}

impl SimCard {
    /// The card of record: its CID and size (`fl1::scratch`).
    pub fn of_record() -> Self {
        Self {
            cid: CARD_OF_RECORD_CID,
            blocks: CARD_OF_RECORD_BLOCKS,
            written: BTreeMap::new(),
            writes: Vec::new(),
            drops_writes: false,
            crc_fails: None,
        }
    }

    /// Another card of the same make and size: its serial number differs.
    pub fn another() -> Self {
        let mut card = Self::of_record();
        card.cid[10] ^= 0x40;
        card
    }

    /// The card as one shared value, for the board and the rehearsal's own reads.
    pub fn shared(self) -> Arc<Mutex<SimCard>> {
        Arc::new(Mutex::new(self))
    }

    /// The base content of sector `lba` (what it holds until written).
    pub fn base(lba: u64) -> [u8; BLOCK] {
        if lba == 1 {
            return gpt_header();
        }
        if (CARD_DECLARED.first..=CARD_DECLARED.last).contains(&lba) {
            return [0; BLOCK];
        }
        let mut out = [0u8; BLOCK];
        let seed = lba.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = ((seed >> ((i % 8) * 8)) as u8 ^ i as u8) | 1;
        }
        out
    }

    /// Sector `lba` as the card holds it now.
    pub fn sector(&self, lba: u64) -> [u8; BLOCK] {
        self.written
            .get(&lba)
            .copied()
            .unwrap_or_else(|| Self::base(lba))
    }

    /// The SHA-256 of `count` sectors from `first`, as the piece path's `hash` job gives it.
    pub fn hash(&self, first: u64, count: u64) -> String {
        let mut digest = Sha256::new();
        for lba in first..first + count {
            match self.written.get(&lba) {
                Some(sector) => digest.update(sector),
                None => digest.update(Self::base(lba)),
            }
        }
        crate::hex(&digest.finalize())
    }

    fn store(&mut self, lba: u64, data: [u8; BLOCK]) {
        self.writes.push(lba);
        if !self.drops_writes {
            self.written.insert(lba, data);
        }
    }
}

/// The regions the plan hashes before and after the card writes (`PLAN-card.md` step 1), by the plan's ids: what
/// the piece path's `hash` jobs read on the board. (id, first sector, sectors.)
pub fn plan_regions() -> [(&'static str, u64, u64); 8] {
    let (first, last) = (CARD_DECLARED.first, CARD_DECLARED.last);
    [
        ("O1 the image region 0..415743", 0, BACKUP_GPT_HEADER + 1),
        ("O2 the primary GPT 0..33", 0, 34),
        ("O3 the backup GPT 415711..415743", BACKUP_GPT_FIRST, 33),
        ("O4 the boot area 34..8191", 34, 8158),
        (
            "O5 the card's last 33 sectors",
            CARD_OF_RECORD_BLOCKS - 33,
            33,
        ),
        ("O6 the 8 sectors before the range", first - 8, 8),
        ("O7 the 8 sectors after the range", last + 1, 8),
        ("R0 the scratch range", first, last - first + 1),
    ]
}

/// The plan's hashes of `card`, by region id.
pub fn plan_hashes(card: &SimCard) -> Vec<(&'static str, String)> {
    plan_regions()
        .iter()
        .map(|&(id, first, count)| (id, card.hash(first, count)))
        .collect()
}

/// The card of record's primary GPT header, as its partition table has it: 128 entries of 128 bytes from
/// sector 2, its backup at [`BACKUP_GPT_HEADER`], with a valid CRC.
pub fn gpt_header() -> [u8; BLOCK] {
    let mut h = [0u8; BLOCK];
    h[..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&1u64.to_le_bytes());
    h[32..40].copy_from_slice(&BACKUP_GPT_HEADER.to_le_bytes());
    h[40..48].copy_from_slice(&34u64.to_le_bytes());
    h[48..56].copy_from_slice(&415_710u64.to_le_bytes());
    h[56..72].copy_from_slice(b"lemnos-sim-disk!");
    h[72..80].copy_from_slice(&2u64.to_le_bytes());
    h[80..84].copy_from_slice(&128u32.to_le_bytes());
    h[84..88].copy_from_slice(&128u32.to_le_bytes());
    let crc = crc32(&h[..92]);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h
}

// The SDHCI standard offsets and bits the host uses (`sdhci.rs`).
const ARGUMENT: usize = 0x08;
const BLOCK_SIZE: usize = 0x04;
const TRANSFER_MODE: usize = 0x0c;
const RESPONSE: usize = 0x10;
const BUFFER: usize = 0x20;
const PRESENT_STATE: usize = 0x24;
const CLOCK_CONTROL: usize = 0x2c;
const MSHC_CTRL: usize = 0x508;
const INT_STATUS: usize = 0x30;
const INT_ENABLE: usize = 0x34;
const CAPABILITIES: usize = 0x40;
const INT_CMD_DONE: u32 = 1 << 0;
const INT_XFER_DONE: u32 = 1 << 1;
const INT_WRITE_READY: u32 = 1 << 4;
const INT_READ_READY: u32 = 1 << 5;
const INT_ERROR: u32 = 1 << 15;
const CMD_INHIBIT: u32 = 1 << 0;
const DAT_INHIBIT: u32 = 1 << 1;
const CLOCK_INT_EN: u32 = 0x0001;
const CLOCK_INT_STABLE: u32 = 0x0002;
const CLOCK_CARD_EN: u32 = 0x0004;
const CLOCK_PLL_EN: u32 = 0x0008;
const CLOCK_PROG: u32 = 0x0020;
const RCA: u32 = 0x1234;
/// Status polls a write keeps the card busy for, at the identification clock's divider (a poll at a faster
/// clock uses up less of it).
const BUSY_POLLS: u32 = 4 * 125;

/// The controller, holding the card's protocol state; the card's content is shared.
pub struct SimSdhci {
    words: [Cell<u32>; 0x150],
    card: Arc<Mutex<SimCard>>,
    divider_in_effect: Cell<u32>,
    acmd: Cell<bool>,
    to_read: RefCell<VecDeque<u32>>,
    writing: RefCell<Option<(u64, Vec<u32>)>>,
    busy_left: Cell<u32>,
    waits_for_stop: Cell<bool>,
}

impl SimSdhci {
    /// The slot's controller (a 100 MHz base, as read on the bench) with `card` in it.
    pub fn new(card: Arc<Mutex<SimCard>>) -> Self {
        let model = Self {
            words: [const { Cell::new(0) }; 0x150],
            card,
            divider_in_effect: Cell::new(0),
            acmd: Cell::new(false),
            to_read: RefCell::new(VecDeque::new()),
            writing: RefCell::new(None),
            busy_left: Cell::new(0),
            waits_for_stop: Cell::new(false),
        };
        model.words[CAPABILITIES / 4].set(0x276a_6481);
        model
    }

    fn card(&self) -> MutexGuard<'_, SimCard> {
        self.card.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn word(&self, offset: usize) -> &Cell<u32> {
        &self.words[offset / 4]
    }

    fn card_clock_runs(&self) -> bool {
        let clock = self.word(CLOCK_CONTROL).get();
        let need = CLOCK_INT_EN | CLOCK_CARD_EN | CLOCK_PLL_EN | CLOCK_PROG;
        clock & need == need && (clock >> 8) & 0xff != 0
    }

    fn raise(&self, raw: u32) {
        let mut status = raw & self.word(INT_ENABLE).get();
        if status >> 16 != 0 {
            status |= INT_ERROR;
        }
        let word = self.word(INT_STATUS);
        word.set(word.get() | status);
    }

    fn reply(&self, value: u32) {
        self.word(RESPONSE).set(value);
        self.raise(INT_CMD_DONE);
    }

    fn long_reply(&self, value: u128) {
        let held = value >> 8;
        for i in 0..4 {
            self.words[RESPONSE / 4 + i].set((held >> (32 * i)) as u32);
        }
        self.raise(INT_CMD_DONE);
    }

    fn issue(&self, value: u32) {
        if !self.card_clock_runs() {
            self.word(PRESENT_STATE).set(CMD_INHIBIT);
            return;
        }
        if self.word(MSHC_CTRL).get() & 1 != 0 {
            self.raise(0x3 << 16);
            return;
        }
        let index = (value >> 24) & 0x3f;
        if (index == 17 || index == 24) && self.word(PRESENT_STATE).get() & DAT_INHIBIT != 0 {
            self.word(PRESENT_STATE).set(CMD_INHIBIT | DAT_INHIBIT);
            return;
        }
        let data_flag = (value >> 16) & 0x20 != 0;
        let reads = value & 0x10 != 0;
        let one_block = self.word(BLOCK_SIZE).get() == (1 << 16) | BLOCK as u32;
        let arg = self.word(ARGUMENT).get();
        let acmd = self.acmd.replace(false);
        let (cid, blocks) = {
            let card = self.card();
            (card.cid, card.blocks)
        };
        match index {
            0 => self.raise(INT_CMD_DONE),
            12 => {
                self.waits_for_stop.set(false);
                self.reply(0x900);
            }
            17 | 24 if self.waits_for_stop.get() => self.raise(1 << 16),
            8 => self.reply(arg),
            55 => {
                self.acmd.set(true);
                self.reply(0x120);
            }
            41 if acmd => self.reply(0xc0ff_8000),
            2 | 10 => self.long_reply(u128::from_be_bytes(cid)),
            3 => self.reply(RCA << 16),
            9 => self.long_reply((1u128 << 126) | (u128::from(blocks / 1024 - 1) << 48)),
            7 | 16 => self.reply(0x900),
            13 => {
                let ready = self.busy_left.get() == 0;
                self.busy_left.set(
                    self.busy_left
                        .get()
                        .saturating_sub(self.divider_in_effect.get().max(1)),
                );
                self.reply(if ready { 0x900 } else { 0xe00 });
            }
            17 | 24
                if !data_flag
                    || !one_block
                    || reads != (index == 17)
                    || u64::from(arg) >= blocks =>
            {
                self.raise(INT_CMD_DONE | (1 << 20));
            }
            17 => {
                let sector = self.card().sector(u64::from(arg));
                *self.to_read.borrow_mut() = sector
                    .chunks(4)
                    .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
                    .collect();
                self.word(PRESENT_STATE).set(DAT_INHIBIT);
                self.reply(0x900);
                self.raise(INT_READ_READY);
            }
            24 => {
                *self.writing.borrow_mut() = Some((u64::from(arg), Vec::new()));
                self.word(PRESENT_STATE).set(DAT_INHIBIT);
                self.reply(0x900);
                self.raise(INT_WRITE_READY);
            }
            _ => self.raise(1 << 16),
        }
    }

    fn take_word(&self, value: u32) {
        let mut writing = self.writing.borrow_mut();
        let Some((sector, words)) = writing.as_mut() else {
            return;
        };
        words.push(value);
        if words.len() < BLOCK / 4 {
            return;
        }
        let sector = *sector;
        let mut data = [0u8; BLOCK];
        for (i, word) in words.iter().enumerate() {
            data[4 * i..4 * i + 4].copy_from_slice(&word.to_le_bytes());
        }
        *writing = None;
        let mut card = self.card();
        if let Some((bad, left)) = card.crc_fails.as_mut()
            && *bad == sector
            && *left > 0
        {
            *left -= 1;
            drop(card);
            self.waits_for_stop.set(true);
            self.raise(1 << 21);
            return;
        }
        card.store(sector, data);
        drop(card);
        self.busy_left.set(BUSY_POLLS);
        self.word(PRESENT_STATE).set(0);
        self.raise(INT_XFER_DONE);
    }
}

impl SdhciRegs for SimSdhci {
    fn read(&self, offset: usize) -> u32 {
        let Some(cell) = self.words.get(offset / 4) else {
            return 0;
        };
        let word = cell.get();
        match offset {
            CLOCK_CONTROL if word & CLOCK_INT_EN != 0 => word | CLOCK_INT_STABLE,
            BUFFER => {
                let mut to_read = self.to_read.borrow_mut();
                let value = to_read.pop_front().unwrap_or(0);
                if to_read.is_empty() {
                    self.word(PRESENT_STATE).set(0);
                    self.raise(INT_XFER_DONE);
                }
                value
            }
            _ => word,
        }
    }

    fn write(&self, offset: usize, value: u32) {
        if offset / 4 >= self.words.len() {
            return;
        }
        match offset {
            // The reset bits clear themselves. A full reset puts the conflict check back on; a reset of the data
            // line lets go of it.
            CLOCK_CONTROL => {
                if value & (1 << 24) != 0 {
                    self.word(MSHC_CTRL).set(1);
                }
                if value & (1 << 26) != 0 {
                    self.word(PRESENT_STATE).set(0);
                    *self.writing.borrow_mut() = None;
                }
                let was = self.word(offset).get();
                if value & !was & CLOCK_PROG != 0 {
                    self.divider_in_effect.set((value >> 8) & 0xff);
                }
                self.word(offset).set(value & 0x00ff_ffff);
            }
            INT_STATUS => {
                let word = self.word(offset);
                word.set(word.get() & !value);
            }
            TRANSFER_MODE => self.issue(value),
            BUFFER => self.take_word(value),
            _ => self.word(offset).set(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_driver_k230_usbboot_sys::sdhci::SdhciHost;
    use lemnos_k230_usbboot_stage::bounds::{Bounds, Declared};
    use lemnos_k230_usbboot_stage::sd::Card;

    #[test]
    fn the_sd_host_starts_the_card_and_reads_and_writes_it_through_the_controller() {
        let shared = SimCard::of_record().shared();
        let mut card =
            Card::init(SdhciHost::start(SimSdhci::new(shared.clone())).unwrap()).unwrap();
        assert_eq!(card.blocks(), CARD_OF_RECORD_BLOCKS);
        assert_eq!(card.cid(), CARD_OF_RECORD_CID);
        card.host_mut().data_clock().unwrap();
        let mut block = [0u8; BLOCK];
        card.read(1, &mut block).unwrap();
        assert_eq!(block, gpt_header());
        let data = [0xa5u8; BLOCK];
        card.write(CARD_DECLARED.first, &data).unwrap();
        card.read(CARD_DECLARED.first, &mut block).unwrap();
        assert_eq!(block, data);
        assert_eq!(shared.lock().unwrap().writes, vec![CARD_DECLARED.first]);
    }

    #[test]
    fn the_header_is_one_the_bounds_take_with_its_backup_where_the_card_of_records_is() {
        let bounds = Bounds::new(
            Declared {
                first: 0,
                last: u64::MAX,
            },
            &gpt_header(),
            CARD_OF_RECORD_BLOCKS,
        );
        assert_eq!(bounds.backup(), Some((BACKUP_GPT_FIRST, BACKUP_GPT_HEADER)));
    }

    #[test]
    fn the_scratch_range_starts_as_zeros_and_every_other_sector_is_not_zero() {
        let card = SimCard::of_record();
        for lba in [CARD_DECLARED.first, CARD_DECLARED.last] {
            assert_eq!(card.sector(lba), [0; BLOCK]);
        }
        for lba in [
            0,
            2048,
            3072,
            CARD_DECLARED.first - 1,
            CARD_DECLARED.last + 1,
            BACKUP_GPT_HEADER,
        ] {
            assert!(card.sector(lba).iter().all(|&b| b != 0), "{lba}");
        }
    }
}
