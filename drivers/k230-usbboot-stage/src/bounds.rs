//! The card stages' write bounds (CARD-STAGE-BOUNDS). No card stage writes or erases a sector unless it is inside the
//! range its build declares and touches none of the sectors the card's boot and partition table depend on, whatever
//! the host sends. The compared mark of the FL1 wire never overrides it.
//!
//! The protected sectors are:
//! - the protective MBR and the primary GPT: sectors 0 to 33;
//! - the K230 boot headers: sectors 2048 and 3072;
//! - the backup GPT, from the primary header the stage reads off the card: its `alternate LBA` (the backup header)
//!   and the sectors its entry array takes just before it. The header must be valid (signature, size, CRC-32, its
//!   own LBA 1, the backup inside the card; the standard 92-byte header only). A card whose header is not valid has no known backup, and every write
//!   is refused: a stage that cannot say where the backup is writes nothing;
//! - the card's last 33 sectors, where a GPT sized to the card keeps its backup, from the card's own size.
//!
//! A request is checked whole before the card is touched: if any one of its sectors is outside the range or
//! protected, none of it is written.

use crate::format::crc32;

/// A range of sectors a build may write, first and last inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Declared {
    pub first: u64,
    pub last: u64,
}

/// The range the card builds declare: the scratch range chosen from the card's own layout
/// (`testing/board-runs/K230-USB.md`, "B2 card writes"), 1 MiB at the 512 MiB mark.
pub const CARD_DECLARED: Declared = Declared {
    first: 1_048_576,
    last: 1_050_623,
};

/// The protective MBR and the primary GPT (header and 32 entry sectors): sectors 0 to 33.
pub const PRIMARY_GPT_LAST: u64 = 33;

/// The two sectors the boot ROM reads its first stage's headers from.
pub const BOOT_HEADERS: [u64; 2] = [2048, 3072];

/// The sectors a GPT sized to the card keeps its backup in: the last 33.
pub const CARD_END_SECTORS: u64 = 33;

/// The GPT header's standard size, the only one taken.
const HEADER_LEN: usize = 92;

/// The largest entry array a header may name (the GPT's own default is 128 entries of 128 bytes, 32 sectors).
const MAX_ENTRY_SECTORS: u64 = 256;

/// Why a range may not be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The card's primary GPT header is not valid, so the backup's place is not known.
    NoGpt,
    /// A sector is outside the declared range.
    Outside,
    /// A sector is one the card's boot or partition table depends on.
    Protected,
}

impl Refusal {
    pub fn text(self) -> &'static str {
        match self {
            Refusal::NoGpt => "the card's GPT header is not valid: no write",
            Refusal::Outside => "the range is outside the stage's declared range",
            Refusal::Protected => "the range touches a protected sector (GPT or boot header)",
        }
    }
}

/// The bounds of one card: the declared range and the protected sectors, found when the card is opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    declared: Declared,
    /// The backup GPT, first and last inclusive, from the primary header; `None` when the header is not valid.
    backup: Option<(u64, u64)>,
    /// The card's size in sectors.
    blocks: u64,
}

impl Bounds {
    /// The bounds of a card of `blocks` sectors whose sector 1 is `header`.
    pub fn new(declared: Declared, header: &[u8; 512], blocks: u64) -> Self {
        Self {
            declared,
            backup: backup_gpt(header, blocks),
            blocks,
        }
    }

    /// Bounds that refuse every write: a card whose sector 1 could not be read.
    pub fn closed(declared: Declared, blocks: u64) -> Self {
        Self {
            declared,
            backup: None,
            blocks,
        }
    }

    /// The backup GPT, first and last inclusive, as the card's primary header names it.
    pub fn backup(&self) -> Option<(u64, u64)> {
        self.backup
    }

    /// Whether `count` sectors at `lba` may be written or erased. `count` zero is refused.
    pub fn check(&self, lba: u64, count: u64) -> Result<(), Refusal> {
        let Some(last) = count.checked_sub(1).and_then(|n| lba.checked_add(n)) else {
            return Err(Refusal::Outside);
        };
        let touches = |first: u64, end: u64| lba <= end && last >= first;
        let Some((backup_first, backup_last)) = self.backup else {
            return Err(Refusal::NoGpt);
        };
        let card_end_first = self.blocks.saturating_sub(CARD_END_SECTORS);
        if touches(0, PRIMARY_GPT_LAST)
            || BOOT_HEADERS.iter().any(|&s| touches(s, s))
            || touches(backup_first, backup_last)
            || touches(card_end_first, u64::MAX)
        {
            return Err(Refusal::Protected);
        }
        if lba < self.declared.first || last > self.declared.last {
            return Err(Refusal::Outside);
        }
        Ok(())
    }
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from(le32(b, at)) | (u64::from(le32(b, at + 4)) << 32)
}

/// The backup GPT a valid primary header names: its entry array and its header, first and last inclusive. Only the
/// standard 92-byte header is taken; any other size is not valid here (fail closed).
fn backup_gpt(header: &[u8; 512], blocks: u64) -> Option<(u64, u64)> {
    if header[..8] != *b"EFI PART" || le32(header, 12) != HEADER_LEN as u32 || le64(header, 24) != 1
    {
        return None;
    }
    let mut copy = [0u8; HEADER_LEN];
    copy.copy_from_slice(&header[..HEADER_LEN]);
    copy[16..20].fill(0);
    if crc32(&copy) != le32(header, 16) {
        return None;
    }
    let alternate = le64(header, 32);
    let entries = le32(header, 80).checked_mul(le32(header, 84))?;
    let entry_sectors = u64::from(entries.div_ceil(512));
    if entry_sectors == 0 || entry_sectors > MAX_ENTRY_SECTORS {
        return None;
    }
    if alternate >= blocks || alternate <= PRIMARY_GPT_LAST + entry_sectors {
        return None;
    }
    Some((alternate - entry_sectors, alternate))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCKS: u64 = 124_735_488;

    fn header(alternate: u64) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[..8].copy_from_slice(b"EFI PART");
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&1u64.to_le_bytes());
        h[32..40].copy_from_slice(&alternate.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        let crc = crc32(&h[..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h
    }

    #[test]
    fn the_backup_is_where_the_primary_header_puts_it() {
        let b = Bounds::new(CARD_DECLARED, &header(415_743), BLOCKS);
        assert_eq!(b.backup(), Some((415_711, 415_743)));
        assert_eq!(b.check(415_711, 1), Err(Refusal::Protected));
        assert_eq!(b.check(415_743, 1), Err(Refusal::Protected));
        assert_eq!(b.check(415_710, 1), Err(Refusal::Outside));
    }

    #[test]
    fn a_damaged_or_missing_header_closes_every_write() {
        let mut bad = header(415_743);
        bad[40] ^= 1;
        for h in [bad, [0u8; 512], header(BLOCKS), header(40)] {
            let b = Bounds::new(CARD_DECLARED, &h, BLOCKS);
            assert_eq!(b.check(CARD_DECLARED.first, 1), Err(Refusal::NoGpt));
        }
        assert_eq!(
            Bounds::closed(CARD_DECLARED, BLOCKS).check(CARD_DECLARED.first, 1),
            Err(Refusal::NoGpt)
        );
    }

    #[test]
    fn every_protected_sector_is_refused_even_inside_a_declared_range_that_covers_it() {
        let everything = Declared {
            first: 0,
            last: BLOCKS - 1,
        };
        let b = Bounds::new(everything, &header(415_743), BLOCKS);
        for lba in [
            0,
            1,
            33,
            2048,
            3072,
            415_711,
            415_743,
            BLOCKS - 33,
            BLOCKS - 1,
        ] {
            assert_eq!(b.check(lba, 1), Err(Refusal::Protected), "{lba}");
        }
        assert_eq!(b.check(2040, 16), Err(Refusal::Protected), "a straddle");
        assert_eq!(b.check(34, 8), Ok(()));
        assert_eq!(b.check(2049, 1023), Ok(()));
    }

    #[test]
    fn the_declared_range_is_exact_and_nothing_wraps() {
        let b = Bounds::new(CARD_DECLARED, &header(415_743), BLOCKS);
        let (first, last) = (CARD_DECLARED.first, CARD_DECLARED.last);
        assert_eq!(b.check(first, last - first + 1), Ok(()));
        assert_eq!(b.check(first - 1, 1), Err(Refusal::Outside));
        assert_eq!(b.check(last + 1, 1), Err(Refusal::Outside));
        assert_eq!(b.check(last, 2), Err(Refusal::Outside));
        assert_eq!(b.check(first, 0), Err(Refusal::Outside));
        assert_eq!(b.check(u64::MAX, 2), Err(Refusal::Outside));
    }
}
