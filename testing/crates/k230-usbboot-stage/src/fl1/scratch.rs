//! The card build's storage for the B2 card writes: the card, reachable only inside one scratch range.
//!
//! The range is the one chosen from the card's own layout (`testing/board-runs/K230-USB.md`, "B2 card
//! writes"): sectors [`SCRATCH_FIRST`] to [`SCRATCH_LAST`], far past the partition table, its backup and
//! the boot copies. A request that names any sector outside it, in part or whole, read or write, is refused
//! whole before the card is touched: the stage answers it as a storage failure (status 2), whatever the
//! request's compared mark says. The card storage under it ([`crate::fl1::storage::Sectors`]) holds the same
//! range as its declared one and the protected sectors besides ([`crate::bounds`]); this limit adds reads, so
//! the scratch build cannot read the card outside the range either.

use crate::bounds::CARD_DECLARED;
use crate::fl1::REGIONS;
use lemnos_flash_stage::link::{IoError, Storage};
use lemnos_flash_wire::frame::SECTOR;
use lemnos_flash_wire::identity::{IDENTITY_LEN, identity_record};

/// The scratch build's id ("K230" "B2", serial 2): neither the check build's
/// ([`crate::fl1::check::CHECK_BUILD_ID`], serial 1) nor the full card build's ([`crate::fl1::BUILD_ID`]), so a host
/// tells the three apart before it sends anything but an identify.
pub const SCRATCH_BUILD_ID: u64 = 0x4b32_3330_4232_0002;

/// The identity record of the scratch build: its own id, the K230's regions.
pub const SCRATCH_IDENTITY: [u8; IDENTITY_LEN] = identity_record(SCRATCH_BUILD_ID, &REGIONS);

/// The card of record's CID, as the SD host reads it (the R2 reply's 128 bits with the CRC byte, which the
/// controller does not keep, as 0): the card the survey read (`testing/board-runs/K230-USB.md`, "B2 card
/// writes"). The scratch build writes no other card.
pub const CARD_OF_RECORD_CID: [u8; 16] = [
    0x03, 0x53, 0x44, 0x53, 0x52, 0x36, 0x34, 0x47, 0x80, 0x14, 0xa7, 0xc9, 0xab, 0x01, 0x51, 0x00,
];

/// The card of record's size in 512-byte blocks.
pub const CARD_OF_RECORD_BLOCKS: u64 = 124_735_488;

/// Whether a card is the card of record: its CID (the CRC byte not compared, as the piece path compares it)
/// and its size.
pub fn card_of_record(cid: &[u8; 16], blocks: u64) -> bool {
    cid[..15] == CARD_OF_RECORD_CID[..15] && blocks == CARD_OF_RECORD_BLOCKS
}

/// The first sector of the scratch range (the 512 MiB mark): the card builds' declared range.
pub const SCRATCH_FIRST: u32 = CARD_DECLARED.first as u32;
/// The last sector of the scratch range, inclusive (2048 sectors, 1 MiB).
pub const SCRATCH_LAST: u32 = CARD_DECLARED.last as u32;

/// Whether `bytes` bytes at `lba` are whole sectors, at least one, all inside the scratch range.
pub fn inside(lba: u32, bytes: usize) -> bool {
    if bytes == 0 || !bytes.is_multiple_of(SECTOR) {
        return false;
    }
    let Ok(sectors) = u32::try_from(bytes / SECTOR) else {
        return false;
    };
    let Some(last) = lba.checked_add(sectors - 1) else {
        return false;
    };
    lba >= SCRATCH_FIRST && last <= SCRATCH_LAST
}

/// A storage reachable only inside the scratch range.
pub struct Scratch<S: Storage> {
    inner: S,
}

impl<S: Storage> Scratch<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }

    /// The storage behind the limit (for a test's look at the card).
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: Storage> Storage for Scratch<S> {
    fn write(&mut self, lba: u32, data: &[u8], budget_us: u64) -> Result<(), IoError> {
        if !inside(lba, data.len()) {
            return Err(IoError::Failed);
        }
        self.inner.write(lba, data, budget_us)
    }

    fn read(&mut self, lba: u32, out: &mut [u8], budget_us: u64) -> Result<(), IoError> {
        if !inside(lba, out.len()) {
            return Err(IoError::Failed);
        }
        self.inner.read(lba, out, budget_us)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fl1::check::CHECK_IDENTITY;
    use crate::fl1::{BUILD_ID, IDENTITY};
    use lemnos_flash_wire::identity;

    #[test]
    fn the_scratch_identity_is_neither_the_check_builds_nor_the_card_builds() {
        assert_ne!(SCRATCH_IDENTITY, CHECK_IDENTITY);
        assert_ne!(SCRATCH_IDENTITY, IDENTITY);
        assert_ne!(SCRATCH_BUILD_ID, BUILD_ID);
        let peer = identity::check(&SCRATCH_IDENTITY).unwrap();
        assert_eq!(peer.build_id, SCRATCH_BUILD_ID);
    }

    #[test]
    fn only_the_card_of_record_is_taken() {
        assert!(card_of_record(&CARD_OF_RECORD_CID, CARD_OF_RECORD_BLOCKS));
        let mut crc = CARD_OF_RECORD_CID;
        crc[15] = 0x5b;
        assert!(
            card_of_record(&crc, CARD_OF_RECORD_BLOCKS),
            "the CRC byte is not kept"
        );
        for byte in 0..15 {
            let mut other = CARD_OF_RECORD_CID;
            other[byte] ^= 1;
            assert!(
                !card_of_record(&other, CARD_OF_RECORD_BLOCKS),
                "byte {byte}"
            );
        }
        assert!(!card_of_record(
            &CARD_OF_RECORD_CID,
            CARD_OF_RECORD_BLOCKS - 1024
        ));
    }

    #[test]
    fn the_range_is_past_every_region_and_excludes_the_header_blocks() {
        let end = REGIONS.iter().map(|r| r.end).max().unwrap();
        assert!(SCRATCH_FIRST > end);
        for lba in [0, 1, 33, 2048, 3072, 415_711, 415_743] {
            assert!(!inside(lba, SECTOR), "{lba}");
        }
    }

    #[test]
    fn each_edge_is_refused_and_the_range_itself_is_not() {
        assert!(inside(SCRATCH_FIRST, SECTOR));
        assert!(inside(SCRATCH_LAST, SECTOR));
        assert!(inside(
            SCRATCH_FIRST,
            (SCRATCH_LAST - SCRATCH_FIRST + 1) as usize * SECTOR
        ));
        assert!(!inside(SCRATCH_FIRST - 1, SECTOR));
        assert!(!inside(SCRATCH_LAST + 1, SECTOR));
        assert!(
            !inside(SCRATCH_FIRST - 1, 2 * SECTOR),
            "straddles the first sector"
        );
        assert!(
            !inside(SCRATCH_LAST, 2 * SECTOR),
            "straddles the last sector"
        );
        assert!(!inside(SCRATCH_FIRST, 0));
        assert!(!inside(SCRATCH_FIRST, SECTOR + 1));
        assert!(!inside(u32::MAX, SECTOR));
        assert!(
            !inside(u32::MAX, 2 * SECTOR),
            "no wrap past the last sector number"
        );
    }
}
