//! The GPT, read raw: the one partition of a disk whose type GUID is the U-Boot environment's.
//!
//! U-Boot (tag `v2026.10`, built with `CONFIG_PARTITION_TYPE_GUID` and `CONFIG_OF_CONTROL`) looks the
//! environment up by that type GUID on the MMC device it saves to (`env/mmc.c:187-193`,
//! `mmc_env_partition_by_guid` at `env/mmc.c:72-110`; the GUID is `PARTITION_U_BOOT_ENVIRONMENT`,
//! `include/part_efi.h:61`: `3de21764-95bd-54bd-a5c3-4abe786f38a8`). Lemnos finds the same partition
//! the same way, from the GPT: the primary header at LBA 1 and the entry array it points to, both
//! CRC-checked as the UEFI specification says (the header's CRC is over its `header size` bytes with
//! the CRC field zero; the entry array's CRC is in the header). Nothing is mounted and nothing is
//! written here.
//!
//! Only the primary table is read. U-Boot falls back to the backup table when the primary is
//! damaged; a disk whose primary table is damaged is refused here by name rather than read from the
//! backup, so a write never goes where the reader is not sure U-Boot looks.

use crate::twocopy::crc32;
use crate::uboot_env::{EnvDev, SECTOR};

/// The U-Boot environment partition's type GUID, as text.
pub const ENV_TYPE_GUID: &str = "3de21764-95bd-54bd-a5c3-4abe786f38a8";

/// The same GUID as the GPT stores it: the first three fields little-endian, the rest as written.
pub const ENV_TYPE_BYTES: [u8; 16] = [
    0x64, 0x17, 0xe2, 0x3d, 0xbd, 0x95, 0xbd, 0x54, 0xa5, 0xc3, 0x4a, 0xbe, 0x78, 0x6f, 0x38, 0xa8,
];

/// The most bytes of entry array this reader takes (128 entries of 128 bytes is the usual 16 KiB).
const MAX_ENTRIES_BYTES: u64 = 1 << 20;

/// A partition: its first and last block (inclusive), in 512-byte blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    pub first: u64,
    pub last: u64,
}

impl Part {
    /// The partition's length in blocks.
    pub fn blocks(&self) -> u64 {
        self.last - self.first + 1
    }
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le64(b: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(word)
}

/// The one partition with the environment's type GUID. Refused by name when the disk has no valid
/// GPT, no such partition, or several.
pub fn find_env_partition<D: EnvDev + ?Sized>(dev: &mut D) -> Result<Part, String> {
    let mut header = vec![0u8; SECTOR];
    dev.read_at(SECTOR as u64, &mut header)
        .map_err(|e| format!("the GPT header can't be read: {e}"))?;
    if &header[..8] != b"EFI PART" {
        return Err("the boot disk has no GPT (LBA 1 does not start with EFI PART)".into());
    }
    let size = le32(&header, 12) as usize;
    if !(92..=SECTOR).contains(&size) {
        return Err(format!("the GPT header's size {size} is not 92 to 512"));
    }
    let stored = le32(&header, 16);
    let mut summed = header[..size].to_vec();
    summed[16..20].fill(0);
    if crc32(&summed) != stored {
        return Err("the GPT header's CRC is wrong".into());
    }
    let table_lba = le64(&header, 72);
    let count = u64::from(le32(&header, 80));
    let entry = u64::from(le32(&header, 84));
    if entry < 128 || entry % 128 != 0 || count == 0 || count * entry > MAX_ENTRIES_BYTES {
        return Err(format!(
            "the GPT's {count} entries of {entry} bytes are not a table this reader takes"
        ));
    }
    let mut table = vec![0u8; (count * entry) as usize];
    dev.read_at(table_lba.saturating_mul(SECTOR as u64), &mut table)
        .map_err(|e| format!("the GPT entries can't be read: {e}"))?;
    if crc32(&table) != le32(&header, 88) {
        return Err("the GPT entry array's CRC is wrong".into());
    }
    let mut found = Vec::new();
    for raw in table.chunks_exact(entry as usize) {
        if raw[..16] == ENV_TYPE_BYTES {
            let part = Part {
                first: le64(raw, 32),
                last: le64(raw, 40),
            };
            if part.last < part.first {
                return Err("a GPT entry ends before it starts".into());
            }
            found.push(part);
        }
    }
    match found.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!(
            "no partition on the boot disk has the U-Boot environment type GUID {ENV_TYPE_GUID}: the image needs one"
        )),
        many => Err(format!(
            "{} partitions on the boot disk have the U-Boot environment type GUID {ENV_TYPE_GUID}: refusing to guess",
            many.len()
        )),
    }
}

/// A GPT image (header, entry array, no partition contents) of `total_blocks` blocks, for tests of
/// this crate and of the crates that read a disk through it: `parts` are `(type bytes, first, last)`.
#[doc(hidden)]
pub fn image(parts: &[([u8; 16], u64, u64)], total_blocks: u64) -> Vec<u8> {
    let mut disk = vec![0u8; (total_blocks as usize) * SECTOR];
    let mut table = vec![0u8; 128 * 128];
    for (i, (kind, first, last)) in parts.iter().enumerate() {
        let at = i * 128;
        table[at..at + 16].copy_from_slice(kind);
        table[at + 16] = 1;
        table[at + 32..at + 40].copy_from_slice(&first.to_le_bytes());
        table[at + 40..at + 48].copy_from_slice(&last.to_le_bytes());
    }
    let mut header = vec![0u8; 92];
    header[..8].copy_from_slice(b"EFI PART");
    header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    header[12..16].copy_from_slice(&92u32.to_le_bytes());
    header[24..32].copy_from_slice(&1u64.to_le_bytes());
    header[72..80].copy_from_slice(&2u64.to_le_bytes());
    header[80..84].copy_from_slice(&128u32.to_le_bytes());
    header[84..88].copy_from_slice(&128u32.to_le_bytes());
    header[88..92].copy_from_slice(&crc32(&table).to_le_bytes());
    let crc = crc32(&header);
    header[16..20].copy_from_slice(&crc.to_le_bytes());
    disk[SECTOR..SECTOR + 92].copy_from_slice(&header);
    disk[2 * SECTOR..2 * SECTOR + table.len()].copy_from_slice(&table);
    disk
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uboot_env::FileEnv;

    const OTHER: [u8; 16] = [0xaf; 16];

    fn on(disk: &[u8], name: &str) -> FileEnv {
        let path = std::env::temp_dir().join(format!("lemnos-gpt-{}-{name}", std::process::id()));
        std::fs::write(&path, disk).unwrap();
        let env = FileEnv::open(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        env
    }

    #[test]
    fn the_guid_bytes_are_the_text_in_gpt_order() {
        let text: String = ENV_TYPE_GUID.replace('-', "");
        let b: Vec<u8> = (0..16)
            .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        let want = [
            b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13],
            b[14], b[15],
        ];
        assert_eq!(ENV_TYPE_BYTES, want);
    }

    #[test]
    fn the_one_partition_of_that_type_is_found_among_others() {
        let disk = image(
            &[
                (OTHER, 64, 127),
                (ENV_TYPE_BYTES, 128, 255),
                (OTHER, 256, 300),
            ],
            512,
        );
        let part = find_env_partition(&mut on(&disk, "one")).unwrap();
        assert_eq!(
            part,
            Part {
                first: 128,
                last: 255
            }
        );
        assert_eq!(part.blocks(), 128);
    }

    #[test]
    fn none_or_several_are_refused_by_name() {
        let none = image(&[(OTHER, 64, 127)], 512);
        let why = find_env_partition(&mut on(&none, "none")).unwrap_err();
        assert!(
            why.contains("no partition on the boot disk has the U-Boot environment type GUID"),
            "{why}"
        );
        let two = image(
            &[(ENV_TYPE_BYTES, 64, 127), (ENV_TYPE_BYTES, 128, 255)],
            512,
        );
        let why = find_env_partition(&mut on(&two, "two")).unwrap_err();
        assert!(
            why.starts_with("2 partitions on the boot disk have"),
            "{why}"
        );
    }

    #[test]
    fn a_disk_without_a_valid_table_is_refused_by_name() {
        let mut disk = image(&[(ENV_TYPE_BYTES, 64, 127)], 512);
        let mut bad_header = disk.clone();
        bad_header[SECTOR + 40] ^= 1;
        assert!(
            find_env_partition(&mut on(&bad_header, "h"))
                .unwrap_err()
                .contains("header's CRC")
        );
        let mut bad_entry = disk.clone();
        bad_entry[2 * SECTOR + 33] ^= 1;
        assert!(
            find_env_partition(&mut on(&bad_entry, "e"))
                .unwrap_err()
                .contains("entry array's CRC")
        );
        disk[SECTOR] = b'X';
        assert!(
            find_env_partition(&mut on(&disk, "s"))
                .unwrap_err()
                .contains("no GPT")
        );
    }
}
