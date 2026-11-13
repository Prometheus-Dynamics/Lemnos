use super::*;

/// A FAT volume built in memory: enough structure for the placement, nothing more.
struct Vol(Vec<u8>);

impl ReadAt for Vol {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let at = offset as usize;
        let end = at + buf.len();
        if end > self.0.len() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.copy_from_slice(&self.0[at..end]);
        Ok(())
    }
}

/// A file for the image: its 8.3 name, size and cluster chain (the chain's clusters in order).
struct Entry {
    name: &'static [u8; 11],
    size: u32,
    chain: Vec<u32>,
}

/// Builds a FAT16 or FAT32 volume of `total` sectors with `spc` sectors per cluster and `files`
/// in the root directory. Each file's chain is written into both FAT copies.
fn build(fat32: bool, spc: u64, total: u64, files: &[Entry]) -> Vol {
    let reserved = 1u64;
    let nfats = 2u64;
    let bpe = if fat32 { 4u64 } else { 2 };
    let root_sectors = if fat32 { 0 } else { 32 };
    let mut fatsz = 1u64;
    let clusters = loop {
        let data = total - reserved - nfats * fatsz - root_sectors;
        let clusters = data / spc;
        let need = ((clusters + 2) * bpe).div_ceil(512);
        if need <= fatsz {
            break clusters;
        }
        fatsz = need;
    };
    let mut img = vec![0u8; (total * 512) as usize];
    // Boot sector.
    img[11..13].copy_from_slice(&512u16.to_le_bytes());
    img[13] = spc as u8;
    img[14..16].copy_from_slice(&(reserved as u16).to_le_bytes());
    img[16] = nfats as u8;
    if fat32 {
        img[17..19].copy_from_slice(&0u16.to_le_bytes());
        img[19..21].copy_from_slice(&0u16.to_le_bytes());
        img[32..36].copy_from_slice(&(total as u32).to_le_bytes());
        img[36..40].copy_from_slice(&(fatsz as u32).to_le_bytes());
        img[44..48].copy_from_slice(&2u32.to_le_bytes());
    } else {
        img[17..19].copy_from_slice(&512u16.to_le_bytes());
        img[19..21].copy_from_slice(&(total as u16).to_le_bytes());
        img[22..24].copy_from_slice(&(fatsz as u16).to_le_bytes());
    }
    img[510] = 0x55;
    img[511] = 0xAA;

    // FAT: media and end entries, then each file's chain, in both copies.
    let eoc: u64 = if fat32 { 0x0FFF_FFFF } else { 0xFFFF };
    let mut fat = vec![0u64; (clusters + 2) as usize];
    fat[0] = eoc;
    fat[1] = eoc;
    if fat32 {
        fat[2] = eoc; // the root directory: one cluster
    }
    for f in files {
        for w in f.chain.windows(2) {
            fat[w[0] as usize] = u64::from(w[1]);
        }
        fat[*f.chain.last().unwrap() as usize] = eoc;
    }
    for copy in 0..nfats {
        let base = ((reserved + copy * fatsz) * 512) as usize;
        for (i, e) in fat.iter().enumerate() {
            let at = base + i * bpe as usize;
            if fat32 {
                img[at..at + 4].copy_from_slice(&(*e as u32).to_le_bytes());
            } else {
                img[at..at + 2].copy_from_slice(&(*e as u16).to_le_bytes());
            }
        }
    }

    // Root directory entries.
    let data_start = reserved + nfats * fatsz + root_sectors;
    let root_at = if fat32 {
        (data_start * 512) as usize
    } else {
        ((reserved + nfats * fatsz) * 512) as usize
    };
    for (i, f) in files.iter().enumerate() {
        let at = root_at + i * 32;
        img[at..at + 11].copy_from_slice(f.name);
        img[at + 11] = 0x20;
        let first = f.chain[0];
        img[at + 20..at + 22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
        img[at + 26..at + 28].copy_from_slice(&(first as u16).to_le_bytes());
        img[at + 28..at + 32].copy_from_slice(&f.size.to_le_bytes());
    }
    Vol(img)
}

const REC: &[u8; 11] = b"TRIAL   REC";
const OTHER: &[u8; 11] = b"OTHER   BIN";

fn data_start(fat32: bool, total: u64, spc: u64) -> u64 {
    // Mirrors `build`: the same loop, reproduced so the expected LBA is computed independently.
    let reserved = 1u64;
    let bpe = if fat32 { 4u64 } else { 2 };
    let root_sectors = if fat32 { 0 } else { 32 };
    let mut fatsz = 1u64;
    loop {
        let clusters = (total - reserved - 2 * fatsz - root_sectors) / spc;
        let need = ((clusters + 2) * bpe).div_ceil(512);
        if need <= fatsz {
            return reserved + 2 * fatsz + root_sectors;
        }
        fatsz = need;
    }
}

#[test]
fn a_contiguous_fat16_record_file_is_placed_at_its_sector() {
    // 16384 sectors, one per cluster: about 16000 clusters, so FAT16 (FAT12 below 4085).
    let mut v = build(
        false,
        1,
        16_384,
        &[Entry {
            name: REC,
            size: 1024,
            chain: vec![3, 4],
        }],
    );
    let found = locate(&mut v, REC, 1024).unwrap();
    assert_eq!(found.first_lba, data_start(false, 16_384, 1) + (3 - 2));
}

#[test]
fn a_contiguous_fat32_record_file_is_placed_at_its_sector() {
    // 70000 sectors, one per cluster: about 69000 clusters, so FAT32.
    let mut v = build(
        true,
        1,
        70_000,
        &[Entry {
            name: REC,
            size: 1024,
            chain: vec![5, 6],
        }],
    );
    let found = locate(&mut v, REC, 1024).unwrap();
    assert_eq!(found.first_lba, data_start(true, 70_000, 1) + (5 - 2));
}

#[test]
fn a_fragmented_record_file_is_refused() {
    let mut v = build(
        true,
        1,
        70_000,
        &[Entry {
            name: REC,
            size: 1024,
            chain: vec![5, 7],
        }],
    );
    assert!(matches!(
        locate(&mut v, REC, 1024),
        Err(Refusal::Fragmented)
    ));
}

#[test]
fn a_chain_longer_than_the_size_is_refused() {
    // Size says one sector, the chain has three clusters after it.
    let mut v = build(
        true,
        1,
        70_000,
        &[Entry {
            name: REC,
            size: 512,
            chain: vec![5, 6, 7],
        }],
    );
    assert!(matches!(locate(&mut v, REC, 512), Err(Refusal::Fragmented)));
}

#[test]
fn a_missing_or_resized_file_is_refused_by_name() {
    let mut v = build(
        false,
        1,
        16_384,
        &[Entry {
            name: OTHER,
            size: 1024,
            chain: vec![3, 4],
        }],
    );
    assert!(matches!(locate(&mut v, REC, 1024), Err(Refusal::Missing)));
    let mut v = build(
        false,
        1,
        16_384,
        &[Entry {
            name: REC,
            size: 512,
            chain: vec![3],
        }],
    );
    assert!(matches!(
        locate(&mut v, REC, 1024),
        Err(Refusal::WrongSize {
            found: 512,
            want: 1024
        })
    ));
}

#[test]
fn a_volume_that_is_not_fat16_or_fat32_is_refused() {
    let mut zeros = Vol(vec![0u8; 512 * 4096]);
    assert!(matches!(
        locate(&mut zeros, REC, 1024),
        Err(Refusal::Unsupported(_))
    ));
    // FAT12: few clusters.
    let mut small = build(false, 1, 2000, &[]);
    assert!(matches!(
        locate(&mut small, REC, 1024),
        Err(Refusal::Unsupported("FAT12"))
    ));
}
