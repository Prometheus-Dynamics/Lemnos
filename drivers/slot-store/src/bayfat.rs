//! The bay writer of layout version 6: a bay set's files at their exact sizes, each in its own
//! clusters inside the set's region, and the filesystem records that name them, rewritten by the
//! install and by nothing else.
//!
//! A set owns, for the life of the image: its region (contiguous clusters, the first a multiple of
//! [`FAT16_PER_SECTOR`] and the count too, so the FAT sectors that cover it hold only its entries),
//! its directories' clusters (`slots/bayN/` and its subdirectories), and its record. Nothing else
//! of the volume is ever written by an install: not another set, not the root, not a FAT sector
//! that holds another file's entry.
//!
//! [`write_set`] is steps b to e of an install (the record steps a and f are the caller's):
//! b. each member's bytes, contiguous from the region's first cluster in member order, the rest of
//!    each member's last cluster zero;
//! c. the FAT sectors that cover the region, the second copy first and then the first: each member
//!    a chain of its own, then `reserve`, which holds the region's remaining clusters;
//! d. the directories: the subdirectories, then the set's directory, whose sector holding the
//!    `reserve` entry is written last. The entries every set keeps (`.`, `..`, the record and the
//!    subdirectories) are copied as they are; each member's entry is written as Linux vfat writes
//!    it (an 8.3 name with its lowercase flags, or a long name);
//! e. every member read back through the FAT, parsed again from the volume, and compared with what
//!    was written; `reserve` read back at its size.

use crate::bootfat::{self, ReadAt, Volume};
use crate::fatimage;
use crate::twocopy::{SECTOR, Sectors};

/// FAT16 entries in one FAT sector: a region aligned to this owns whole FAT sectors.
pub const FAT16_PER_SECTOR: u64 = (SECTOR / 2) as u64;

/// The file that owns a set's free clusters: whatever of the region the members do not take.
pub const RESERVE: &str = "reserve";

/// A set's region: its first cluster and its cluster count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// The first cluster (a multiple of [`FAT16_PER_SECTOR`]).
    pub start: u64,
    /// The clusters (a multiple of [`FAT16_PER_SECTOR`]).
    pub clusters: u64,
}

impl Region {
    /// Whether the region is aligned as a set's region must be, and lies on the volume.
    pub(crate) fn check(&self, v: &Volume) -> Result<(), String> {
        if v.fat32 {
            return Err("the boot partition is FAT32: bays are made on FAT16 only".to_string());
        }
        if self.start == 0
            || !self.start.is_multiple_of(FAT16_PER_SECTOR)
            || self.clusters == 0
            || !self.clusters.is_multiple_of(FAT16_PER_SECTOR)
        {
            return Err(format!(
                "the bay's region (cluster {}, {} clusters) is not aligned to whole FAT sectors",
                self.start, self.clusters
            ));
        }
        if self.start + self.clusters > v.clusters + 2 {
            return Err(format!(
                "the bay's region (cluster {}, {} clusters) runs past the volume's {} clusters",
                self.start, self.clusters, v.clusters
            ));
        }
        Ok(())
    }
}

/// A file of a set: its path under the set's directory and its bytes.
#[derive(Debug, Clone, Copy)]
pub struct Member<'a> {
    /// `kernel8.img`, `overlays/README`, ...
    pub path: &'a str,
    /// The bytes, stored exactly.
    pub bytes: &'a [u8],
}

/// Where each member lands: its first cluster (0 for an empty file) and cluster count, then the
/// reserve's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) members: Vec<(u64, u64)>,
    pub(crate) reserve: (u64, u64),
}

/// The clusters `len` bytes take.
pub(crate) fn clusters_for(len: u64, cluster_bytes: u64) -> u64 {
    len.div_ceil(cluster_bytes)
}

/// Places `members` in `region`, in order, refused by name when they do not fit.
pub(crate) fn plan(v: &Volume, region: Region, members: &[Member<'_>]) -> Result<Plan, String> {
    let cb = v.spc * SECTOR as u64;
    let mut next = region.start;
    let mut out = Vec::with_capacity(members.len());
    for m in members {
        let n = clusters_for(m.bytes.len() as u64, cb);
        out.push((if n == 0 { 0 } else { next }, n));
        next += n;
    }
    let used = next - region.start;
    if used > region.clusters {
        return Err(format!(
            "the boot files take {used} clusters of {cb} bytes and the bay's region holds {}: \
             nothing was written; rebuild the image with larger bays or bring smaller files",
            region.clusters
        ));
    }
    let left = region.clusters - used;
    Ok(Plan {
        members: out,
        reserve: (if left == 0 { 0 } else { next }, left),
    })
}

/// The directory of a member path (`""` for the set's own) and its file name.
fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// The set's subdirectories the members name, in the order they first appear.
pub(crate) fn subdirs<'a>(members: &[Member<'a>]) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for m in members {
        let (d, _) = split(m.path);
        if !d.is_empty() && !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

fn io(e: std::io::Error) -> String {
    e.to_string()
}

/// The FAT bytes of the region under `plan`: each member's chain and the reserve's.
fn fat_bytes(region: Region, plan: &Plan) -> Vec<u8> {
    let mut fat = vec![0u8; (region.clusters * 2) as usize];
    let mut chain = |first: u64, n: u64| {
        for k in 0..n {
            let c = first + k;
            let value: u16 = if k + 1 == n { 0xFFFF } else { (c + 1) as u16 };
            let at = ((c - region.start) * 2) as usize;
            fat[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
    };
    for &(first, n) in &plan.members {
        chain(first, n);
    }
    chain(plan.reserve.0, plan.reserve.1);
    fat
}

/// The entries one directory keeps through every install: the leading run of `.`, `..`, the
/// set's record (`keep_name`, by its 8.3 name) and the subdirectory entries, which the builder and
/// every install put first, alone in their sectors. The run ends at the first entry of any other
/// kind (a padding entry, a file, a long name), so nothing past it (the sectors an install
/// rewrites, which a cut may have torn) is ever read as an entry to keep.
/// A directory composed before any write: its cluster chain, its old bytes, its new bytes, and the
/// index of its `reserve` entry when it has one (see [`compose`]).
type Composed = (Vec<u64>, Vec<u8>, Vec<u8>, Option<usize>);

fn kept_entries(dir: &[u8], keep_name: Option<&str>) -> Vec<[u8; 32]> {
    let record = keep_name.map(fatimage::short_name);
    let mut out: Vec<[u8; 32]> = Vec::new();
    for e in dir.as_chunks::<32>().0 {
        let attr = e[11];
        let dot = e[0] == b'.';
        let sub = attr & 0x10 != 0 && attr != 0x0F && e[0] != 0 && e[0] != 0xE5;
        let named = record.is_some_and(|r| e[..11] == r[..] && attr & 0x18 == 0);
        if !(dot || sub || named) {
            break;
        }
        out.push(*e);
    }
    out
}

/// The entries per sector.
const PER_SECTOR: usize = SECTOR / 32;

/// A deleted entry: what fills the kept entries' last sector, so the files' entries start on a
/// sector of their own.
const PADDING: [u8; 32] = {
    let mut e = [0u8; 32];
    e[0] = 0xE5;
    e
};

/// The entries of one file named `name`: its long-name entries when it is not an 8.3 name, then its
/// 8.3 entry, whose short name is unique among `taken` (`BASE~N`, the first free `N`).
fn file_entries(name: &str, first: u64, size: u32, taken: &mut Vec<[u8; 11]>) -> Vec<[u8; 32]> {
    let mut out = Vec::new();
    let sfn = if fatimage::is_83(name) {
        fatimage::short_name(name)
    } else {
        let builder = fatimage::short_name(name);
        let base: Vec<u8> = builder[..8]
            .iter()
            .copied()
            .take_while(|&b| b != b'~' && b != b' ')
            .collect();
        let mut n = 1u32;
        let sfn = loop {
            let tail = format!("~{n}");
            let keep = base.len().min(8 - tail.len());
            let mut s = [b' '; 11];
            s[..keep].copy_from_slice(&base[..keep]);
            s[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
            s[8..].copy_from_slice(&builder[8..]);
            if !taken.contains(&s) {
                break s;
            }
            n += 1;
        };
        out.extend(fatimage::long_entries(name, &sfn));
        sfn
    };
    taken.push(sfn);
    out.push(fatimage::named_entry(name, &sfn, 0x20, first as u32, size));
    out
}

/// One directory's new bytes: the kept entries, then each file's, then (the set's own directory)
/// the reserve's. Also the index of the reserve's entry. Refused when they do not fit its clusters.
fn compose(
    path: &str,
    old: &[u8],
    keep_name: Option<&str>,
    files: &[(&str, u64, u32)],
    reserve: Option<(u64, u64)>,
    cluster_bytes: u64,
) -> Result<(Vec<u8>, Option<usize>), String> {
    let mut entries = kept_entries(old, keep_name);
    let mut taken: Vec<[u8; 11]> = entries
        .iter()
        .map(|e| {
            let mut s = [0u8; 11];
            s.copy_from_slice(&e[..11]);
            s
        })
        .collect();
    // The kept entries' sectors hold nothing else: an install never writes them.
    while !entries.len().is_multiple_of(PER_SECTOR) {
        entries.push(PADDING);
    }
    for &(name, first, size) in files {
        entries.extend(file_entries(name, first, size, &mut taken));
    }
    let mut at = None;
    if let Some((first, n)) = reserve {
        at = Some(entries.len());
        entries.extend(file_entries(
            RESERVE,
            first,
            (n * cluster_bytes) as u32,
            &mut taken,
        ));
    }
    if entries.len() * 32 > old.len() {
        return Err(format!(
            "{path}: {} directory entries do not fit its {} bytes: nothing was written",
            entries.len(),
            old.len()
        ));
    }
    let mut bytes = vec![0u8; old.len()];
    for (i, e) in entries.iter().enumerate() {
        bytes[i * 32..i * 32 + 32].copy_from_slice(e);
    }
    Ok((bytes, at))
}

/// Writes directory bytes over its chain, sector by sector, skipping each sector whose bytes do not
/// change from `old`; the sector holding entry `last` (when given) is written after every other.
fn write_dir<D: Sectors>(
    dev: &mut D,
    durability: Durability,
    v: &Volume,
    chain: &[u64],
    old: &[u8],
    bytes: &[u8],
    last: Option<usize>,
) -> Result<(), String> {
    let mut sectors: Vec<u64> = Vec::with_capacity(chain.len() * v.spc as usize);
    for &c in chain {
        let at = v.cluster_lba(c);
        sectors.extend(at..at + v.spc);
    }
    let last_sector = last.map(|i| i * 32 / SECTOR);
    let order = (0..sectors.len())
        .filter(|&s| Some(s) != last_sector)
        .chain(last_sector);
    for s in order {
        let new = &bytes[s * SECTOR..(s + 1) * SECTOR];
        if new == &old[s * SECTOR..(s + 1) * SECTOR] {
            continue;
        }
        let mut buf = [0u8; SECTOR];
        buf.copy_from_slice(new);
        put_sector(dev, durability, sectors[s], &buf).map_err(io)?;
    }
    Ok(())
}

/// Steps b to e of an install into the set at `set_dir` (`slots/bayN`): see the module text.
/// `record` is the set's record name, kept in its directory. Nothing outside the set is written.
/// `subdirs_of_set` are the set's subdirectories (each is laid out, whether or not a member is in it).
pub(crate) fn write_set<D: Sectors + ReadAt>(
    dev: &mut D,
    set_dir: &str,
    record: &str,
    region: Region,
    subdirs_of_set: &[&str],
    members: &[Member<'_>],
) -> Result<(), String> {
    write_set_with(
        dev,
        set_dir,
        record,
        region,
        subdirs_of_set,
        members,
        Durability::Synced,
    )
}

/// How a set's sectors are written: each one durable as it is written, or left for the caller to
/// [`Sectors::flush`] before anything that names them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Durability {
    Synced,
    Unsynced,
}

/// Writes one sector as `durability` says.
fn put_sector<D: Sectors>(
    dev: &mut D,
    durability: Durability,
    lba: u64,
    buf: &[u8; SECTOR],
) -> std::io::Result<()> {
    match durability {
        Durability::Synced => dev.write(lba, buf),
        Durability::Unsynced => dev.write_unsynced(lba, buf),
    }
}

/// [`write_set`], with the sectors' durability chosen by the caller. The install path writes its
/// bulk unsynced and flushes once before its commit record; every other caller keeps `Synced`.
pub(crate) fn write_set_with<D: Sectors + ReadAt>(
    dev: &mut D,
    set_dir: &str,
    record: &str,
    region: Region,
    subdirs_of_set: &[&str],
    members: &[Member<'_>],
    durability: Durability,
) -> Result<(), String> {
    let v = bootfat::volume(dev).map_err(|e| e.to_string())?;
    region.check(&v)?;
    let plan = plan(&v, region, members)?;
    let cb = v.spc * SECTOR as u64;
    // The directories first, read and composed before any write: one that cannot take its entries
    // is refused with nothing written.
    let mut dirs: Vec<Composed> = Vec::new();
    let mut subs: Vec<&str> = subdirs_of_set.to_vec();
    for d in subdirs(members) {
        if !subs.contains(&d) {
            subs.push(d);
        }
    }
    for (k, d) in subs.iter().chain(std::iter::once(&"")).enumerate() {
        let own = k == subs.len();
        let path = if own {
            set_dir.to_string()
        } else {
            format!("{set_dir}/{d}")
        };
        let (chain, old) = bootfat::dir_chain(dev, &path).map_err(|e| format!("{path}: {e}"))?;
        let files: Vec<(&str, u64, u32)> = members
            .iter()
            .zip(&plan.members)
            .filter(|(m, _)| split(m.path).0 == *d)
            .map(|(m, &(first, _))| (split(m.path).1, first, m.bytes.len() as u32))
            .collect();
        let (bytes, at) = compose(
            &path,
            &old,
            if own { Some(record) } else { None },
            &files,
            if own { Some(plan.reserve) } else { None },
            cb,
        )?;
        dirs.push((chain, old, bytes, at));
    }
    // b. The data.
    for (m, &(first, n)) in members.iter().zip(&plan.members) {
        let sectors = n * v.spc;
        for s in 0..sectors {
            let mut buf = [0u8; SECTOR];
            let from = (s as usize) * SECTOR;
            if from < m.bytes.len() {
                let to = (from + SECTOR).min(m.bytes.len());
                buf[..to - from].copy_from_slice(&m.bytes[from..to]);
            }
            put_sector(dev, durability, v.cluster_lba(first) + s, &buf).map_err(io)?;
        }
    }
    // c. The FAT sectors of the region, the last copy first.
    let fat = fat_bytes(region, &plan);
    let first_sector = region.start * 2 / SECTOR as u64;
    for copy in (0..v.fats).rev() {
        for (k, chunk) in fat.chunks(SECTOR).enumerate() {
            let mut buf = [0u8; SECTOR];
            buf.copy_from_slice(chunk);
            put_sector(
                dev,
                durability,
                v.fat_lba(copy) + first_sector + k as u64,
                &buf,
            )
            .map_err(io)?;
        }
    }
    // d. The directories: the subdirectories, then the set's own (reserve's sector last).
    for (chain, old, bytes, at) in &dirs {
        write_dir(dev, durability, &v, chain, old, bytes, *at)?;
    }
    // e. Read back through the FAT, parsed again.
    for m in members {
        let path = format!("{set_dir}/{}", m.path);
        let back = bootfat::read_file(dev, &path, m.bytes.len() as u64)
            .map_err(|e| format!("{path}: read back: {e}: nothing was committed"))?;
        if back != m.bytes {
            return Err(format!(
                "{path}: the read-back does not match what was written: nothing was committed"
            ));
        }
    }
    let path = format!("{set_dir}/{RESERVE}");
    let size = bootfat::size_of_path(dev, &path).map_err(|e| format!("{path}: {e}"))?;
    if size != plan.reserve.1 * cb {
        return Err(format!(
            "{path}: {size} bytes after the write, {} expected: nothing was committed",
            plan.reserve.1 * cb
        ));
    }
    Ok(())
}

/// The sectors a set owns (its write set): the region's data, the region's FAT sectors in every
/// copy, its directories' clusters and its record. An install writes nothing else.
pub fn owned_sectors<D: ReadAt>(
    dev: &mut D,
    set_dir: &str,
    record: &str,
    region: Region,
) -> Result<Vec<u64>, String> {
    let v = bootfat::volume(dev).map_err(|e| e.to_string())?;
    region.check(&v)?;
    let mut out: Vec<u64> = Vec::new();
    let first = v.cluster_lba(region.start);
    out.extend(first..first + region.clusters * v.spc);
    let fat_first = region.start * 2 / SECTOR as u64;
    let fat_n = region.clusters * 2 / SECTOR as u64;
    for copy in 0..v.fats {
        let at = v.fat_lba(copy) + fat_first;
        out.extend(at..at + fat_n);
    }
    let (chain, bytes) = bootfat::dir_chain(dev, set_dir).map_err(|e| format!("{set_dir}: {e}"))?;
    let mut dirs = vec![chain];
    let mut subs: Vec<String> = Vec::new();
    bootfat::each_entry(&bytes, |e| {
        if e.attr & 0x10 != 0 && e.raw[0] != b'.' {
            subs.push(e.long.clone().unwrap_or_else(|| e.short.clone()));
        }
        false
    });
    for s in subs {
        let path = format!("{set_dir}/{s}");
        dirs.push(
            bootfat::dir_chain(dev, &path)
                .map_err(|e| format!("{path}: {e}"))?
                .0,
        );
    }
    for chain in dirs {
        for c in chain {
            let at = v.cluster_lba(c);
            out.extend(at..at + v.spc);
        }
    }
    let path = format!("{set_dir}/{record}");
    let found =
        bootfat::locate_path(dev, &path, 2 * SECTOR as u64).map_err(|e| format!("{path}: {e}"))?;
    out.extend(found.first_lba..found.first_lba + 2);
    out.sort_unstable();
    out.dedup();
    Ok(out)
}
