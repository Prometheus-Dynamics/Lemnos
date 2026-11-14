//! The MBR's arithmetic for growing the data partition at first start (M36).
//!
//! Pure: a sector buffer in, a sector buffer out. No I/O, no ioctl. The caller reads sector 0, asks
//! [`plan_grow`] what to do, writes the result with [`set_sectors`], and tells the kernel itself.
//!
//! Layout of sector 0 (the classic MBR): the 446 bytes of boot code, then four 16-byte entries at
//! 446, 462, 478 and 494, then the signature `0x55 0xAA` at 510. An entry is: status (1), CHS start
//! (3), type (1), CHS end (3), start LBA (u32 LE), sector count (u32 LE).
//!
//! The only byte [`set_sectors`] changes is the sector count of one entry (bytes 474..478 for entry
//! 1, the data partition). Nothing else in the sector is touched; [`changed_bytes`] proves that in
//! the tests.

/// One sector.
pub const SECTOR: usize = 512;
/// Offset of the first partition entry.
pub const TABLE_AT: usize = 446;
/// The size of one entry.
pub const ENTRY_BYTES: usize = 16;
/// The number of primary entries.
pub const ENTRIES: usize = 4;
/// The Linux data partition type this layout uses.
pub const TYPE_LINUX: u8 = 0x83;
/// The grow target is aligned to this many sectors (1 MiB).
pub const ALIGN_SECTORS: u64 = 2048;
/// The smallest gain worth a write: below this nothing is changed.
pub const MIN_GAIN_SECTORS: u64 = 2048;

/// One partition entry, as the table holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub status: u8,
    pub chs_start: [u8; 3],
    pub kind: u8,
    pub chs_end: [u8; 3],
    pub start: u32,
    pub sectors: u32,
}

impl Entry {
    /// Whether the entry describes nothing (type 0, or no sectors).
    pub fn is_empty(&self) -> bool {
        self.kind == 0 || self.sectors == 0
    }

    /// The first sector after the partition.
    pub fn end(&self) -> u64 {
        u64::from(self.start) + u64::from(self.sectors)
    }
}

/// What the grow step does, or why it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// The partition grows to `sectors`.
    Grow { sectors: u32 },
    /// Nothing to do: the partition already reaches the aligned end of the disk, or the gain is
    /// under [`MIN_GAIN_SECTORS`].
    Done,
}

/// A refusal, with the reason in words for the board's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The sector has no `0x55 0xAA` signature.
    NoSignature,
    /// The entry asked for is not the one the layout names (type or position).
    NotDataPartition(String),
    /// Another partition follows the data partition, so it cannot grow to the disk's end.
    NotLast,
    /// The partition ends past the disk: the disk is smaller than its table.
    DiskSmaller { end: u64, disk: u64 },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NoSignature => write!(f, "the MBR has no 0x55 0xAA signature: nothing changed"),
            Refusal::NotDataPartition(why) => write!(f, "{why}: nothing changed"),
            Refusal::NotLast => write!(f, "data partition is not the last one: nothing grown"),
            Refusal::DiskSmaller { end, disk } => write!(
                f,
                "the disk is smaller than its partition table: the data partition ends at sector {end}, the disk has {disk}: nothing changed"
            ),
        }
    }
}

/// The four entries of sector 0.
///
/// # Errors
///
/// [`Refusal::NoSignature`] when the sector does not end in `0x55 0xAA`.
pub fn entries(sector: &[u8; SECTOR]) -> Result<[Entry; ENTRIES], Refusal> {
    if sector[510] != 0x55 || sector[511] != 0xAA {
        return Err(Refusal::NoSignature);
    }
    Ok(std::array::from_fn(|i| {
        let at = TABLE_AT + i * ENTRY_BYTES;
        let e = &sector[at..at + ENTRY_BYTES];
        Entry {
            status: e[0],
            chs_start: [e[1], e[2], e[3]],
            kind: e[4],
            chs_end: [e[5], e[6], e[7]],
            start: u32::from_le_bytes([e[8], e[9], e[10], e[11]]),
            sectors: u32::from_le_bytes([e[12], e[13], e[14], e[15]]),
        }
    }))
}

/// The aligned length the data partition would have: from `start` to the last whole 1 MiB before
/// `disk_sectors`. `None` when the disk does not reach past `start`.
pub fn grow_target(start: u64, disk_sectors: u64) -> Option<u64> {
    let room = disk_sectors.checked_sub(start)?;
    Some(room / ALIGN_SECTORS * ALIGN_SECTORS)
}

/// What to do for entry `index` (0-based) so it fills the disk, given `disk_sectors`.
///
/// The entry must be the Linux data type, and no entry after it may be in use (it must be the last
/// partition). The disk must reach past its end. A grow that gains less than [`MIN_GAIN_SECTORS`]
/// is [`Plan::Done`], so a second run after a grow changes nothing.
///
/// # Errors
///
/// The [`Refusal`] that stops the grow; nothing is to be written.
pub fn plan_grow(sector: &[u8; SECTOR], index: usize, disk_sectors: u64) -> Result<Plan, Refusal> {
    let table = entries(sector)?;
    let part = table[index];
    if part.kind != TYPE_LINUX {
        return Err(Refusal::NotDataPartition(format!(
            "entry {} has type {:#04x}, not the data partition's {TYPE_LINUX:#04x}",
            index + 1,
            part.kind
        )));
    }
    if table[index + 1..].iter().any(|later| !later.is_empty()) {
        return Err(Refusal::NotLast);
    }
    let end = part.end();
    if end > disk_sectors {
        return Err(Refusal::DiskSmaller { end, disk: disk_sectors });
    }
    let Some(target) = grow_target(u64::from(part.start), disk_sectors) else {
        return Err(Refusal::DiskSmaller { end, disk: disk_sectors });
    };
    if target < u64::from(part.sectors) + MIN_GAIN_SECTORS {
        return Ok(Plan::Done);
    }
    let sectors = u32::try_from(target).map_err(|_| {
        Refusal::NotDataPartition("the grown partition would exceed 2 TiB in 32-bit sectors".into())
    })?;
    Ok(Plan::Grow { sectors })
}

/// Checks a new size against the old one before it is written, so a torn or stale value is caught
/// on the next start: the new size must be at least the old one and must end at or before the disk.
pub fn sane_new_size(old: u32, new: u32, start: u32, disk_sectors: u64) -> bool {
    new >= old && u64::from(start) + u64::from(new) <= disk_sectors
}

/// Sets the sector count of entry `index` (0-based) in `sector`, and nothing else.
pub fn set_sectors(sector: &mut [u8; SECTOR], index: usize, sectors: u32) {
    let at = TABLE_AT + index * ENTRY_BYTES + 12;
    sector[at..at + 4].copy_from_slice(&sectors.to_le_bytes());
}

/// The byte offsets that differ between two sectors, in order.
pub fn changed_bytes(before: &[u8; SECTOR], after: &[u8; SECTOR]) -> Vec<usize> {
    (0..SECTOR).filter(|&i| before[i] != after[i]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sector with entry 1 a FAT boot partition and entry 2 a Linux data partition.
    fn table(p2_start: u32, p2_sectors: u32) -> [u8; SECTOR] {
        let mut s = [0u8; SECTOR];
        s[510] = 0x55;
        s[511] = 0xAA;
        // Entry 1: type 0x0c (FAT32 LBA), start 8192, 161792 sectors (82,837,504 bytes).
        put(&mut s, 0, 0x0c, 8192, 161_792);
        put(&mut s, 1, TYPE_LINUX, p2_start, p2_sectors);
        s
    }

    fn put(s: &mut [u8; SECTOR], index: usize, kind: u8, start: u32, sectors: u32) {
        let at = TABLE_AT + index * ENTRY_BYTES;
        s[at] = 0x00;
        s[at + 1..at + 4].copy_from_slice(&[0xfe, 0xff, 0xff]);
        s[at + 4] = kind;
        s[at + 5..at + 8].copy_from_slice(&[0xfe, 0xff, 0xff]);
        s[at + 8..at + 12].copy_from_slice(&start.to_le_bytes());
        s[at + 12..at + 16].copy_from_slice(&sectors.to_le_bytes());
    }

    const P2_START: u32 = 8192 + 161_792;

    #[test]
    fn the_boot_partition_is_82837504_bytes_as_atlas_lays_it_out() {
        assert_eq!(161_792u64 * 512, 82_837_504);
        let s = table(P2_START, 100_000);
        let t = entries(&s).unwrap();
        assert_eq!(t[0].sectors as u64 * 512, 82_837_504);
        assert_eq!(t[1].kind, TYPE_LINUX);
        assert!(t[2].is_empty() && t[3].is_empty());
    }

    #[test]
    fn a_sector_without_the_signature_is_refused() {
        let mut s = table(P2_START, 100_000);
        s[511] = 0;
        assert_eq!(entries(&s), Err(Refusal::NoSignature));
        assert_eq!(plan_grow(&s, 1, 10_000_000), Err(Refusal::NoSignature));
    }

    #[test]
    fn a_last_data_partition_grows_to_the_aligned_end_of_the_disk() {
        let disk = 32_000_000u64; // 16 GB
        let s = table(P2_START, 100_000);
        let target = grow_target(P2_START as u64, disk).unwrap();
        assert_eq!(target % ALIGN_SECTORS, 0);
        assert_eq!(
            plan_grow(&s, 1, disk),
            Ok(Plan::Grow { sectors: target as u32 })
        );
    }

    #[test]
    fn after_the_grow_a_second_plan_does_nothing() {
        let disk = 32_000_000u64;
        let mut s = table(P2_START, 100_000);
        let Plan::Grow { sectors } = plan_grow(&s, 1, disk).unwrap() else {
            panic!("expected a grow");
        };
        set_sectors(&mut s, 1, sectors);
        assert_eq!(plan_grow(&s, 1, disk), Ok(Plan::Done));
    }

    #[test]
    fn a_gain_under_one_mib_writes_nothing() {
        let disk = P2_START as u64 + 100_000 + 2047;
        let s = table(P2_START, 100_000);
        assert_eq!(plan_grow(&s, 1, disk), Ok(Plan::Done));
    }

    #[test]
    fn a_partition_that_is_not_last_is_refused() {
        let mut s = table(P2_START, 100_000);
        put(&mut s, 2, 0x83, P2_START + 100_000, 2048);
        assert_eq!(plan_grow(&s, 1, 32_000_000), Err(Refusal::NotLast));
    }

    #[test]
    fn the_wrong_type_is_refused_by_name() {
        let mut s = table(P2_START, 100_000);
        put(&mut s, 1, 0x0c, P2_START, 100_000);
        let Err(Refusal::NotDataPartition(why)) = plan_grow(&s, 1, 32_000_000) else {
            panic!("expected a type refusal");
        };
        assert!(why.contains("0x0c"), "{why}");
    }

    #[test]
    fn a_disk_smaller_than_its_table_is_refused_and_not_changed() {
        let s = table(P2_START, 100_000);
        let disk = P2_START as u64 + 50_000;
        assert_eq!(
            plan_grow(&s, 1, disk),
            Err(Refusal::DiskSmaller {
                end: P2_START as u64 + 100_000,
                disk
            })
        );
    }

    #[test]
    fn only_the_sector_count_of_the_data_entry_changes() {
        let disk = 32_000_000u64;
        let before = table(P2_START, 100_000);
        let mut after = before;
        let Plan::Grow { sectors } = plan_grow(&before, 1, disk).unwrap() else {
            panic!("expected a grow");
        };
        set_sectors(&mut after, 1, sectors);
        // Entry 2's sector count is bytes 474..478; nothing else moves.
        assert_eq!(changed_bytes(&before, &after), vec![474, 475, 476, 477]);
    }

    #[test]
    fn the_boot_entry_and_the_disk_id_are_never_touched() {
        let before = table(P2_START, 100_000);
        let mut after = before;
        set_sectors(&mut after, 1, 1_000_000);
        let changed = changed_bytes(&before, &after);
        assert!(changed.iter().all(|&i| (474..478).contains(&i)));
        assert!(changed.iter().all(|&i| i < 440 || i > 445));
    }

    #[test]
    fn a_stale_or_torn_size_is_caught_by_sane_new_size() {
        assert!(sane_new_size(100_000, 500_000, P2_START, 32_000_000));
        // Smaller than the old size: a torn write.
        assert!(!sane_new_size(100_000, 10, P2_START, 32_000_000));
        // Past the disk: a torn write of the high bytes.
        assert!(!sane_new_size(100_000, 0xFFFF_FFF0, P2_START, 32_000_000));
    }

    #[test]
    fn the_refusals_say_what_is_left_alone() {
        assert!(Refusal::NotLast.to_string().contains("nothing grown"));
        assert!(Refusal::DiskSmaller { end: 2, disk: 1 }.to_string().contains("nothing changed"));
    }
}
