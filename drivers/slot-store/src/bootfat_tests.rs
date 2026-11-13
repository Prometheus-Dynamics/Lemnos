use super::*;

/// A FAT volume built in memory: enough structure for the placement, nothing more.
pub(crate) struct Vol(pub(crate) Vec<u8>);

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
pub(crate) struct Entry {
    pub(crate) name: &'static [u8; 11],
    pub(crate) size: u32,
    pub(crate) chain: Vec<u32>,
}

/// Builds a FAT16 or FAT32 volume of `total` sectors with `spc` sectors per cluster and `files`
/// in the root directory. Each file's chain is written into both FAT copies.
pub(crate) fn build(fat32: bool, spc: u64, total: u64, files: &[Entry]) -> Vol {
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

pub(crate) const REC: &[u8; 11] = b"TRIAL   REC";
const OTHER: &[u8; 11] = b"OTHER   BIN";

pub(crate) fn data_start(fat32: bool, total: u64, spc: u64) -> u64 {
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

// ---- The reader: a tree of directories and files, with long names and fragmented chains ----

/// One item of a test tree: a file (its bytes) or a directory (its children), with its long name,
/// its 8.3 name and whether it carries a long-name entry.
pub(crate) enum Kind {
    Dir(Vec<Item>),
    File(Vec<u8>),
}

pub(crate) struct Item {
    pub(crate) long: &'static str,
    pub(crate) short: &'static [u8; 11],
    pub(crate) lfn: bool,
    pub(crate) kind: Kind,
}

const TOTAL: u64 = 16384;

/// The FAT16 geometry the tree builder uses: one sector per cluster, two FATs, a 512-entry root.
struct Geometry {
    fatsz: u64,
    clusters: u64,
    data_start: u64,
}

fn geometry() -> Geometry {
    let mut fatsz = 1u64;
    loop {
        let clusters = TOTAL - 1 - 2 * fatsz - 32;
        let need = ((clusters + 2) * 2).div_ceil(512);
        if need <= fatsz {
            return Geometry {
                fatsz,
                clusters,
                data_start: 1 + 2 * fatsz + 32,
            };
        }
        fatsz = need;
    }
}

/// Builds the tree's image. With `gap`, every chain skips a cluster between its links, so the
/// files are fragmented.
pub(crate) fn tree(root: &[Item], gap: bool) -> Vol {
    let g = geometry();
    let mut img = vec![0u8; (TOTAL * 512) as usize];
    img[11..13].copy_from_slice(&512u16.to_le_bytes());
    img[13] = 1;
    img[14..16].copy_from_slice(&1u16.to_le_bytes());
    img[16] = 2;
    img[17..19].copy_from_slice(&512u16.to_le_bytes());
    img[19..21].copy_from_slice(&(TOTAL as u16).to_le_bytes());
    img[22..24].copy_from_slice(&(g.fatsz as u16).to_le_bytes());
    img[510] = 0x55;
    img[511] = 0xAA;
    let mut fat = vec![0u64; (g.clusters + 2) as usize];
    fat[0] = 0xFFFF;
    fat[1] = 0xFFFF;
    let mut ctx = Ctx {
        img,
        fat,
        next: 2,
        gap,
        data_start: g.data_start,
    };
    let mut entries = Vec::new();
    for item in root {
        place(&mut ctx, item, &mut entries);
    }
    assert!(entries.len() <= 512 * 32, "the root holds 512 entries");
    let root_at = ((1 + 2 * g.fatsz) * 512) as usize;
    ctx.img[root_at..root_at + entries.len()].copy_from_slice(&entries);
    for copy in 0..2u64 {
        let base = ((1 + copy * g.fatsz) * 512) as usize;
        for (i, e) in ctx.fat.iter().enumerate() {
            ctx.img[base + i * 2..base + i * 2 + 2].copy_from_slice(&(*e as u16).to_le_bytes());
        }
    }
    Vol(ctx.img)
}

struct Ctx {
    img: Vec<u8>,
    fat: Vec<u64>,
    next: u64,
    gap: bool,
    data_start: u64,
}

impl Ctx {
    /// `n` clusters in a chain, each linked to the next and the last ending it.
    fn alloc(&mut self, n: usize) -> Vec<u64> {
        let mut chain = Vec::new();
        for _ in 0..n {
            if self.gap {
                self.next += 1;
            }
            chain.push(self.next);
            self.next += 1;
        }
        for w in chain.windows(2) {
            self.fat[w[0] as usize] = w[1];
        }
        if let Some(&last) = chain.last() {
            self.fat[last as usize] = 0xFFFF;
        }
        chain
    }

    /// Writes `bytes` over the chain, one sector per cluster, zero-padding the last.
    fn write(&mut self, chain: &[u64], bytes: &[u8]) {
        for (i, &c) in chain.iter().enumerate() {
            let at = ((self.data_start + (c - 2)) * 512) as usize;
            let from = (i * 512).min(bytes.len());
            let to = ((i + 1) * 512).min(bytes.len());
            self.img[at..at + 512].fill(0);
            self.img[at..at + (to - from)].copy_from_slice(&bytes[from..to]);
        }
    }
}

/// Lays out one item, appending its directory entries to `entries`.
fn place(ctx: &mut Ctx, item: &Item, entries: &mut Vec<u8>) {
    let (first, size, attr) = match &item.kind {
        Kind::File(data) => {
            let n = (data.len().div_ceil(512)).max(usize::from(!data.is_empty()));
            let chain = if n == 0 { Vec::new() } else { ctx.alloc(n) };
            ctx.write(&chain, data);
            (
                chain.first().copied().unwrap_or(0),
                data.len() as u32,
                0x20u8,
            )
        }
        Kind::Dir(kids) => {
            let mut inner = Vec::new();
            for kid in kids {
                place(ctx, kid, &mut inner);
            }
            let n = (inner.len().div_ceil(512)).max(1);
            let chain = ctx.alloc(n);
            ctx.write(&chain, &inner);
            (chain[0], 0, 0x10u8)
        }
    };
    if item.lfn {
        let units: Vec<u16> = item.long.encode_utf16().collect();
        let n = units.len().div_ceil(13);
        let sum = short_checksum(item.short);
        for seq in (1..=n).rev() {
            let mut e = [0u8; 32];
            e[0] = seq as u8 | if seq == n { 0x40 } else { 0 };
            for (k, &at) in LFN_AT.iter().enumerate() {
                let idx = (seq - 1) * 13 + k;
                let unit = if idx < units.len() {
                    units[idx]
                } else if idx == units.len() {
                    0
                } else {
                    0xFFFF
                };
                e[at..at + 2].copy_from_slice(&unit.to_le_bytes());
            }
            e[11] = 0x0F;
            e[13] = sum;
            entries.extend_from_slice(&e);
        }
    }
    let mut e = [0u8; 32];
    e[0..11].copy_from_slice(item.short);
    e[11] = attr;
    e[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
    e[26..28].copy_from_slice(&(first as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    entries.extend_from_slice(&e);
}

/// The first cluster of the root entry with 8.3 name `short`.
fn root_first(vol: &Vol, short: &[u8; 11]) -> u64 {
    let g = geometry();
    let at = ((1 + 2 * g.fatsz) * 512) as usize;
    let root = &vol.0[at..at + 512 * 32];
    let e = root
        .as_chunks::<32>()
        .0
        .iter()
        .find(|e| &e[0..11] == short)
        .expect("the entry is in the root");
    (u64::from(u16::from_le_bytes([e[20], e[21]])) << 16)
        | u64::from(u16::from_le_bytes([e[26], e[27]]))
}

/// Sets FAT entry `c` in both copies.
fn set_fat(vol: &mut Vol, c: u64, value: u16) {
    let g = geometry();
    for copy in 0..2u64 {
        let at = ((1 + copy * g.fatsz) * 512 + c * 2) as usize;
        vol.0[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }
}

/// The clusters of a chain, as the FAT now says, from `first`.
fn chain_of(vol: &Vol, first: u64) -> Vec<u64> {
    let mut out = vec![first];
    let mut c = first;
    loop {
        let at = (512 + c * 2) as usize;
        let next = u64::from(u16::from_le_bytes([vol.0[at], vol.0[at + 1]]));
        if next >= 0xFFF8 || out.len() > 64 {
            break;
        }
        out.push(next);
        c = next;
    }
    out
}

fn sample_tree() -> Vec<Item> {
    let kernel: Vec<u8> = (0..1500u32).map(|i| (i % 251) as u8).collect();
    let dtb: Vec<u8> = (0..300u32).map(|i| (i * 7 % 256) as u8).collect();
    vec![
        Item {
            long: "kernel8.img",
            short: b"KERNEL8 IMG",
            lfn: true,
            kind: Kind::File(kernel),
        },
        Item {
            long: "overlays",
            short: b"OVERLAYS   ",
            lfn: true,
            kind: Kind::Dir(vec![
                Item {
                    long: "README",
                    short: b"README     ",
                    lfn: false,
                    kind: Kind::File(b"text".to_vec()),
                },
                Item {
                    long: "bcm2712-rpi-5-b.dtb",
                    short: b"BCM2712~DTB",
                    lfn: true,
                    kind: Kind::File(dtb),
                },
            ]),
        },
        Item {
            long: "cmdline.txt",
            short: b"CMDLINE TXT",
            lfn: true,
            kind: Kind::File(b"console=tty1 os.slot=a\n".to_vec()),
        },
    ]
}

fn kernel_bytes() -> Vec<u8> {
    (0..1500u32).map(|i| (i % 251) as u8).collect()
}

#[test]
fn a_short_name_file_in_a_subdirectory_reads_back() {
    let mut vol = tree(&sample_tree(), false);
    assert_eq!(read_file(&mut vol, "overlays/README", 64).unwrap(), b"text");
}

#[test]
fn a_long_name_across_three_clusters_reads_back_whole() {
    let mut vol = tree(&sample_tree(), false);
    assert_eq!(
        read_file(&mut vol, "kernel8.img", 4096).unwrap(),
        kernel_bytes()
    );
    assert_eq!(
        read_file(&mut vol, "KERNEL8.IMG", 4096).unwrap(),
        kernel_bytes()
    );
}

#[test]
fn a_long_name_in_a_subdirectory_is_found_by_its_long_name_and_its_short_one() {
    let mut vol = tree(&sample_tree(), false);
    let dtb: Vec<u8> = (0..300u32).map(|i| (i * 7 % 256) as u8).collect();
    assert_eq!(
        read_file(&mut vol, "overlays/bcm2712-rpi-5-b.dtb", 4096).unwrap(),
        dtb
    );
    assert_eq!(
        read_file(&mut vol, "overlays/BCM2712~.DTB", 4096).unwrap(),
        dtb
    );
}

#[test]
fn a_fragmented_file_is_read_by_following_its_chain() {
    let mut vol = tree(&sample_tree(), true);
    let first = root_first(&vol, b"KERNEL8 IMG");
    assert!(chain_of(&vol, first).windows(2).any(|w| w[1] != w[0] + 1));
    assert_eq!(
        read_file(&mut vol, "kernel8.img", 4096).unwrap(),
        kernel_bytes()
    );
}

#[test]
fn a_missing_name_is_missing_and_a_path_through_a_file_is_refused() {
    let mut vol = tree(&sample_tree(), false);
    assert!(matches!(
        read_file(&mut vol, "nope", 64),
        Err(Refusal::Missing)
    ));
    assert!(matches!(
        read_file(&mut vol, "cmdline.txt/x", 64),
        Err(Refusal::Corrupt(_))
    ));
    assert!(matches!(
        read_file(&mut vol, "overlays//README", 64),
        Err(Refusal::Corrupt(_))
    ));
}

#[test]
fn a_directory_asked_for_as_a_file_is_refused() {
    let mut vol = tree(&sample_tree(), false);
    assert!(matches!(
        read_file(&mut vol, "overlays", 64),
        Err(Refusal::Corrupt(_))
    ));
}

#[test]
fn a_file_over_the_limit_is_refused_before_it_is_read() {
    let mut vol = tree(&sample_tree(), false);
    assert!(matches!(
        read_file(&mut vol, "kernel8.img", 100),
        Err(Refusal::TooLarge {
            found: 1500,
            limit: 100
        })
    ));
}

#[test]
fn a_chain_that_loops_is_refused_by_name() {
    let mut vol = tree(&sample_tree(), false);
    let chain = chain_of(&vol, root_first(&vol, b"KERNEL8 IMG"));
    assert_eq!(chain.len(), 3);
    set_fat(&mut vol, chain[2], chain[0] as u16);
    assert!(matches!(
        read_file(&mut vol, "kernel8.img", 4096),
        Err(Refusal::Corrupt(_))
    ));
}

#[test]
fn a_chain_through_a_free_cluster_is_refused() {
    let mut vol = tree(&sample_tree(), false);
    let chain = chain_of(&vol, root_first(&vol, b"KERNEL8 IMG"));
    set_fat(&mut vol, chain[1], 0);
    assert!(matches!(
        read_file(&mut vol, "kernel8.img", 4096),
        Err(Refusal::Corrupt(_))
    ));
}

#[test]
fn a_chain_through_a_bad_cluster_is_refused() {
    let mut vol = tree(&sample_tree(), false);
    let chain = chain_of(&vol, root_first(&vol, b"KERNEL8 IMG"));
    set_fat(&mut vol, chain[1], 0xFFF7);
    assert!(matches!(
        read_file(&mut vol, "kernel8.img", 4096),
        Err(Refusal::Corrupt(_))
    ));
}

#[test]
fn a_chain_that_ends_before_its_size_is_refused() {
    let mut vol = tree(&sample_tree(), false);
    let chain = chain_of(&vol, root_first(&vol, b"KERNEL8 IMG"));
    set_fat(&mut vol, chain[1], 0xFFFF);
    assert!(matches!(
        read_file(&mut vol, "kernel8.img", 4096),
        Err(Refusal::Corrupt(_))
    ));
}

#[test]
fn a_first_cluster_outside_the_volume_is_refused() {
    // Cluster 16368 is a FAT entry that exists but lies past the volume's clusters. A one-cluster
    // file is pointed at it; the FAT entry says end-of-chain, so only the range check refuses it.
    let mut vol = tree(&sample_tree(), false);
    set_fat(&mut vol, 16368, 0xFFFF);
    let g = geometry();
    let at = ((1 + 2 * g.fatsz) * 512) as usize;
    let entry = (0..512)
        .map(|i| at + i * 32)
        .find(|&o| &vol.0[o..o + 11] == b"CMDLINE TXT")
        .unwrap();
    vol.0[entry + 20..entry + 22].copy_from_slice(&0u16.to_le_bytes());
    vol.0[entry + 26..entry + 28].copy_from_slice(&16368u16.to_le_bytes());
    assert!(matches!(
        read_file(&mut vol, "cmdline.txt", 4096),
        Err(Refusal::Corrupt(_))
    ));
}

#[test]
fn a_long_name_whose_checksum_does_not_match_is_not_used() {
    let mut vol = tree(&sample_tree(), false);
    let dir = root_first(&vol, b"OVERLAYS   ");
    let at = ((geometry().data_start + (dir - 2)) * 512) as usize;
    // The README entry is first; the dtb's two long-name entries follow it. Both carry the same
    // wrong checksum, so the run is consistent and only the match against the short entry refuses.
    vol.0[at + 32 + 13] ^= 0x5A;
    vol.0[at + 64 + 13] ^= 0x5A;
    assert!(matches!(
        read_file(&mut vol, "overlays/bcm2712-rpi-5-b.dtb", 4096),
        Err(Refusal::Missing)
    ));
    assert!(read_file(&mut vol, "overlays/BCM2712~.DTB", 4096).is_ok());
}

#[test]
fn corrupt_fat_and_directory_bytes_never_panic_and_never_read_past_the_size() {
    // A deterministic walk over single-byte corruptions of the FAT and the directories.
    let base = tree(&sample_tree(), false);
    let g = geometry();
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let region_end = ((g.data_start + 64) * 512) as usize;
    for _ in 0..400 {
        let mut vol = Vol(base.0.clone());
        let at = 512 + (next() as usize % (region_end - 512));
        vol.0[at] ^= (next() as u8) | 1;
        for path in [
            "kernel8.img",
            "overlays/README",
            "overlays/bcm2712-rpi-5-b.dtb",
            "cmdline.txt",
        ] {
            if let Ok(bytes) = read_file(&mut vol, path, 4096) {
                assert!(bytes.len() <= 4096, "{path} read {} bytes", bytes.len());
            }
        }
    }
}
