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

/// The FAT copies this writer makes: two, as the board's known-good boot partition (FAT16, made by
/// mtools) has them. Nothing writes a FAT at run time, so the second copy costs space only, and a
/// firmware that reads the second copy sees the same table.
pub const FATS: u64 = 2;
/// The root directory's entries (FAT16 fixed table).
pub const ROOT_ENTRIES: u64 = 512;
/// The reserved sectors before the FAT (the boot sector).
pub const RESERVED: u64 = 1;

/// The FAT16 geometry of a volume of `total` sectors with `spc` sectors per cluster:
/// `(fat_sectors, clusters, data_start)`. The FAT is sized for every cluster the data area holds.
pub fn geometry(total: u64, spc: u64) -> (u64, u64, u64) {
    let root = ROOT_ENTRIES * 32 / 512;
    let mut fat = 1u64;
    loop {
        let clusters = (total - RESERVED - FATS * fat - root) / spc;
        let need = ((clusters + 2) * 2).div_ceil(512);
        if need <= fat {
            return (fat, clusters, RESERVED + FATS * fat + root);
        }
        fat = need;
    }
}

/// The cluster size the reader and the image agree on, or `None` when no FAT16 cluster size exists
/// for `total` sectors: the smallest power-of-two number of sectors whose volume keeps FAT16's
/// cluster count, below 65,525 and at least 4,085 (`bootfat.rs`). At 79 MiB (161,792 sectors) that
/// is 4 sectors, 2 KiB clusters, which the consumer's own image also uses.
pub fn try_cluster_sectors(total: u64) -> Option<u64> {
    if total < 4096 + RESERVED + ROOT_ENTRIES * 32 / 512 {
        return None;
    }
    let mut spc = 1u64;
    while spc <= 128 {
        let (_, clusters, _) = geometry(total, spc);
        if clusters < 65_525 {
            return (clusters >= 4_085).then_some(spc);
        }
        spc *= 2;
    }
    None
}

/// The cluster size [`try_cluster_sectors`] gives, for a volume that has one.
pub fn cluster_sectors(total: u64) -> u64 {
    try_cluster_sectors(total)
        .unwrap_or_else(|| panic!("volume of {total} sectors has no FAT16 cluster size"))
}

/// The clusters a tree takes at `spc` sectors per cluster: each file its own run of at least one
/// cluster, each directory one cluster. This is the count [`volume`] needs for the data area.
pub fn clusters_needed(root: &[Node], spc: u64) -> u64 {
    let cluster_bytes = (spc * 512) as usize;
    root.iter()
        .map(|node| match node {
            Node::File { data, .. } => data.len().div_ceil(cluster_bytes).max(1) as u64,
            Node::Dir { children, .. } => 1 + clusters_needed(children, spc),
        })
        .sum()
}

/// One entry of a tree the test volume holds: a file with its bytes, or a directory with its
/// children. A name that is not a valid 8.3 name is written with long-name entries; its short
/// name is the usual `BASE~1.EXT` form. A directory is one cluster.
pub enum Node {
    /// A file, with its bytes in contiguous clusters.
    File {
        /// The name as the directory shows it (`initramfs.cpio`, `TRIAL.REC`).
        name: &'static str,
        /// The file's bytes.
        data: Vec<u8>,
    },
    /// A directory, one cluster, with its children.
    Dir {
        /// The name.
        name: &'static str,
        /// The entries inside.
        children: Vec<Node>,
    },
}

/// Whether `name` is a valid 8.3 name (so no long-name entry is needed).
fn is_83(name: &str) -> bool {
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => (name, ""),
    };
    !base.is_empty()
        && base.len() <= 8
        && ext.len() <= 3
        && base
            .bytes()
            .chain(ext.bytes())
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The 11-byte short name of `name`: the 8.3 form when it fits, else `BASE~1EXT`.
fn short_name(name: &str) -> [u8; 11] {
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => (name, ""),
    };
    let clean = |s: &str| -> String {
        s.bytes()
            .filter(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
            .map(|b| b.to_ascii_uppercase() as char)
            .collect()
    };
    let (b, e) = if is_83(name) {
        (clean(base), clean(ext))
    } else {
        let mut b: String = clean(base).chars().take(6).collect();
        b.push_str("~1");
        (b, clean(ext).chars().take(3).collect())
    };
    let mut out = [b' '; 11];
    out[..b.len()].copy_from_slice(b.as_bytes());
    out[8..8 + e.len()].copy_from_slice(e.as_bytes());
    out
}

/// The FAT short-name checksum that every long-name entry carries.
fn short_checksum(sfn: &[u8; 11]) -> u8 {
    sfn.iter()
        .fold(0u8, |sum, &b| sum.rotate_right(1).wrapping_add(b))
}

/// The long-name entries for `name` (on disk: last part first), for the short name `sfn`.
fn long_entries(name: &str, sfn: &[u8; 11]) -> Vec<[u8; 32]> {
    // The byte offsets of the 13 UCS-2 characters in a long-name entry (as bootfat reads them).
    const AT: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
    let chars: Vec<u16> = name.encode_utf16().collect();
    let parts = chars.len().div_ceil(13);
    let sum = short_checksum(sfn);
    let mut out = Vec::with_capacity(parts);
    for i in (0..parts).rev() {
        let mut e = [0u8; 32];
        e[0] = (i + 1) as u8 | if i + 1 == parts { 0x40 } else { 0 };
        e[11] = 0x0F;
        e[13] = sum;
        for (k, &at) in AT.iter().enumerate() {
            let idx = i * 13 + k;
            let c = match idx.cmp(&chars.len()) {
                std::cmp::Ordering::Less => chars[idx],
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 0xFFFF,
            };
            e[at..at + 2].copy_from_slice(&c.to_le_bytes());
        }
        out.push(e);
    }
    out
}

/// The short entry for `sfn`, with its first cluster and size.
fn short_entry(sfn: &[u8; 11], attr: u8, first: u32, size: u32) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0..11].copy_from_slice(sfn);
    e[11] = attr;
    e[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
    e[26..28].copy_from_slice(&(first as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    e
}

/// The volume being filled.
struct Filler {
    image: Vec<u8>,
    next: u64,
    spc: u64,
    data_start: u64,
    fat_start: u64,
    /// Entries one directory cluster holds.
    dir_entries: usize,
}

impl Filler {
    fn set_fat(&mut self, c: u64, value: u16) {
        let at = (self.fat_start + c * 2) as usize;
        self.image[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    /// The byte offset of cluster `c`.
    fn cluster_at(&self, c: u64) -> usize {
        ((self.data_start + (c - 2) * self.spc) * 512) as usize
    }

    /// Fills `children` into one directory's entries. `own` is the directory's cluster (`None` for
    /// the root), `parent` its parent's (`0` for the root).
    fn fill(&mut self, children: &[Node], own: Option<u64>, parent: u64) -> Vec<[u8; 32]> {
        let mut entries: Vec<[u8; 32]> = Vec::new();
        if let Some(own) = own {
            let mut dot = short_entry(b".          ", 0x10, own as u32, 0);
            dot[0] = b'.';
            let mut dotdot = short_entry(b"..         ", 0x10, parent as u32, 0);
            dotdot[0] = b'.';
            dotdot[1] = b'.';
            entries.push(dot);
            entries.push(dotdot);
        }
        for child in children {
            match child {
                Node::File { name, data } => {
                    let cluster_bytes = (self.spc * 512) as usize;
                    let clusters = data.len().div_ceil(cluster_bytes).max(1) as u64;
                    let first = self.next;
                    for k in 0..clusters {
                        let c = first + k;
                        let value = if k + 1 == clusters {
                            0xFFFF
                        } else {
                            (c + 1) as u16
                        };
                        self.set_fat(c, value);
                    }
                    let at = self.cluster_at(first);
                    self.image[at..at + data.len()].copy_from_slice(data);
                    self.next += clusters;
                    let sfn = short_name(name);
                    if !is_83(name) {
                        entries.extend(long_entries(name, &sfn));
                    }
                    entries.push(short_entry(&sfn, 0x20, first as u32, data.len() as u32));
                }
                Node::Dir { name, children } => {
                    let c = self.next;
                    self.next += 1;
                    self.set_fat(c, 0xFFFF);
                    let sub = self.fill(children, Some(c), own.unwrap_or(0));
                    assert!(
                        sub.len() <= self.dir_entries,
                        "directory {name} holds at most {} entries",
                        self.dir_entries
                    );
                    let mut block = vec![0u8; (self.spc * 512) as usize];
                    for (i, e) in sub.iter().enumerate() {
                        block[i * 32..i * 32 + 32].copy_from_slice(e);
                    }
                    let at = self.cluster_at(c);
                    self.image[at..at + block.len()].copy_from_slice(&block);
                    let sfn = short_name(name);
                    if !is_83(name) {
                        entries.extend(long_entries(name, &sfn));
                    }
                    entries.push(short_entry(&sfn, 0x10, c as u32, 0));
                }
            }
        }
        entries
    }
}

/// Builds a FAT16 volume of `total` sectors, the cluster size from [`cluster_sectors`], holding
/// `root`. Files are contiguous, in the order given, so their placement is deterministic. The boot
/// sector carries no timestamp, serial or label that depends on the host: the same tree gives the
/// same bytes. Panics when the root holds more than 512 entries or a directory more than its cluster.
pub fn volume(root: &[Node], total: u64) -> Vec<u8> {
    let spc = cluster_sectors(total);
    let (fat, _clusters, data_start) = geometry(total, spc);
    let mut image = vec![0u8; (total * 512) as usize];
    image[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
    image[3..11].copy_from_slice(b"LEMNOS  ");
    image[11..13].copy_from_slice(&512u16.to_le_bytes());
    image[13] = spc as u8;
    image[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
    image[16] = FATS as u8;
    image[17..19].copy_from_slice(&(ROOT_ENTRIES as u16).to_le_bytes());
    image[21] = 0xF8;
    image[22..24].copy_from_slice(&(fat as u16).to_le_bytes());
    image[24..26].copy_from_slice(&63u16.to_le_bytes());
    image[26..28].copy_from_slice(&255u16.to_le_bytes());
    image[32..36].copy_from_slice(&(total as u32).to_le_bytes());
    image[38] = 0x29;
    image[39..43].copy_from_slice(&0x4C45_4D4Eu32.to_le_bytes());
    image[54..62].copy_from_slice(b"FAT16   ");
    image[510..512].copy_from_slice(&[0x55, 0xAA]);
    let fat_start = RESERVED * 512;
    image[fat_start as usize..fat_start as usize + 4].copy_from_slice(&[0xF8, 0xFF, 0xFF, 0xFF]);
    let mut f = Filler {
        image,
        next: 2,
        spc,
        data_start,
        fat_start,
        dir_entries: (spc * 16) as usize,
    };
    let entries = f.fill(root, None, 0);
    assert!(
        entries.len() <= ROOT_ENTRIES as usize,
        "root directory holds at most 512 entries"
    );
    let root_at = ((RESERVED + FATS * fat) * 512) as usize;
    for (i, e) in entries.iter().enumerate() {
        f.image[root_at + i * 32..root_at + i * 32 + 32].copy_from_slice(e);
    }
    // The second and later copies are identical to the first.
    let one = (fat * 512) as usize;
    let first = fat_start as usize;
    for copy in 1..FATS as usize {
        let at = first + copy * one;
        let table = f.image[first..first + one].to_vec();
        f.image[at..at + one].copy_from_slice(&table);
    }
    f.image
}

/// The small test volume: 16,384 sectors (8 MiB), the cluster rule gives one sector per cluster.
pub fn tree(root: &[Node]) -> Vec<u8> {
    volume(root, 16_384)
}
