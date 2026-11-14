//! A small FAT16 volume with pre-created files, for tests (here and in the crates that read the
//! boot partition). Each file is one contiguous run of clusters, one sector per cluster, in the
//! root directory, as the image builder places the records and pointers ([`crate::bootfat`]
//! refuses anything else).

/// Sectors per FAT: 16384 clusters of two bytes.
const FAT_SECTORS: usize = 64;
/// Root directory sectors: 512 entries of 32 bytes.
const ROOT_SECTORS: usize = 32;
/// The volume's sector count.
const SECTORS: usize = 16_384;
/// The first data sector.
const DATA_START: usize = 1 + FAT_SECTORS + ROOT_SECTORS;

/// Builds the volume: a boot sector, one FAT, a root directory with one entry per file, and each
/// file's bytes in its own contiguous clusters, from cluster 2 on. A file of `n` bytes takes
/// `ceil(n / 512)` clusters, at least one.
pub fn fat16(files: &[(&[u8; 11], Vec<u8>)]) -> Vec<u8> {
    let mut image = vec![0u8; SECTORS * 512];
    // Boot sector (FAT16 BIOS parameter block).
    image[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
    image[3..11].copy_from_slice(b"LEMNOS  ");
    image[11..13].copy_from_slice(&512u16.to_le_bytes());
    image[13] = 1;
    image[14..16].copy_from_slice(&1u16.to_le_bytes());
    image[16] = 1;
    image[17..19].copy_from_slice(&512u16.to_le_bytes());
    image[21] = 0xF8;
    image[22..24].copy_from_slice(&(FAT_SECTORS as u16).to_le_bytes());
    image[32..36].copy_from_slice(&(SECTORS as u32).to_le_bytes());
    image[38] = 0x29;
    image[54..62].copy_from_slice(b"FAT16   ");
    image[510..512].copy_from_slice(&[0x55, 0xAA]);
    // The FAT: entries 0 and 1 are reserved; each file's chain runs to an end marker.
    let fat = 512;
    image[fat..fat + 2].copy_from_slice(&0xFFF8u16.to_le_bytes());
    image[fat + 2..fat + 4].copy_from_slice(&0xFFFFu16.to_le_bytes());
    let root = (1 + FAT_SECTORS) * 512;
    let mut next = 2usize;
    for (i, (name, bytes)) in files.iter().enumerate() {
        let clusters = bytes.len().div_ceil(512).max(1);
        for k in 0..clusters {
            let c = next + k;
            let value = if k + 1 == clusters {
                0xFFFFu16
            } else {
                (c + 1) as u16
            };
            let at = fat + c * 2;
            image[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        let entry = root + i * 32;
        image[entry..entry + 11].copy_from_slice(*name);
        image[entry + 11] = 0x20;
        image[entry + 26..entry + 28].copy_from_slice(&(next as u16).to_le_bytes());
        image[entry + 28..entry + 32].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        let at = (DATA_START + (next - 2)) * 512;
        image[at..at + bytes.len()].copy_from_slice(bytes);
        next += clusters;
    }
    image
}
