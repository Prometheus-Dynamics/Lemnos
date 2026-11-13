//! Finds a pre-created record file's sector on a FAT16 or FAT32 partition, reading the volume
//! only: no mount, no write. A record file must be one contiguous run of clusters whose size is
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

#[cfg(test)]
#[path = "bootfat_tests.rs"]
pub(crate) mod tests;
