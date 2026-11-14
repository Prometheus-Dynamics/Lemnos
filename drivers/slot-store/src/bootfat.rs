//! Finds a pre-created record file's sector on a FAT16 or FAT32 partition, and reads a file's
//! bytes through its cluster chain ([`read_file`]), reading the volume only: no mount, no write. A record file must be one contiguous run of clusters whose size is
//! exactly the size the image builder wrote, so a record's copies are two sectors at a known LBA.
//! Anything else (missing, fragmented, a different size, FAT12, a bad boot sector) is refused by
//! name: the caller writes nothing.
//!
//! The layout parsed is the one in the Microsoft FAT specification (boot sector offsets 11..90,
//! FAT16 entries of 2 bytes, FAT32 entries of 4 bytes masked to 28 bits, fixed root directory on
//! FAT16, a root chain on FAT32). The `fs/fat` driver in Linux reads the same fields
//! (`fs/fat/inode.c`, `fat_read_bpb`).

use std::fmt;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

/// Reads bytes at an offset: a block node, or a test image.
pub trait ReadAt {
    /// Fills `buf` from `offset`.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
}

impl ReadAt for File {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.read_exact_at(buf, offset)
    }
}

/// Where a record file is: its first sector, relative to the partition.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Found {
    /// The first sector of the file's first cluster.
    pub first_lba: u64,
}

/// Why a file could not be placed. Each one leaves the device unwritten.
#[derive(Debug)]
pub enum Refusal {
    /// The boot sector does not describe a FAT16 or FAT32 volume with 512-byte sectors.
    Unsupported(&'static str),
    /// No such file in the root directory.
    Missing,
    /// The file exists but is not exactly the size the image builder wrote.
    WrongSize {
        /// The size on the volume.
        found: u32,
        /// The size expected.
        want: u64,
    },
    /// The file's clusters are not one contiguous run ending where its size says.
    Fragmented,
    /// The device failed.
    Io(io::Error),
    /// A directory, cluster chain or entry is not what the FAT says it must be (a loop, a free or
    /// bad cluster in a chain, a chain that disagrees with its size, a path component that is not
    /// a directory). The reader names the fault and reads nothing more.
    Corrupt(&'static str),
    /// The file is larger than the caller will read.
    TooLarge {
        /// The size on the volume.
        found: u64,
        /// The most the caller reads.
        limit: u64,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Unsupported(why) => {
                write!(f, "boot partition is not a usable FAT volume: {why}")
            }
            Refusal::Missing => write!(f, "record file is missing: the image is not ours"),
            Refusal::WrongSize { found, want } => write!(
                f,
                "record file is {found} bytes, expected {want}: the image is not ours"
            ),
            Refusal::Fragmented => write!(f, "record file is fragmented or moved: refused"),
            Refusal::Io(e) => write!(f, "boot partition read failed: {e}"),
            Refusal::Corrupt(why) => write!(f, "boot partition's FAT is corrupt: {why}"),
            Refusal::TooLarge { found, limit } => {
                write!(f, "file is {found} bytes, more than the {limit} allowed")
            }
        }
    }
}

impl std::error::Error for Refusal {}

impl From<io::Error> for Refusal {
    fn from(e: io::Error) -> Self {
        Refusal::Io(e)
    }
}

/// The fields of the volume the placement needs.
#[derive(Debug, Clone, Copy)]
struct Volume {
    spc: u64,
    fat_start: u64,
    fat_bytes_per_entry: u64,
    fat32: bool,
    root_start: u64,
    root_entries: u64,
    root_cluster: u32,
    data_start: u64,
    clusters: u64,
}

fn le16(b: &[u8], at: usize) -> u64 {
    u64::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

fn le32(b: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]))
}

fn volume<D: ReadAt>(dev: &mut D) -> Result<Volume, Refusal> {
    let mut b = [0u8; 512];
    dev.read_at(0, &mut b)?;
    if le16(&b, 11) != 512 {
        return Err(Refusal::Unsupported("sector size is not 512"));
    }
    let spc = u64::from(b[13]);
    if spc == 0 || !spc.is_power_of_two() {
        return Err(Refusal::Unsupported(
            "sectors per cluster is not a power of two",
        ));
    }
    let reserved = le16(&b, 14);
    let nfats = u64::from(b[16]);
    if nfats == 0 {
        return Err(Refusal::Unsupported("no FAT copies"));
    }
    let root_entries = le16(&b, 17);
    let tot16 = le16(&b, 19);
    let fatsz16 = le16(&b, 22);
    let tot32 = le32(&b, 32);
    let fatsz32 = le32(&b, 36);
    let root_cluster = le32(&b, 44) as u32;
    let total = if tot16 != 0 { tot16 } else { tot32 };
    let fatsz = if fatsz16 != 0 { fatsz16 } else { fatsz32 };
    let root_sectors = (root_entries * 32).div_ceil(512);
    let meta = reserved + nfats * fatsz;
    if total <= meta + root_sectors {
        return Err(Refusal::Unsupported("no data area"));
    }
    let clusters = (total - meta - root_sectors) / spc;
    let (fat32, bytes_per_entry) = if clusters < 4085 {
        return Err(Refusal::Unsupported("FAT12"));
    } else if clusters < 65525 {
        (false, 2u64)
    } else {
        (true, 4u64)
    };
    if !fat32 && root_entries == 0 {
        return Err(Refusal::Unsupported("FAT16 without a root directory"));
    }
    if fat32 && root_entries != 0 {
        return Err(Refusal::Unsupported("FAT32 with a fixed root directory"));
    }
    Ok(Volume {
        spc,
        fat_start: reserved * 512,
        fat_bytes_per_entry: bytes_per_entry,
        fat32,
        root_start: meta * 512,
        root_entries,
        root_cluster,
        data_start: meta + root_sectors,
        clusters,
    })
}

/// The FAT entry for cluster `c`.
fn fat_entry<D: ReadAt>(dev: &mut D, v: &Volume, c: u64) -> Result<u64, Refusal> {
    let at = v.fat_start + c * v.fat_bytes_per_entry;
    if v.fat32 {
        let mut b = [0u8; 4];
        dev.read_at(at, &mut b)?;
        Ok(u64::from(u32::from_le_bytes(b)) & 0x0FFF_FFFF)
    } else {
        let mut b = [0u8; 2];
        dev.read_at(at, &mut b)?;
        Ok(u64::from(u16::from_le_bytes(b)))
    }
}

fn is_end(v: &Volume, entry: u64) -> bool {
    if v.fat32 {
        entry >= 0x0FFF_FFF8
    } else {
        entry >= 0xFFF8
    }
}

/// Looks for the 8.3 name `name` (eleven bytes, space padded, upper case) among the root
/// directory's entries, and returns its first cluster and size.
fn find<D: ReadAt>(
    dev: &mut D,
    v: &Volume,
    name: &[u8; 11],
) -> Result<Option<(u64, u32)>, Refusal> {
    let mut entries: Vec<u8> = Vec::new();
    if v.fat32 {
        // Follow the root directory's chain, one cluster at a time.
        let mut c = u64::from(v.root_cluster);
        let mut hops = 0u64;
        loop {
            if c < 2 || c >= v.clusters + 2 || hops > v.clusters {
                return Err(Refusal::Unsupported("root directory chain is broken"));
            }
            let at = (v.data_start + (c - 2) * v.spc) * 512;
            let mut buf = vec![0u8; (v.spc * 512) as usize];
            dev.read_at(at, &mut buf)?;
            entries.extend_from_slice(&buf);
            if let Some(hit) = scan(&entries, name) {
                return Ok(Some(hit));
            }
            let next = fat_entry(dev, v, c)?;
            if is_end(v, next) {
                return Ok(None);
            }
            c = next;
            hops += 1;
        }
    } else {
        let mut buf = vec![0u8; (v.root_entries * 32) as usize];
        dev.read_at(v.root_start, &mut buf)?;
        Ok(scan(&buf, name))
    }
}

/// The first live entry named `name` in a directory's bytes, or `None` at the end marker.
fn scan(dir: &[u8], name: &[u8; 11]) -> Option<(u64, u32)> {
    for e in dir.chunks(32) {
        if e.len() < 32 {
            break;
        }
        if e[0] == 0x00 {
            return None;
        }
        if e[0] == 0xE5 {
            continue;
        }
        let attr = e[11];
        // Long names (0x0F) and directories or volume labels (0x10, 0x08) are not record files.
        if attr & 0x0F == 0x0F || attr & 0x18 != 0 {
            continue;
        }
        if &e[0..11] == name {
            let hi = u64::from(u16::from_le_bytes([e[20], e[21]]));
            let lo = u64::from(u16::from_le_bytes([e[26], e[27]]));
            let size = u32::from_le_bytes([e[28], e[29], e[30], e[31]]);
            return Some(((hi << 16) | lo, size));
        }
    }
    None
}

/// Places the record file `name` (eleven bytes, 8.3, upper case, space padded) of exactly `want`
/// bytes. Reads the volume only. The file's clusters must be one run in order, and the FAT must end
/// the chain exactly where the size says.
pub fn locate<D: ReadAt>(dev: &mut D, name: &[u8; 11], want: u64) -> Result<Found, Refusal> {
    let v = volume(dev)?;
    let (first, size) = find(dev, &v, name)?.ok_or(Refusal::Missing)?;
    place(dev, &v, first, size, want)
}

/// Places a file of `want` bytes at `path` (`/`-separated, from the root; each component a long or
/// short name, as [`read_file`] reads them). The file must be exactly `want` bytes and one
/// contiguous run of clusters that the FAT ends where its size says. Reads the volume only.
pub fn locate_path<D: ReadAt>(dev: &mut D, path: &str, want: u64) -> Result<Found, Refusal> {
    let v = volume(dev)?;
    let (first, size) = find_path(dev, &v, path)?;
    place(dev, &v, first, size, want)
}

/// The size a file at `path` has in its directory entry, read from the volume only. Its clusters are
/// not checked: this is how the driver learns the bay layout the image was built with, and
/// [`locate_path`] then checks every file against that layout.
pub fn size_of_path<D: ReadAt>(dev: &mut D, path: &str) -> Result<u64, Refusal> {
    let v = volume(dev)?;
    Ok(u64::from(find_path(dev, &v, path)?.1))
}

/// The first cluster and the size of the file at `path`, from its directory entries.
fn find_path<D: ReadAt>(dev: &mut D, v: &Volume, path: &str) -> Result<(u64, u32), Refusal> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(Refusal::Corrupt("a path has an empty component"));
    }
    let last = parts.len() - 1;
    let mut dir = Dir::Root;
    for (i, part) in parts.iter().enumerate() {
        let bytes = dir_bytes(dev, v, dir)?;
        let hit = find_entry(&bytes, part).ok_or(Refusal::Missing)?;
        if i < last {
            if hit.attr & ATTR_DIR == 0 {
                return Err(Refusal::Corrupt("a path component is not a directory"));
            }
            if hit.first < 2 {
                return Err(Refusal::Corrupt("a directory entry names no cluster"));
            }
            dir = Dir::Cluster(hit.first);
            continue;
        }
        if hit.attr & ATTR_DIR != 0 {
            return Err(Refusal::Corrupt("the path names a directory, not a file"));
        }
        return Ok((hit.first, hit.size));
    }
    Err(Refusal::Missing)
}

/// The placement of a found file: its size is `want`, its clusters one run ending where the FAT says.
fn place<D: ReadAt>(
    dev: &mut D,
    v: &Volume,
    first: u64,
    size: u32,
    want: u64,
) -> Result<Found, Refusal> {
    let v = *v;
    if u64::from(size) != want {
        return Err(Refusal::WrongSize { found: size, want });
    }
    if first < 2 || first >= v.clusters + 2 {
        return Err(Refusal::Fragmented);
    }
    let cluster_bytes = v.spc * 512;
    let needed = want.div_ceil(cluster_bytes).max(1);
    let mut c = first;
    for i in 0..needed {
        if i > 0 && c != first + i {
            return Err(Refusal::Fragmented);
        }
        let next = fat_entry(dev, &v, c)?;
        if i + 1 == needed {
            if !is_end(&v, next) {
                return Err(Refusal::Fragmented);
            }
        } else if next != c + 1 {
            return Err(Refusal::Fragmented);
        }
        c = next;
    }
    Ok(Found {
        first_lba: v.data_start + (first - 2) * v.spc,
    })
}

/// The file attribute bits the reader looks at.
const ATTR_DIR: u8 = 0x10;
const ATTR_VOLUME: u8 = 0x08;
const ATTR_LFN: u8 = 0x0F;
/// Where the 13 UCS-2 characters of a long-name entry sit in its 32 bytes.
const LFN_AT: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

/// A directory to read from: the FAT16 fixed root, or a directory's first cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Root,
    Cluster(u64),
}

/// A directory entry that matched.
#[derive(Debug, Clone, Copy)]
struct Hit {
    attr: u8,
    first: u64,
    size: u32,
}

/// Follows a FAT chain from `first`, at most `limit` clusters. A free, bad or out-of-range
/// cluster, a loop, or a chain longer than `limit` is refused; so is a chain that ends early
/// only when the caller checks the length.
fn cluster_chain<D: ReadAt>(
    dev: &mut D,
    v: &Volume,
    first: u64,
    limit: u64,
) -> Result<Vec<u64>, Refusal> {
    let mut out = Vec::new();
    let mut c = first;
    loop {
        if c < 2 || c >= v.clusters + 2 {
            return Err(Refusal::Corrupt(
                "a chain names a cluster outside the volume",
            ));
        }
        if out.len() as u64 >= limit {
            return Err(Refusal::Corrupt("a chain is longer than its size or loops"));
        }
        out.push(c);
        let next = fat_entry(dev, v, c)?;
        if is_end(v, next) {
            return Ok(out);
        }
        c = next;
    }
}

/// Reads the clusters of `chain` whole, in order, appending them to `out`.
fn read_clusters<D: ReadAt>(
    dev: &mut D,
    v: &Volume,
    chain: &[u64],
    out: &mut Vec<u8>,
) -> Result<(), Refusal> {
    let cluster_bytes = (v.spc * 512) as usize;
    for &c in chain {
        let at = (v.data_start + (c - 2) * v.spc) * 512;
        let start = out.len();
        out.resize(start + cluster_bytes, 0);
        dev.read_at(at, &mut out[start..])?;
    }
    Ok(())
}

/// The bytes of a directory: the fixed FAT16 root, or its chain.
fn dir_bytes<D: ReadAt>(dev: &mut D, v: &Volume, dir: Dir) -> Result<Vec<u8>, Refusal> {
    match dir {
        Dir::Root if !v.fat32 => {
            let mut buf = vec![0u8; (v.root_entries * 32) as usize];
            dev.read_at(v.root_start, &mut buf)?;
            Ok(buf)
        }
        Dir::Root => {
            let chain = cluster_chain(dev, v, u64::from(v.root_cluster), v.clusters)?;
            let mut out = Vec::new();
            read_clusters(dev, v, &chain, &mut out)?;
            Ok(out)
        }
        Dir::Cluster(first) => {
            let chain = cluster_chain(dev, v, first, v.clusters)?;
            let mut out = Vec::new();
            read_clusters(dev, v, &chain, &mut out)?;
            Ok(out)
        }
    }
}

/// The 8-bit checksum of an 8.3 name, which a long name's entries carry (FAT specification).
fn short_checksum(name: &[u8]) -> u8 {
    name.iter().fold(0u8, |sum, &b| {
        (sum >> 1)
            .wrapping_add(if sum & 1 != 0 { 0x80 } else { 0 })
            .wrapping_add(b)
    })
}

/// The 8.3 name as text: `NAME.EXT`, spaces dropped.
fn short_name(e: &[u8]) -> String {
    let base = String::from_utf8_lossy(&e[0..8]).trim_end().to_string();
    let ext = String::from_utf8_lossy(&e[8..11]).trim_end().to_string();
    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

/// The long name the entries before a short entry carry, when they are one run in order (1, 2,
/// ..., last flagged) with the short entry's checksum.
fn long_name(parts: &[(u8, [u16; 13])]) -> Option<String> {
    let mut sorted: Vec<&(u8, [u16; 13])> = parts.iter().collect();
    sorted.sort_by_key(|(seq, _)| *seq);
    let mut units: Vec<u16> = Vec::new();
    for (i, (seq, chars)) in sorted.iter().enumerate() {
        if usize::from(*seq) != i + 1 {
            return None;
        }
        for &u in chars {
            if u == 0x0000 || u == 0xFFFF {
                break;
            }
            units.push(u);
        }
    }
    Some(String::from_utf16_lossy(&units))
}

/// The first directory entry in `dir` whose long or short name is `want` (ASCII case-insensitive).
fn find_entry(dir: &[u8], want: &str) -> Option<Hit> {
    let mut parts: Vec<(u8, [u16; 13])> = Vec::new();
    let mut sum: Option<u8> = None;
    for e in dir.as_chunks::<32>().0 {
        if e[0] == 0x00 {
            return None;
        }
        if e[0] == 0xE5 {
            parts.clear();
            sum = None;
            continue;
        }
        if e[11] == ATTR_LFN {
            if e[0] & 0x40 != 0 {
                parts.clear();
                sum = Some(e[13]);
            }
            let seq = e[0] & 0x1F;
            if seq == 0 || sum != Some(e[13]) {
                parts.clear();
                sum = None;
                continue;
            }
            let mut chars = [0xFFFFu16; 13];
            for (k, &at) in LFN_AT.iter().enumerate() {
                chars[k] = u16::from_le_bytes([e[at], e[at + 1]]);
            }
            parts.push((seq, chars));
            continue;
        }
        let run = std::mem::take(&mut parts);
        let run_sum = sum.take();
        if e[11] & ATTR_VOLUME != 0 {
            continue;
        }
        let long = match run_sum {
            Some(s) if s == short_checksum(&e[0..11]) && !run.is_empty() => long_name(&run),
            _ => None,
        };
        let matched = long.is_some_and(|l| l.eq_ignore_ascii_case(want))
            || short_name(e).eq_ignore_ascii_case(want);
        if matched {
            let hi = u64::from(u16::from_le_bytes([e[20], e[21]]));
            let lo = u64::from(u16::from_le_bytes([e[26], e[27]]));
            return Some(Hit {
                attr: e[11],
                first: (hi << 16) | lo,
                size: u32::from_le_bytes([e[28], e[29], e[30], e[31]]),
            });
        }
    }
    None
}

/// Reads the file at `path` (`/`-separated, relative to the root; a component is a long or short
/// name, ASCII case-insensitive) without mounting, writing nothing. The file must be at most
/// `limit` bytes. Its chain is followed, so a fragmented file reads; a chain that loops, leaves
/// the volume, holds a free or bad cluster, or disagrees with the file's size is refused.
///
/// # Errors
///
/// [`Refusal::Missing`] when a component is not there, [`Refusal::Corrupt`] for what the FAT
/// gets wrong, [`Refusal::TooLarge`] past `limit`, and the device's own failure.
pub fn read_file<D: ReadAt>(dev: &mut D, path: &str, limit: u64) -> Result<Vec<u8>, Refusal> {
    let v = volume(dev)?;
    let parts: Vec<&str> = path.split('/').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(Refusal::Corrupt("a path has an empty component"));
    }
    let mut dir = Dir::Root;
    for (i, part) in parts.iter().enumerate() {
        let bytes = dir_bytes(dev, &v, dir)?;
        let hit = find_entry(&bytes, part).ok_or(Refusal::Missing)?;
        if i + 1 < parts.len() {
            if hit.attr & ATTR_DIR == 0 {
                return Err(Refusal::Corrupt("a path component is not a directory"));
            }
            if hit.first < 2 {
                return Err(Refusal::Corrupt("a directory entry names no cluster"));
            }
            dir = Dir::Cluster(hit.first);
            continue;
        }
        if hit.attr & ATTR_DIR != 0 {
            return Err(Refusal::Corrupt("the path names a directory, not a file"));
        }
        let size = u64::from(hit.size);
        if size > limit {
            return Err(Refusal::TooLarge { found: size, limit });
        }
        let cluster_bytes = v.spc * 512;
        let needed = size.div_ceil(cluster_bytes);
        if needed == 0 {
            return Ok(Vec::new());
        }
        if hit.first < 2 {
            return Err(Refusal::Corrupt("a file entry names no cluster"));
        }
        let chain = cluster_chain(dev, &v, hit.first, needed)?;
        if chain.len() as u64 != needed {
            return Err(Refusal::Corrupt("a file's chain is shorter than its size"));
        }
        let mut out = Vec::with_capacity((needed * cluster_bytes) as usize);
        read_clusters(dev, &v, &chain, &mut out)?;
        out.truncate(size as usize);
        return Ok(out);
    }
    Err(Refusal::Missing)
}

#[cfg(test)]
#[path = "bootfat_tests.rs"]
pub(crate) mod tests;
