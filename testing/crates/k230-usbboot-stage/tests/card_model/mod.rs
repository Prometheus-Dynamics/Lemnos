//! The card of record as the stage's own SD code sees it: a sparse SD host (`ModelHost`) answering the commands
//! `sd::Card` sends as a healthy SDHC card of 124,735,488 blocks. Its sector 1 is a valid primary GPT header with
//! the backup at 415,711..415,743, and every protected sector holds a pattern. Shared by the card-bounds tests
//! (`card_bounds.rs`) and the flash-extent tests (`flash_extent.rs`).

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use lemnos_k230_usbboot_stage::format::crc32;
use lemnos_k230_usbboot_stage::sd::{BLOCK, Card, Expect, Reply, SdError, SdHost};

/// The card of record's size.
pub const BLOCKS: u64 = 124_735_488;
/// Where the image's primary GPT header puts the backup: entries from 415,711, header at 415,743.
pub const BACKUP_HEADER: u64 = 415_743;
pub const BACKUP_FIRST: u64 = 415_711;

/// The sectors no card stage may change, and the pattern each holds.
pub fn protected() -> Vec<u64> {
    let mut out: Vec<u64> = (0..34).collect();
    out.extend([2048, 3072]);
    out.extend(BACKUP_FIRST..=BACKUP_HEADER);
    out.extend(BLOCKS - 33..BLOCKS);
    out
}

pub fn pattern(lba: u64) -> [u8; BLOCK] {
    let mut out = [0x5a; BLOCK];
    out[..8].copy_from_slice(&lba.to_le_bytes());
    out
}

/// The image's primary GPT header (sector 1), with a valid CRC, backup at `alt`.
pub fn gpt_header(alt: u64) -> [u8; BLOCK] {
    let mut h = [0u8; BLOCK];
    h[..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&1u64.to_le_bytes());
    h[32..40].copy_from_slice(&alt.to_le_bytes());
    h[40..48].copy_from_slice(&34u64.to_le_bytes());
    h[48..56].copy_from_slice(&(alt - 33).to_le_bytes());
    h[72..80].copy_from_slice(&2u64.to_le_bytes());
    h[80..84].copy_from_slice(&128u32.to_le_bytes());
    h[84..88].copy_from_slice(&128u32.to_le_bytes());
    let crc = crc32(&h[..92]);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h
}

/// A sparse SD card behind the SD host interface: the commands `sd::Card` sends, answered as a healthy SDHC card.
#[derive(Clone)]
pub struct ModelHost(Rc<RefCell<HostState>>);

struct HostState {
    sectors: BTreeMap<u64, [u8; BLOCK]>,
    writes: Vec<u64>,
    at: Option<(u8, u64)>,
    acmd: bool,
}

impl ModelHost {
    /// The card of record: the GPT header in sector 1, a pattern in every other protected sector.
    pub fn card_of_record() -> Self {
        let mut sectors = BTreeMap::new();
        for lba in protected() {
            sectors.insert(lba, pattern(lba));
        }
        sectors.insert(1, gpt_header(BACKUP_HEADER));
        Self(Rc::new(RefCell::new(HostState {
            sectors,
            writes: Vec::new(),
            at: None,
            acmd: false,
        })))
    }

    pub fn writes(&self) -> Vec<u64> {
        self.0.borrow().writes.clone()
    }

    pub fn sector(&self, lba: u64) -> [u8; BLOCK] {
        self.0
            .borrow()
            .sectors
            .get(&lba)
            .copied()
            .unwrap_or([0; BLOCK])
    }

    pub fn csd() -> [u8; 16] {
        let mut csd = [0u8; 16];
        csd[0] = 0x40;
        let c_size = BLOCKS / 1024 - 1;
        for i in 0..22 {
            if (c_size >> i) & 1 == 1 {
                let bit = 48 + i;
                csd[15 - (bit / 8) as usize] |= 1 << (bit % 8);
            }
        }
        csd
    }
}

impl SdHost for ModelHost {
    fn command(&mut self, index: u8, arg: u32, _expect: Expect) -> Result<Reply, SdError> {
        let mut s = self.0.borrow_mut();
        let was_acmd = std::mem::replace(&mut s.acmd, false);
        match (index, was_acmd) {
            (0, _) => Ok(Reply::None),
            (8, _) => Ok(Reply::Short(arg)),
            (55, _) => {
                s.acmd = true;
                Ok(Reply::Short(0x0120))
            }
            (41, true) => Ok(Reply::Short(0xc0ff_8000)),
            (2, _) => Ok(Reply::Long([0x11; 16])),
            (10, _) => Ok(Reply::Long([0x5c; 16])),
            (3, _) => Ok(Reply::Short(0x1234_0000)),
            (9, _) => Ok(Reply::Long(Self::csd())),
            (7, _) | (16, _) | (12, _) => Ok(Reply::Short(0x0000_0900)),
            (13, _) => Ok(Reply::Short(1 << 8)),
            (17, _) | (24, _) => {
                s.at = Some((index, u64::from(arg)));
                Ok(Reply::Short(0x0000_0900))
            }
            _ => Err(SdError::Timeout(index)),
        }
    }

    fn read_block(&mut self, out: &mut [u8; BLOCK]) -> Result<(), SdError> {
        let mut s = self.0.borrow_mut();
        let (_, lba) = s.at.take().ok_or(SdError::Data("no read"))?;
        *out = s.sectors.get(&lba).copied().unwrap_or([0; BLOCK]);
        Ok(())
    }

    fn write_block(&mut self, data: &[u8; BLOCK]) -> Result<(), SdError> {
        let mut s = self.0.borrow_mut();
        let (_, lba) = s.at.take().ok_or(SdError::Data("no write"))?;
        s.writes.push(lba);
        s.sectors.insert(lba, *data);
        Ok(())
    }
}

pub fn card(host: &ModelHost) -> Card<ModelHost> {
    Card::init(host.clone()).expect("the model card initialises")
}

pub fn untouched(host: &ModelHost) -> bool {
    protected().into_iter().all(|lba| {
        let want = if lba == 1 {
            gpt_header(BACKUP_HEADER)
        } else {
            pattern(lba)
        };
        host.sector(lba) == want
    })
}
