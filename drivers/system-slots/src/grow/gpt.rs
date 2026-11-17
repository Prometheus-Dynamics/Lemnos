//! The GPT side of the data-partition grow: the two headers and the entry array, read and written
//! with their CRCs. The MBR side is in `grow.rs`; the pass that uses both is [`super::grow`].
//!
//! A GPT disk keeps a primary header at LBA 1 and its entries at LBA 2 to 33, and a backup at the
//! disk's last sector with its entries just before it. The usable area ends 34 sectors before the
//! disk's end. A grow moves the backup to the new end, then the primary, so that at every write the
//! disk reads as the old table or the new one (see [`super::grow`]'s write order).

use lemnos_slot_store::mbr::ALIGN_SECTORS;
use lemnos_slot_store::twocopy::crc32;

use super::Disk;

/// The sector size of every table here.
pub const SECTOR: u64 = 512;
/// The entry array: 128 entries of 128 bytes, 32 sectors.
pub const ENTRY_COUNT: usize = 128;
pub const ENTRY_BYTES: usize = 128;
pub const ARRAY_SECTORS: u64 = 32;
pub const ARRAY_BYTES: usize = ENTRY_COUNT * ENTRY_BYTES;
/// The header's own size.
const HEADER_BYTES: usize = 92;
pub const SIGNATURE: &[u8; 8] = b"EFI PART";
const REVISION: u32 = 0x0001_0000;
/// The primary header's LBA.
pub const PRIMARY_LBA: u64 = 1;
/// The primary entry array's first LBA.
pub const PRIMARY_ARRAY_LBA: u64 = 2;
/// Sectors at the disk's end that the backup takes: the header and the array.
pub const BACKUP_TAIL: u64 = 1 + ARRAY_SECTORS;

/// A GPT header, the fields a grow reads and writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub my_lba: u64,
    pub alt_lba: u64,
    pub first_usable: u64,
    pub last_usable: u64,
    pub disk_guid: [u8; 16],
    pub array_lba: u64,
    pub array_crc: u32,
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le64(b: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(word)
}

/// The header as its 512-byte sector: the fixed fields, the CRC over the first 92 bytes.
pub fn encode_header(h: &Header) -> [u8; SECTOR as usize] {
    let mut s = [0u8; SECTOR as usize];
    s[0..8].copy_from_slice(SIGNATURE);
    s[8..12].copy_from_slice(&REVISION.to_le_bytes());
    s[12..16].copy_from_slice(&(HEADER_BYTES as u32).to_le_bytes());
    s[24..32].copy_from_slice(&h.my_lba.to_le_bytes());
    s[32..40].copy_from_slice(&h.alt_lba.to_le_bytes());
    s[40..48].copy_from_slice(&h.first_usable.to_le_bytes());
    s[48..56].copy_from_slice(&h.last_usable.to_le_bytes());
    s[56..72].copy_from_slice(&h.disk_guid);
    s[72..80].copy_from_slice(&h.array_lba.to_le_bytes());
    s[80..84].copy_from_slice(&(ENTRY_COUNT as u32).to_le_bytes());
    s[84..88].copy_from_slice(&(ENTRY_BYTES as u32).to_le_bytes());
    s[88..92].copy_from_slice(&h.array_crc.to_le_bytes());
    let crc = crc32(&s[..HEADER_BYTES]);
    s[16..20].copy_from_slice(&crc.to_le_bytes());
    s
}

/// What a pass reads of the GPT: the valid header, its entry array, and whether it came from the
/// backup (the primary is damaged).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub header: Header,
    pub array: Vec<u8>,
    pub from_backup: bool,
}

/// Why a header or an array is not the table a grow can use: named, for the unavailable state.
pub type Unusable = String;

/// Decodes and checks a header sector: its signature, its size, its CRC, its own LBA (`expect_my`),
/// and the array shape this grow takes (128 entries of 128 bytes).
pub fn parse_header(s: &[u8], expect_my: u64) -> Result<Header, Unusable> {
    if s.len() < SECTOR as usize || &s[0..8] != SIGNATURE {
        return Err("it has no GPT signature".into());
    }
    let size = le32(s, 12) as usize;
    if size != HEADER_BYTES {
        return Err(format!("its header size {size} is not {HEADER_BYTES}"));
    }
    let mut summed = s[..HEADER_BYTES].to_vec();
    summed[16..20].fill(0);
    if crc32(&summed) != le32(s, 16) {
        return Err("its header CRC is wrong".into());
    }
    let my_lba = le64(s, 24);
    if my_lba != expect_my {
        return Err(format!("it claims LBA {my_lba}, not {expect_my}"));
    }
    if le32(s, 80) as usize != ENTRY_COUNT || le32(s, 84) as usize != ENTRY_BYTES {
        return Err("its entry array is not 128 entries of 128 bytes".into());
    }
    let mut disk_guid = [0u8; 16];
    disk_guid.copy_from_slice(&s[56..72]);
    Ok(Header {
        my_lba,
        alt_lba: le64(s, 32),
        first_usable: le64(s, 40),
        last_usable: le64(s, 48),
        disk_guid,
        array_lba: le64(s, 72),
        array_crc: le32(s, 88),
    })
}

/// Checks an entry array against its header's CRC.
pub fn check_array(array: &[u8], header: &Header) -> Result<(), Unusable> {
    if array.len() != ARRAY_BYTES {
        return Err("its entry array is short".into());
    }
    if crc32(array) != header.array_crc {
        return Err("its entry array CRC is wrong".into());
    }
    Ok(())
}

/// One entry of the array: its first and last LBA and its type GUID (all zero when unused).
pub fn entry(array: &[u8], index: usize) -> (u64, u64, [u8; 16]) {
    let at = index * ENTRY_BYTES;
    let mut guid = [0u8; 16];
    guid.copy_from_slice(&array[at..at + 16]);
    (le64(array, at + 32), le64(array, at + 40), guid)
}

/// The array with entry `index` set to `first..=last`, the type GUID and name kept as they were.
pub fn with_last(array: &[u8], index: usize, last: u64) -> Vec<u8> {
    let mut out = array.to_vec();
    let at = index * ENTRY_BYTES + 40;
    out[at..at + 8].copy_from_slice(&last.to_le_bytes());
    out
}

/// The array's CRC as a header records it.
pub fn array_crc(array: &[u8]) -> u32 {
    crc32(array)
}

/// The last usable LBA of a disk of `disk_sectors`: the backup takes the disk's last 33 sectors.
pub fn last_usable(disk_sectors: u64) -> u64 {
    disk_sectors.saturating_sub(BACKUP_TAIL + 1)
}

/// The grown partition's last LBA: the last whole aligned unit (1 MiB) of the usable area.
pub fn target_end(disk_sectors: u64) -> u64 {
    ((last_usable(disk_sectors) + 1) / ALIGN_SECTORS) * ALIGN_SECTORS - 1
}

/// Why partition `index` (the data partition at `start`) is not the last used entry, when another
/// used entry starts after it.
pub fn not_last(array: &[u8], index: usize, start: u64) -> Option<String> {
    (0..ENTRY_COUNT)
        .filter(|&i| i != index)
        .find(|&i| {
            let (first, _, guid) = entry(array, i);
            guid != [0u8; 16] && first > start
        })
        .map(|i| {
            format!(
                "partition {} starts after partition 2, which must be the last one",
                i + 1
            )
        })
}

/// Reads the table: the primary when its header and array are good, else the backup when that is.
/// `Err` names why neither is usable.
pub fn load(disk: &dyn Disk, disk_sectors: u64) -> Result<Loaded, String> {
    let primary = read_copy(disk, PRIMARY_LBA, PRIMARY_LBA, disk_sectors);
    match primary {
        Ok((header, array)) => Ok(Loaded {
            header,
            array,
            from_backup: false,
        }),
        Err(why_primary) => {
            let backup_at = disk_sectors.saturating_sub(1);
            match read_copy(disk, backup_at, backup_at, disk_sectors) {
                Ok((header, array)) => Ok(Loaded {
                    header,
                    array,
                    from_backup: true,
                }),
                Err(why_backup) => Err(format!(
                    "the primary GPT is unusable ({why_primary}) and the backup is too ({why_backup})"
                )),
            }
        }
    }
}

/// One copy of the table, read at its header's LBA `at`, checked against `expect_my`.
fn read_copy(
    disk: &dyn Disk,
    at: u64,
    expect_my: u64,
    disk_sectors: u64,
) -> Result<(Header, Vec<u8>), String> {
    let mut sector = [0u8; SECTOR as usize];
    disk.read_at(at * SECTOR, &mut sector)
        .map_err(|e| format!("its header can't be read: {e}"))?;
    let header = parse_header(&sector, expect_my)?;
    if header.array_lba + ARRAY_SECTORS > disk_sectors {
        return Err("its entry array lies outside the disk".into());
    }
    let mut array = vec![0u8; ARRAY_BYTES];
    disk.read_at(header.array_lba * SECTOR, &mut array)
        .map_err(|e| format!("its entry array can't be read: {e}"))?;
    check_array(&array, &header)?;
    Ok((header, array))
}

/// One table write: `bytes` at `lba`, only inside `lo..hi`, then synced so that the next write
/// cannot land before it. Refused without writing when the range is outside its bounds.
fn put(disk: &mut dyn Disk, lba: u64, bytes: &[u8], lo: u64, hi: u64) -> Result<(), String> {
    let count = bytes.len() as u64 / SECTOR;
    if lba < lo || lba.saturating_add(count) > hi {
        return Err(format!(
            "a GPT write at LBA {lba} for {count} sectors is outside its bounds {lo}..{hi}: nothing more was written"
        ));
    }
    disk.write_at(lba * SECTOR, bytes)
        .map_err(|e| format!("the GPT write at LBA {lba} failed: {e}"))?;
    disk.sync()
        .map_err(|e| format!("the GPT write at LBA {lba} can't be flushed: {e}"))
}

fn array_bytes(array: &[u8]) -> Vec<u8> {
    array.to_vec()
}

/// The grow's table writes, in the order that keeps the table readable at every cut:
///
/// 1. zero the stale backup (its array and header at the primary's alternate LBA): the primary still
///    reads the old table;
/// 2. the new backup's array at the new end;
/// 3. the new backup's header (the backup is now the new table);
/// 4. the primary's array: its CRC no longer matches the old header, so the backup is read;
/// 5. the primary's header: the commit. Read back and checked.
///
/// A cut after any write leaves the old table or the new one readable, never neither.
pub fn grow_table(
    disk: &mut dyn Disk,
    loaded: &Loaded,
    target_end: u64,
    disk_sectors: u64,
) -> Result<(), String> {
    let h = &loaded.header;
    let old_end = entry(&loaded.array, super::ENTRY).1;
    let new_array = with_last(&loaded.array, super::ENTRY, target_end);
    let new_crc = array_crc(&new_array);
    let last = last_usable(disk_sectors);
    // 1. The stale backup: its 32 array sectors and its header, past the old partition's end.
    let stale = vec![0u8; ((ARRAY_SECTORS + 1) * SECTOR) as usize];
    put(
        disk,
        h.alt_lba - ARRAY_SECTORS,
        &stale,
        old_end + 1,
        disk_sectors,
    )?;
    // 2. The new backup's array, then 3. its header.
    let backup_array = disk_sectors - 1 - ARRAY_SECTORS;
    put(
        disk,
        backup_array,
        &array_bytes(&new_array),
        target_end + 1,
        disk_sectors,
    )?;
    let backup = Header {
        my_lba: disk_sectors - 1,
        alt_lba: PRIMARY_LBA,
        first_usable: h.first_usable,
        last_usable: last,
        disk_guid: h.disk_guid,
        array_lba: backup_array,
        array_crc: new_crc,
    };
    put(
        disk,
        disk_sectors - 1,
        &encode_header(&backup),
        target_end + 1,
        disk_sectors,
    )?;
    // 4. The primary's array, then 5. its header.
    put(
        disk,
        PRIMARY_ARRAY_LBA,
        &new_array,
        PRIMARY_ARRAY_LBA,
        PRIMARY_LBA + BACKUP_TAIL + 1,
    )?;
    let primary = Header {
        my_lba: PRIMARY_LBA,
        alt_lba: disk_sectors - 1,
        first_usable: h.first_usable,
        last_usable: last,
        disk_guid: h.disk_guid,
        array_lba: PRIMARY_ARRAY_LBA,
        array_crc: new_crc,
    };
    put(
        disk,
        PRIMARY_LBA,
        &encode_header(&primary),
        PRIMARY_LBA,
        PRIMARY_ARRAY_LBA,
    )?;
    // Read back: the primary must now parse, with the new end.
    let (after, _) = read_copy(disk, PRIMARY_LBA, PRIMARY_LBA, disk_sectors).map_err(|why| {
        format!("the primary GPT after the grow is not the one written ({why}): retried")
    })?;
    if after.last_usable != last || entry(&new_array, super::ENTRY).1 != target_end {
        return Err("the primary GPT after the grow does not hold the new end: retried".into());
    }
    Ok(())
}

/// The primary is damaged and the backup holds the table the grow needs: its array and header are
/// rewritten from the backup (steps 4 and 5 of [`grow_table`]), so the primary reads again.
pub fn repair_primary(
    disk: &mut dyn Disk,
    loaded: &Loaded,
    disk_sectors: u64,
) -> Result<(), String> {
    let h = &loaded.header;
    let primary = Header {
        my_lba: PRIMARY_LBA,
        alt_lba: disk_sectors - 1,
        first_usable: h.first_usable,
        last_usable: last_usable(disk_sectors),
        disk_guid: h.disk_guid,
        array_lba: PRIMARY_ARRAY_LBA,
        array_crc: array_crc(&loaded.array),
    };
    put(
        disk,
        PRIMARY_ARRAY_LBA,
        &loaded.array,
        PRIMARY_ARRAY_LBA,
        PRIMARY_LBA + BACKUP_TAIL + 1,
    )?;
    put(
        disk,
        PRIMARY_LBA,
        &encode_header(&primary),
        PRIMARY_LBA,
        PRIMARY_ARRAY_LBA,
    )
}
