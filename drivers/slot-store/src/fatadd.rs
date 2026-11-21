//! Adds files to an existing FAT16 volume held in memory (a dump of a partition), without moving or
//! changing anything the volume already holds. An offline tool's code: nothing here runs on a board.
//!
//! What it does, and the rules it keeps:
//!
//! - **FAT16 only**, 512-byte sectors. FAT12 and FAT32 are refused by name ([`Geometry::parse`]).
//! - **The free map** comes from the file allocation tables: a cluster is free only when its entry is
//!   0 in every FAT copy, and a volume whose copies differ is refused ([`survey`]).
//! - **One contiguous run per file.** [`first_fit`] is the rule: the lowest-numbered run of free
//!   clusters that is long enough, files taken in the order they are added. A file is never split. A
//!   file with no run long enough is refused by name with the longest free run. Clusters freed by a
//!   replacement in the same edit are never handed out again in it, so the bytes they held stay
//!   where they were.
//! - **Directory entries** are appended after a directory's last entry, with the names the image
//!   builder writes ([`crate::fatimage`]): an 8.3 name as it is, any other name as long-name entries
//!   and a `BASE~N` short name that no entry of the directory already has. A full directory is
//!   refused; the FAT16 root is fixed in size.
//! - **The audit** ([`Editor::audit`]) is the last step and the guard: every sector that differs from
//!   the original is classified, and the edit is refused when one of them is a sector of a cluster
//!   that was allocated before (other than a directory that gained entries), when an existing FAT
//!   entry or directory entry changed, or when the boot sector changed. It works from the difference
//!   of the two images, so no write path can be left out of it.
//!
//! The same edit gives the list of changed sector ranges ([`Change`]) and the undo file
//! ([`undo_file`], [`apply_undo`]), which holds the bytes each range had before.

use crate::bootfat::{self, ATTR_DIR, Listed};
use crate::fatimage::{self, Node};
use sha2::{Digest, Sha256};

/// One sector.
const SECTOR: usize = 512;

/// The fields of a FAT16 volume this module needs, all in sectors unless named otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// Sectors per cluster.
    pub spc: u64,
    /// Reserved sectors before the first FAT.
    pub reserved: u64,
    /// The number of FAT copies.
    pub fats: u64,
    /// Sectors of one FAT copy.
    pub fat_sectors: u64,
    /// Entries of the fixed root directory.
    pub root_entries: u64,
    /// The root directory's first sector.
    pub root_start: u64,
    /// The first sector of the data area (cluster 2).
    pub data_start: u64,
    /// The number of data clusters. Their numbers run from 2 to `clusters + 1`.
    pub clusters: u64,
    /// The volume's sectors.
    pub total: u64,
}

fn le16(b: &[u8], at: usize) -> u64 {
    u64::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

fn le32(b: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]))
}

impl Geometry {
    /// Reads the boot sector of `image`. Refused by name: a file that is not whole sectors or has no
    /// boot sector signature, any sector size but 512, FAT12, FAT32, a FAT too short for its
    /// clusters, and a file shorter than the volume it describes.
    pub fn parse(image: &[u8]) -> Result<Self, String> {
        let not_fat = |why: &str| format!("not a FAT16 volume: {why}: nothing was written");
        if image.len() < SECTOR || !image.len().is_multiple_of(SECTOR) {
            return Err(not_fat(&format!(
                "{} bytes is not a whole number of 512-byte sectors",
                image.len()
            )));
        }
        if image[510..512] != [0x55, 0xAA] {
            return Err(not_fat("the first sector has no boot signature (55 AA)"));
        }
        if le16(image, 11) != 512 {
            return Err(not_fat("the sector size is not 512"));
        }
        let spc = u64::from(image[13]);
        if spc == 0 || !spc.is_power_of_two() {
            return Err(not_fat("sectors per cluster is not a power of two"));
        }
        let reserved = le16(image, 14);
        let fats = u64::from(image[16]);
        let root_entries = le16(image, 17);
        let fat_sectors = le16(image, 22);
        if reserved == 0 || fats == 0 {
            return Err(not_fat("no reserved sector or no FAT copy"));
        }
        let total = match le16(image, 19) {
            0 => le32(image, 32),
            n => n,
        };
        if fat_sectors == 0 || root_entries == 0 {
            return Err(
                "a FAT32 volume (no 16-bit FAT size or no fixed root directory): --add-to writes FAT16 only: nothing was written"
                    .to_string(),
            );
        }
        if !(root_entries * 32).is_multiple_of(SECTOR as u64) {
            return Err(not_fat("the root directory is not whole sectors"));
        }
        let root_sectors = root_entries * 32 / SECTOR as u64;
        let root_start = reserved + fats * fat_sectors;
        let data_start = root_start + root_sectors;
        if total <= data_start {
            return Err(not_fat("no data area"));
        }
        let clusters = (total - data_start) / spc;
        if clusters < 4085 {
            return Err(format!(
                "a FAT12 volume ({clusters} clusters): --add-to writes FAT16 only: nothing was written"
            ));
        }
        if clusters >= 65525 {
            return Err(format!(
                "a FAT32 volume ({clusters} clusters): --add-to writes FAT16 only: nothing was written"
            ));
        }
        if fat_sectors * (SECTOR as u64) < (clusters + 2) * 2 {
            return Err(not_fat("the FAT is too short for the volume's clusters"));
        }
        if (image.len() / SECTOR) as u64 != total {
            return Err(format!(
                "the file is {} sectors and its boot sector says the volume is {total}: not a dump of the whole partition: nothing was written",
                image.len() / SECTOR
            ));
        }
        Ok(Self {
            spc,
            reserved,
            fats,
            fat_sectors,
            root_entries,
            root_start,
            data_start,
            clusters,
            total,
        })
    }

    /// The byte offset of cluster `c`'s entry in FAT copy `copy`.
    fn fat_at(&self, copy: u64, c: u64) -> usize {
        ((self.reserved + copy * self.fat_sectors) * SECTOR as u64 + c * 2) as usize
    }

    /// Cluster `c`'s entry in FAT copy `copy`.
    pub fn entry(&self, image: &[u8], copy: u64, c: u64) -> u16 {
        let at = self.fat_at(copy, c);
        u16::from_le_bytes([image[at], image[at + 1]])
    }

    /// The first sector of cluster `c`.
    pub fn cluster_lba(&self, c: u64) -> u64 {
        self.data_start + (c - 2) * self.spc
    }

    /// The bytes of one cluster.
    pub fn cluster_bytes(&self) -> usize {
        self.spc as usize * SECTOR
    }

    /// Whether `c` is a data cluster's number.
    fn in_range(&self, c: u64) -> bool {
        c >= 2 && c < self.clusters + 2
    }
}

/// A file or directory the volume already holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Existing {
    /// Its path from the root, `/`-separated, by long name where it has one.
    pub path: String,
    /// Whether it is a directory.
    pub is_dir: bool,
    /// The first cluster its entry names (0 for an empty file).
    pub first: u64,
    /// The size its entry gives (0 for a directory).
    pub size: u32,
    /// Its clusters, in chain order.
    pub chain: Vec<u64>,
    /// The byte offset in the image of its 32-byte short entry.
    pub at: usize,
}

/// The free clusters of a volume: how many, in how many runs, and the longest run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreeMap {
    /// Free clusters.
    pub clusters: u64,
    /// Runs of consecutive free clusters.
    pub runs: u64,
    /// The longest run, in clusters.
    pub longest: u64,
}

/// What a volume holds, read once before anything is changed.
#[derive(Debug, Clone)]
pub struct Survey {
    /// The geometry.
    pub geo: Geometry,
    /// Every file and directory, in directory order, parents before their children.
    pub entries: Vec<Existing>,
    /// For each cluster number, the index in `entries` of what owns it, plus one; 0 for none.
    owner: Vec<u32>,
    /// For each cluster number, whether it is free: its entry is 0 in every FAT copy.
    pub free: Vec<bool>,
}

impl Survey {
    /// The free map's figures.
    pub fn free_map(&self) -> FreeMap {
        free_map(&self.free)
    }

    /// The path of what owns cluster `c`, when a directory entry's chain holds it.
    pub fn owner_of(&self, c: u64) -> Option<&Existing> {
        match self.owner.get(c as usize) {
            Some(&n) if n > 0 => self.entries.get(n as usize - 1),
            _ => None,
        }
    }

    /// The entry at `path` (ASCII case-insensitive).
    pub fn find(&self, path: &str) -> Option<&Existing> {
        self.entries
            .iter()
            .find(|e| e.path.eq_ignore_ascii_case(path))
    }
}

/// The figures of a free map (`free[c]` for cluster `c`; entries 0 and 1 are never free).
pub fn free_map(free: &[bool]) -> FreeMap {
    let mut out = FreeMap {
        clusters: 0,
        runs: 0,
        longest: 0,
    };
    let mut run = 0u64;
    for &f in free.iter().skip(2) {
        if f {
            if run == 0 {
                out.runs += 1;
            }
            run += 1;
            out.clusters += 1;
            out.longest = out.longest.max(run);
        } else {
            run = 0;
        }
    }
    out
}

/// The allocation rule: the first cluster of the lowest-numbered run of at least `need` free
/// clusters, `None` when the map has no such run. `free[c]` is cluster `c`.
pub fn first_fit(free: &[bool], need: u64) -> Option<u64> {
    let mut run = 0u64;
    for (c, &f) in free.iter().enumerate().skip(2) {
        if f {
            run += 1;
            if run == need {
                return Some(c as u64 + 1 - need);
            }
        } else {
            run = 0;
        }
    }
    None
}

/// An allocator: the free map and the clusters needed, to the first cluster of the run it gives.
/// [`first_fit`] is the one the tool uses; a test passes a broken one to show the audit refuses it.
pub type Alloc<'a> = &'a dyn Fn(&[bool], u64) -> Option<u64>;

/// Reads the whole volume: its geometry, its FAT copies (refused when they differ), and every
/// directory entry with its cluster chain. A chain that leaves the volume, holds a free or bad
/// cluster, loops, or shares a cluster with another entry is refused, and so is a file whose chain
/// is not the length its size needs and a directory with bytes after its end marker: the volume
/// must be sound before anything is added to it.
pub fn survey(image: &[u8]) -> Result<Survey, String> {
    let geo = Geometry::parse(image)?;
    let fat_bytes = geo.fat_sectors as usize * SECTOR;
    let first = geo.fat_at(0, 0);
    for copy in 1..geo.fats {
        let other = geo.fat_at(copy, 0);
        if let Some(i) = (0..fat_bytes).find(|&i| image[first + i] != image[other + i]) {
            return Err(format!(
                "the FAT copies differ (copy 1 and copy {} at the entry of cluster {}): the volume is not sound: nothing was written",
                copy + 1,
                i / 2
            ));
        }
    }
    let count = geo.clusters as usize + 2;
    let free: Vec<bool> = (0..count)
        .map(|c| c >= 2 && geo.entry(image, 0, c as u64) == 0)
        .collect();
    let mut sv = Survey {
        geo,
        entries: Vec::new(),
        owner: vec![0; count],
        free,
    };
    let root: Vec<usize> = (0..geo.root_entries as usize)
        .map(|i| geo.root_start as usize * SECTOR + i * 32)
        .collect();
    walk(image, &mut sv, &root, "")?;
    Ok(sv)
}

/// The chain from `first`, each cluster claimed for entry number `who` (index plus one).
fn claim(
    image: &[u8],
    sv: &mut Survey,
    first: u64,
    who: u32,
    path: &str,
) -> Result<Vec<u64>, String> {
    let mut chain = Vec::new();
    let mut c = first;
    loop {
        if !sv.geo.in_range(c) {
            return Err(format!(
                "{path}: its chain names cluster {c}, which is outside the volume: the volume is not sound: nothing was written"
            ));
        }
        let held = sv.owner[c as usize];
        if held != 0 {
            let other = &sv.entries[held as usize - 1].path;
            return Err(format!(
                "cluster {c} is in the chains of both {other} and {path}: the volume is not sound: nothing was written"
            ));
        }
        let next = sv.geo.entry(image, 0, c);
        if next == 0 || next == 0xFFF7 || next == 1 {
            return Err(format!(
                "{path}: its chain holds cluster {c}, which the FAT marks free or bad: the volume is not sound: nothing was written"
            ));
        }
        sv.owner[c as usize] = who;
        chain.push(c);
        if next >= 0xFFF8 {
            return Ok(chain);
        }
        c = u64::from(next);
    }
}

/// The byte offsets of a directory's 32-byte entries, for the clusters of `chain`.
fn slots_of(geo: &Geometry, chain: &[u64]) -> Vec<usize> {
    let per = geo.cluster_bytes() / 32;
    chain
        .iter()
        .flat_map(|&c| {
            let base = geo.cluster_lba(c) as usize * SECTOR;
            (0..per).map(move |i| base + i * 32)
        })
        .collect()
}

/// A directory's bytes, entry by entry.
fn dir_bytes(image: &[u8], slots: &[usize]) -> Vec<u8> {
    let mut out = Vec::with_capacity(slots.len() * 32);
    for &at in slots {
        out.extend_from_slice(&image[at..at + 32]);
    }
    out
}

/// The live entries of a directory, as the reader lists them.
fn listed(bytes: &[u8]) -> Vec<Listed> {
    let mut out = Vec::new();
    bootfat::each_entry(bytes, |e| {
        out.push(e.clone());
        false
    });
    out
}

fn walk(image: &[u8], sv: &mut Survey, slots: &[usize], path: &str) -> Result<(), String> {
    let bytes = dir_bytes(image, slots);
    let shown = if path.is_empty() {
        "the root directory"
    } else {
        path
    };
    // Everything after the end marker must be empty: new entries go there.
    if let Some(end) = bytes.as_chunks::<32>().0.iter().position(|e| e[0] == 0)
        && bytes[end * 32..].iter().any(|&b| b != 0)
    {
        return Err(format!(
            "{shown} has bytes after its end marker: the volume is not sound: nothing was written"
        ));
    }
    let cluster_bytes = sv.geo.cluster_bytes() as u64;
    for e in listed(&bytes) {
        if e.short == "." || e.short == ".." {
            continue;
        }
        let name = e.long.clone().unwrap_or_else(|| e.short.clone());
        let full = if path.is_empty() {
            name
        } else {
            format!("{path}/{name}")
        };
        let is_dir = e.attr & ATTR_DIR != 0;
        let who = sv.entries.len() as u32 + 1;
        sv.entries.push(Existing {
            path: full.clone(),
            is_dir,
            first: e.first,
            size: e.size,
            chain: Vec::new(),
            at: slots[e.index],
        });
        let chain = if e.first == 0 && !is_dir {
            Vec::new()
        } else {
            claim(image, sv, e.first, who, &full)?
        };
        if !is_dir && chain.len() as u64 != u64::from(e.size).div_ceil(cluster_bytes) && e.size != 0
        {
            return Err(format!(
                "{full}: its chain is {} clusters and its size of {} bytes needs {}: the volume is not sound: nothing was written",
                chain.len(),
                e.size,
                u64::from(e.size).div_ceil(cluster_bytes)
            ));
        }
        sv.entries[who as usize - 1].chain = chain.clone();
        if is_dir {
            let sub = slots_of(&sv.geo, &chain);
            walk(image, sv, &sub, &full)?;
        }
    }
    Ok(())
}

/// What a changed sector is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// A sector of FAT copy `n` (1 is the first).
    Fat(u8),
    /// A sector of the fixed root directory.
    RootDir,
    /// A sector of a directory that existed before and gained entries.
    Dir,
    /// A sector of a new directory's cluster, free before.
    NewDir,
    /// A sector of a new file's cluster, free before.
    NewFile,
    /// A sector of a file the caller named as its own to rewrite in place (a bay file placed by an
    /// earlier add).
    OwnFile,
}

impl std::fmt::Display for Class {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Class::Fat(n) => write!(f, "fat{n}"),
            Class::RootDir => write!(f, "root-dir"),
            Class::Dir => write!(f, "dir"),
            Class::NewDir => write!(f, "new-dir"),
            Class::NewFile => write!(f, "new-file"),
            Class::OwnFile => write!(f, "own-file"),
        }
    }
}

/// One range of changed sectors, all of one class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The first sector, relative to the partition.
    pub lba: u64,
    /// The number of sectors.
    pub count: u64,
    /// What the sectors are.
    pub class: Class,
    /// SHA-256 of the range's bytes before.
    pub old: [u8; 32],
    /// SHA-256 of the range's bytes after.
    pub new: [u8; 32],
}

/// A file or directory an edit added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    /// Its path from the root.
    pub path: String,
    /// Whether it is a directory.
    pub is_dir: bool,
    /// Its size in bytes (0 for a directory).
    pub size: u64,
    /// Its first cluster.
    pub first: u64,
    /// Its clusters, one run.
    pub clusters: u64,
}

/// A root file an edit replaced: what it was, so it can be put back, and where the new one is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replaced {
    /// Its name.
    pub name: String,
    /// The size it had.
    pub old_size: u64,
    /// SHA-256 of the bytes it had.
    pub old_sha256: [u8; 32],
    /// The first cluster it had (0 for an empty file).
    pub old_first: u64,
    /// The clusters it had, now free.
    pub old_clusters: u64,
    /// Its size now.
    pub size: u64,
    /// Its first cluster now.
    pub first: u64,
    /// Its clusters now, one run.
    pub clusters: u64,
}

/// A directory being added to: the fixed root, or a chain of clusters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirAt {
    /// The fixed root directory.
    Root,
    /// A directory's clusters, in chain order.
    Chain(Vec<u64>),
}

/// An edit of a volume: the original, the working copy, and what the edit has allocated.
pub struct Editor<'a> {
    orig: &'a [u8],
    /// The working copy. The bay install writes its sectors here too.
    pub work: Vec<u8>,
    /// What the original holds.
    pub survey: Survey,
    free: Vec<bool>,
    alloc: Alloc<'a>,
    new_file: Vec<bool>,
    new_dir: Vec<bool>,
    freed: Vec<bool>,
    own: Vec<bool>,
    rewritten: Vec<usize>,
    /// Every file and directory added, in the order they were added.
    pub added: Vec<Added>,
}

impl<'a> Editor<'a> {
    /// Starts an edit of `orig`, which is surveyed first.
    pub fn new(orig: &'a [u8], alloc: Alloc<'a>) -> Result<Self, String> {
        let survey = survey(orig)?;
        let count = survey.free.len();
        Ok(Self {
            orig,
            work: orig.to_vec(),
            free: survey.free.clone(),
            survey,
            alloc,
            new_file: vec![false; count],
            new_dir: vec![false; count],
            freed: vec![false; count],
            own: vec![false; count],
            rewritten: Vec::new(),
            added: Vec::new(),
        })
    }

    fn geo(&self) -> Geometry {
        self.survey.geo
    }

    fn slots(&self, dir: &DirAt) -> Vec<usize> {
        let geo = self.geo();
        match dir {
            DirAt::Root => (0..geo.root_entries as usize)
                .map(|i| geo.root_start as usize * SECTOR + i * 32)
                .collect(),
            DirAt::Chain(chain) => slots_of(&geo, chain),
        }
    }

    fn listing(&self, dir: &DirAt) -> Vec<Listed> {
        listed(&dir_bytes(&self.work, &self.slots(dir)))
    }

    /// The chain from `first` in the working copy.
    fn chain_of(&self, first: u64) -> Result<Vec<u64>, String> {
        let geo = self.geo();
        let mut out = Vec::new();
        let mut c = first;
        while geo.in_range(c) && out.len() as u64 <= geo.clusters {
            out.push(c);
            let next = geo.entry(&self.work, 0, c);
            if next >= 0xFFF8 {
                return Ok(out);
            }
            c = u64::from(next);
        }
        Err(format!(
            "the chain from cluster {first} does not end inside the volume: nothing was written"
        ))
    }

    fn set_fat(&mut self, c: u64, value: u16) {
        let geo = self.geo();
        for copy in 0..geo.fats {
            let at = geo.fat_at(copy, c);
            self.work[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
    }

    /// One run of `need` clusters for `path`, chained in every FAT copy and filled with `data` and
    /// then zeros. Refused, naming `path` and the longest free run, when the map has no such run.
    fn take_run(&mut self, path: &str, need: u64, data: &[u8], dir: bool) -> Result<u64, String> {
        let geo = self.geo();
        let Some(first) = (self.alloc)(&self.free, need) else {
            let map = free_map(&self.free);
            return Err(format!(
                "{path}: it needs one run of {need} free clusters ({} bytes) and the longest free run is {} clusters ({} free in {} runs): a file is never split: nothing was written",
                need * geo.cluster_bytes() as u64,
                map.longest,
                map.clusters,
                map.runs
            ));
        };
        self.take_at(path, first, need, data, dir)
    }

    /// Takes the clusters `first..first + need` (each must be free) for `path` and writes `data`
    /// into them; [`Editor::take_run`] after its allocation, and a region's aligned run.
    fn take_at(
        &mut self,
        path: &str,
        first: u64,
        need: u64,
        data: &[u8],
        dir: bool,
    ) -> Result<u64, String> {
        let geo = self.geo();
        if !geo.in_range(first) || !geo.in_range(first + need - 1) {
            return Err(format!(
                "{path}: the allocator gave clusters {first} to {}, outside the volume: nothing was written",
                first + need - 1
            ));
        }
        for k in 0..need {
            let c = first + k;
            self.set_fat(
                c,
                if k + 1 == need {
                    0xFFFF
                } else {
                    (c + 1) as u16
                },
            );
            self.free[c as usize] = false;
            if dir {
                self.new_dir[c as usize] = true;
            } else {
                self.new_file[c as usize] = true;
            }
        }
        let at = geo.cluster_lba(first) as usize * SECTOR;
        let len = need as usize * geo.cluster_bytes();
        self.work[at..at + len].fill(0);
        self.work[at..at + data.len()].copy_from_slice(data);
        Ok(first)
    }

    /// The directory entries for `name`: its long-name entries when it is not an 8.3 name, then its
    /// short entry. The short name of a long name is `BASE~N`, the first `N` no entry of the
    /// directory has.
    fn entries_for(
        &self,
        dir: &DirAt,
        name: &str,
        attr: u8,
        first: u64,
        size: u32,
    ) -> Vec<[u8; 32]> {
        let mut out = Vec::new();
        let sfn = if fatimage::is_83(name) {
            fatimage::short_name(name)
        } else {
            let taken: Vec<[u8; 11]> = self.listing(dir).iter().map(|e| e.raw).collect();
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
        out.push(fatimage::named_entry(name, &sfn, attr, first as u32, size));
        out
    }

    /// Appends `entries` after the directory's last entry. Refused when they do not fit.
    fn append(&mut self, dir: &DirAt, shown: &str, entries: &[[u8; 32]]) -> Result<(), String> {
        let slots = self.slots(dir);
        let used = slots
            .iter()
            .position(|&at| self.work[at] == 0)
            .unwrap_or(slots.len());
        if used + entries.len() > slots.len() {
            let what = match dir {
                DirAt::Root => "the root directory is full".to_string(),
                DirAt::Chain(_) => format!("the directory {shown} is full"),
            };
            return Err(format!(
                "{what}: it holds {} entries, {used} are used and {} more are needed: nothing was written",
                slots.len(),
                entries.len()
            ));
        }
        for (i, e) in entries.iter().enumerate() {
            let at = slots[used + i];
            self.work[at..at + 32].copy_from_slice(e);
        }
        Ok(())
    }

    /// Adds the file `name` with `data` to `dir` (`dir_path` is the directory's path, empty for the
    /// root): one run of clusters, at least one, and its entries. A name the directory already has
    /// is refused.
    pub fn add_file(
        &mut self,
        dir: &DirAt,
        dir_path: &str,
        name: &str,
        data: &[u8],
    ) -> Result<(), String> {
        let path = join(dir_path, name);
        if self.listing(dir).iter().any(|e| e.is_named(name)) {
            return Err(format!(
                "{path} is already on the volume: nothing was written"
            ));
        }
        let need = (data.len().div_ceil(self.geo().cluster_bytes())).max(1) as u64;
        let first = self.take_run(&path, need, data, false)?;
        let entries = self.entries_for(dir, name, 0x20, first, data.len() as u32);
        self.append(dir, dir_path, &entries)?;
        self.added.push(Added {
            path,
            is_dir: false,
            size: data.len() as u64,
            first,
            clusters: need,
        });
        Ok(())
    }

    /// Adds a bay set's region file `name` to `dir`: one run of clusters that starts on a multiple of
    /// [`crate::bayfat::FAT16_PER_SECTOR`] and numbers a multiple of it ([`fatimage::region_clusters`]),
    /// so its FAT sectors hold only its entries. The first such free run is taken; when there is
    /// none the add is refused by name. Nothing is ever moved to make room.
    pub fn add_region(
        &mut self,
        dir: &DirAt,
        dir_path: &str,
        name: &str,
        caps: &[u64],
    ) -> Result<(), String> {
        let path = join(dir_path, name);
        let geo = self.geo();
        let spc = geo.cluster_bytes() as u64 / SECTOR as u64;
        let need = fatimage::region_clusters(caps, spc);
        let align = crate::bayfat::FAT16_PER_SECTOR;
        let free = &self.free;
        let fits = |c: u64| (c..c + need).all(|k| free.get(k as usize).copied().unwrap_or(false));
        let first = (1..)
            .map(|k| k * align)
            .take_while(|c| c + need <= free.len() as u64)
            .find(|&c| fits(c))
            .ok_or_else(|| {
                format!(
                    "{path}: the bay's region needs one run of {need} free clusters starting on a multiple of {align} (whole FAT sectors), and the volume has none: nothing is ever moved to make room: nothing was written"
                )
            })?;
        let zeros = vec![0u8; (need * geo.cluster_bytes() as u64) as usize];
        let taken = self.take_at(&path, first, need, &zeros, false)?;
        let entries = self.entries_for(dir, name, 0x20, taken, zeros.len() as u32);
        self.append(dir, dir_path, &entries)?;
        self.added.push(Added {
            path,
            is_dir: false,
            size: zeros.len() as u64,
            first: taken,
            clusters: need,
        });
        Ok(())
    }

    /// Adds `nodes` to `dir`, in order, the way the image builder fills a volume: a file gets its
    /// run, a directory that is not there yet is made (its `.` and `..` entries, room for its
    /// children) and one that is there is added to. `keep(path)` says whether a file is added;
    /// a directory none of whose files are kept is not made.
    pub fn add_nodes(
        &mut self,
        dir: &DirAt,
        dir_path: &str,
        nodes: &[Node],
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<(), String> {
        for node in nodes {
            match node {
                Node::File { name, data } => {
                    if keep(&join(dir_path, name)) {
                        self.add_file(dir, dir_path, name, data)?;
                    }
                }
                Node::Region { name, caps } => {
                    let path = join(dir_path, name);
                    if !keep(&path) {
                        continue;
                    }
                    self.add_region(dir, dir_path, name, caps)?;
                }
                Node::Dir { name, children } | Node::DirOf { name, children, .. } => {
                    let want = match node {
                        Node::DirOf { entries, .. } => *entries as u64,
                        _ => 0,
                    };
                    let path = join(dir_path, name);
                    if want == 0 && kept_entries(children, &path, keep) == 0 {
                        continue;
                    }
                    let found = self.listing(dir).into_iter().find(|e| e.is_named(name));
                    let sub = match found {
                        Some(e) if e.attr & ATTR_DIR != 0 => DirAt::Chain(self.chain_of(e.first)?),
                        Some(_) => {
                            return Err(format!(
                                "{path} is a file on the volume and a directory is needed there: nothing was written"
                            ));
                        }
                        None => {
                            let per = (self.geo().cluster_bytes() / 32) as u64;
                            let need = (2 + kept_entries(children, &path, keep))
                                .max(want)
                                .div_ceil(per)
                                .max(1);
                            let parent = match dir {
                                DirAt::Root => 0,
                                DirAt::Chain(chain) => chain[0],
                            };
                            let first = self.take_run(&path, need, &[], true)?;
                            let mut dot =
                                fatimage::short_entry(b".          ", 0x10, first as u32, 0);
                            dot[0] = b'.';
                            let mut dotdot =
                                fatimage::short_entry(b"..         ", 0x10, parent as u32, 0);
                            dotdot[0] = b'.';
                            dotdot[1] = b'.';
                            let at = self.geo().cluster_lba(first) as usize * SECTOR;
                            self.work[at..at + 32].copy_from_slice(&dot);
                            self.work[at + 32..at + 64].copy_from_slice(&dotdot);
                            let entries = self.entries_for(dir, name, 0x10, first, 0);
                            self.append(dir, dir_path, &entries)?;
                            self.added.push(Added {
                                path: path.clone(),
                                is_dir: true,
                                size: 0,
                                first,
                                clusters: need,
                            });
                            DirAt::Chain((first..first + need).collect())
                        }
                    };
                    self.add_nodes(&sub, &path, children, keep)?;
                }
            }
        }
        Ok(())
    }

    /// Replaces the root file `name` with `data`: a new run is allocated and written, the file's
    /// directory entry is rewritten in place (its first cluster and its size, nothing else), and
    /// the clusters it had are freed. They are freed only when no other entry's chain holds them,
    /// which the survey has already established for the whole volume and this checks again.
    pub fn replace_root_file(&mut self, name: &str, data: &[u8]) -> Result<Replaced, String> {
        let slots = self.slots(&DirAt::Root);
        let entry = self
            .listing(&DirAt::Root)
            .into_iter()
            .find(|e| e.is_named(name) && e.attr & ATTR_DIR == 0)
            .ok_or_else(|| {
                format!("{name} is not a file in the root directory: nothing was written")
            })?;
        let at = slots[entry.index];
        let old = self
            .survey
            .entries
            .iter()
            .position(|e| e.at == at)
            .ok_or_else(|| {
                format!("{name} was not on the volume before this edit: nothing was written")
            })?;
        let chain = self.survey.entries[old].chain.clone();
        for &c in &chain {
            if self.survey.owner[c as usize] != old as u32 + 1 {
                return Err(format!(
                    "{name}: cluster {c} of the file is shared with another entry: nothing was written"
                ));
            }
        }
        let cluster_bytes = self.geo().cluster_bytes();
        let mut bytes = Vec::with_capacity(chain.len() * cluster_bytes);
        for &c in &chain {
            let from = self.geo().cluster_lba(c) as usize * SECTOR;
            bytes.extend_from_slice(&self.orig[from..from + cluster_bytes]);
        }
        bytes.truncate(entry.size as usize);
        let need = data.len().div_ceil(cluster_bytes).max(1) as u64;
        let first = self.take_run(name, need, data, false)?;
        self.work[at + 20..at + 22].copy_from_slice(&[0, 0]);
        self.work[at + 26..at + 28].copy_from_slice(&(first as u16).to_le_bytes());
        self.work[at + 28..at + 32].copy_from_slice(&(data.len() as u32).to_le_bytes());
        self.rewritten.push(at);
        for &c in &chain {
            self.set_fat(c, 0);
            self.freed[c as usize] = true;
        }
        Ok(Replaced {
            name: name.to_string(),
            old_size: u64::from(entry.size),
            old_sha256: Sha256::digest(&bytes).into(),
            old_first: entry.first,
            old_clusters: chain.len() as u64,
            size: data.len() as u64,
            first,
            clusters: need,
        })
    }

    /// Names `bytes` from sector `lba` as the caller's own that this edit may rewrite in place, its
    /// FAT entries included: a bay set an earlier add placed (its region and its directories). The
    /// audit lets no other allocated cluster change.
    pub fn allow_own(&mut self, lba: u64, bytes: u64) {
        let geo = self.geo();
        if lba < geo.data_start {
            return;
        }
        let first = (lba - geo.data_start) / geo.spc + 2;
        let count = bytes.div_ceil(geo.cluster_bytes() as u64).max(1);
        for c in first..first + count {
            if geo.in_range(c) {
                self.own[c as usize] = true;
            }
        }
    }

    /// The guard, and the list of changes. Every sector of the working copy that differs from the
    /// original is classified. Refused, with nothing to write:
    ///
    /// - a changed sector of a cluster that was allocated before, unless the cluster is a
    ///   directory's (which may only gain entries) or one [`Editor::allow_own`] named;
    /// - a changed sector of a free cluster this edit did not allocate, or past the last cluster;
    /// - a changed reserved sector (the boot sector);
    /// - a FAT entry that was not 0 and changed, other than a replaced file's freed clusters; FAT
    ///   copies that differ from each other; a changed byte past the FAT's last entry;
    /// - a directory entry that was not empty and changed, other than a replaced file's (whose
    ///   first cluster and size alone may change).
    pub fn audit(&self) -> Result<Vec<Change>, String> {
        let geo = self.geo();
        let sectors = self.orig.len() / SECTOR;
        let changed: Vec<u64> = (0..sectors)
            .filter(|&s| {
                self.orig[s * SECTOR..(s + 1) * SECTOR] != self.work[s * SECTOR..(s + 1) * SECTOR]
            })
            .map(|s| s as u64)
            .collect();
        let refuse = |why: String| {
            Err(format!(
                "{why}: the edit is refused and nothing was written"
            ))
        };
        let mut classes: Vec<(u64, Class)> = Vec::with_capacity(changed.len());
        // The data area first: the rule that matters most is checked before any other.
        for &lba in changed.iter().filter(|&&l| l >= geo.data_start) {
            let c = (lba - geo.data_start) / geo.spc + 2;
            if !geo.in_range(c) {
                return refuse(format!(
                    "sector {lba} is past the volume's last cluster and would change"
                ));
            }
            let i = c as usize;
            let class = if self.survey.free[i] {
                if self.new_dir[i] {
                    Class::NewDir
                } else if self.new_file[i] {
                    Class::NewFile
                } else {
                    return refuse(format!(
                        "sector {lba} is in the free cluster {c}, which no new file was given, and would change"
                    ));
                }
            } else {
                match self.survey.owner_of(c) {
                    // A cluster of the caller's own set (its region, its directories): rewritten
                    // whole by the install, which keeps the set's kept entries itself.
                    _ if self.own[i] => Class::OwnFile,
                    Some(e) if e.is_dir => {
                        self.audit_entries(lba, &e.path)?;
                        Class::Dir
                    }
                    Some(e) => {
                        return refuse(format!(
                            "sector {lba} would touch an existing file's cluster: cluster {c} belongs to {}",
                            e.path
                        ));
                    }
                    None => {
                        return refuse(format!(
                            "sector {lba} would touch an existing file's cluster: cluster {c} is allocated in the FAT"
                        ));
                    }
                }
            };
            classes.push((lba, class));
        }
        // The same rule from the allocation's side: a cluster given to a new file or directory was
        // free before, whether or not the bytes written into it differ from what it held.
        for c in 2..geo.clusters + 2 {
            let i = c as usize;
            if !self.survey.free[i] && (self.new_file[i] || self.new_dir[i]) {
                let owner = self.survey.owner_of(c).map_or_else(
                    || "is allocated in the FAT".to_string(),
                    |e| format!("belongs to {}", e.path),
                );
                return refuse(format!(
                    "a new file was given cluster {c} and would touch an existing file's cluster: cluster {c} {owner}"
                ));
            }
        }
        for &lba in changed.iter().filter(|&&l| l < geo.data_start) {
            if lba < geo.reserved {
                return refuse(format!(
                    "sector {lba} is the boot sector or a reserved sector and would change"
                ));
            }
            if lba < geo.root_start {
                classes.push((
                    lba,
                    Class::Fat(((lba - geo.reserved) / geo.fat_sectors) as u8 + 1),
                ));
            } else {
                self.audit_entries(lba, "the root directory")?;
                classes.push((lba, Class::RootDir));
            }
        }
        // The FAT, entry by entry, and its copies against each other.
        let fat_bytes = geo.fat_sectors as usize * SECTOR;
        let first = geo.fat_at(0, 0);
        for copy in 1..geo.fats {
            let other = geo.fat_at(copy, 0);
            if self.work[first..first + fat_bytes] != self.work[other..other + fat_bytes] {
                return refuse(format!("FAT copy {} would differ from copy 1", copy + 1));
            }
        }
        for c in 0..geo.clusters + 2 {
            let (was, is) = (geo.entry(self.orig, 0, c), geo.entry(&self.work, 0, c));
            if was == is {
                continue;
            }
            let i = c as usize;
            let allowed = (was == 0 && c >= 2 && (self.new_file[i] || self.new_dir[i]))
                || (self.freed[i] && is == 0)
                || (c >= 2 && self.own[i]);
            if !allowed {
                let owner = self
                    .survey
                    .owner_of(c)
                    .map_or_else(|| "no directory entry".to_string(), |e| e.path.clone());
                return refuse(format!(
                    "the FAT entry of cluster {c} ({owner}) would change from {was:#06x} to {is:#06x}"
                ));
            }
        }
        let tail = geo.fat_at(0, geo.clusters + 2);
        if self.orig[tail..first + fat_bytes] != self.work[tail..first + fat_bytes] {
            return refuse("bytes past the FAT's last entry would change".to_string());
        }
        classes.sort();
        let mut out: Vec<Change> = Vec::new();
        let mut i = 0;
        while i < classes.len() {
            let (lba, class) = classes[i];
            let mut count = 1u64;
            while i + (count as usize) < classes.len()
                && classes[i + count as usize] == (lba + count, class)
            {
                count += 1;
            }
            let (from, to) = (lba as usize * SECTOR, (lba + count) as usize * SECTOR);
            out.push(Change {
                lba,
                count,
                class,
                old: Sha256::digest(&self.orig[from..to]).into(),
                new: Sha256::digest(&self.work[from..to]).into(),
            });
            i += count as usize;
        }
        Ok(out)
    }

    /// The entries of one changed directory sector: an entry that differs was empty before, or is
    /// a replaced file's, with only its first cluster and size changed.
    fn audit_entries(&self, lba: u64, shown: &str) -> Result<(), String> {
        let base = lba as usize * SECTOR;
        for at in (base..base + SECTOR).step_by(32) {
            let (was, is) = (&self.orig[at..at + 32], &self.work[at..at + 32]);
            if was == is || was.iter().all(|&b| b == 0) {
                continue;
            }
            let same_but_place = was[..20] == is[..20] && was[22..26] == is[22..26];
            if self.rewritten.contains(&at) && same_but_place {
                continue;
            }
            return Err(format!(
                "an existing directory entry of {shown} (sector {lba}, entry {}) would change: the edit is refused and nothing was written",
                (at - base) / 32
            ));
        }
        Ok(())
    }
}

/// `dir/name`, or `name` at the root.
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// The directory entries the kept children of a directory take: one per child, plus its long-name
/// entries. A directory with no kept file below it takes none.
fn kept_entries(children: &[Node], path: &str, keep: &dyn Fn(&str) -> bool) -> u64 {
    let own = |name: &str| -> u64 {
        if fatimage::is_83(name) {
            1
        } else {
            1 + name.encode_utf16().count().div_ceil(13) as u64
        }
    };
    children
        .iter()
        .map(|child| match child {
            Node::File { name, .. } if keep(&join(path, name)) => own(name),
            Node::File { .. } => 0,
            Node::Dir { name, children } => {
                if kept_entries(children, &join(path, name), keep) == 0 {
                    0
                } else {
                    own(name)
                }
            }
            Node::DirOf { name, .. } => own(name),
            Node::Region { name, .. } if keep(&join(path, name)) => own(name),
            Node::Region { .. } => 0,
        })
        .sum()
}

/// The first line of an undo file.
const UNDO_MAGIC: &[u8] = b"LEMNOS-BOOT-UNDO 1\n";

/// The undo file of an edit: for each changed range its first sector, its sector count, the SHA-256
/// its bytes have after the edit, and the bytes it had before. Applying it ([`apply_undo`]) to the
/// edited volume gives the original back, byte for byte.
///
/// Layout: the magic line, the volume's sector count (8 bytes, little endian), the number of ranges
/// (4 bytes), then per range: the first sector (8 bytes), the sector count (4 bytes), the SHA-256 of
/// the edited bytes (32 bytes), and the original bytes.
pub fn undo_file(orig: &[u8], changes: &[Change]) -> Vec<u8> {
    let mut out = UNDO_MAGIC.to_vec();
    out.extend_from_slice(&((orig.len() / SECTOR) as u64).to_le_bytes());
    out.extend_from_slice(&(changes.len() as u32).to_le_bytes());
    for c in changes {
        out.extend_from_slice(&c.lba.to_le_bytes());
        out.extend_from_slice(&(c.count as u32).to_le_bytes());
        out.extend_from_slice(&c.new);
        let (from, to) = (c.lba as usize * SECTOR, (c.lba + c.count) as usize * SECTOR);
        out.extend_from_slice(&orig[from..to]);
    }
    out
}

/// The ranges of an undo file: `(first sector, sector count)`, in the file's order.
pub fn undo_ranges(undo: &[u8]) -> Result<Vec<(u64, u64)>, String> {
    Ok(parse_undo(undo)?.1.iter().map(|r| (r.0, r.1)).collect())
}

/// One range of an undo file: its first sector, its sector count, the hash its bytes have after the
/// edit, and the bytes it had before.
type UndoRange<'a> = (u64, u64, &'a [u8], &'a [u8]);

/// The sector count and the ranges of an undo file.
fn parse_undo(undo: &[u8]) -> Result<(u64, Vec<UndoRange<'_>>), String> {
    let bad = || {
        "not an undo file of lemnos-ctl boot-partition --add-to: nothing was written".to_string()
    };
    let rest = undo.strip_prefix(UNDO_MAGIC).ok_or_else(bad)?;
    let total = u64::from_le_bytes(
        rest.get(..8)
            .ok_or_else(bad)?
            .try_into()
            .map_err(|_| bad())?,
    );
    let count = u32::from_le_bytes(
        rest.get(8..12)
            .ok_or_else(bad)?
            .try_into()
            .map_err(|_| bad())?,
    );
    let mut at = 12usize;
    let mut out = Vec::new();
    for _ in 0..count {
        let lba = u64::from_le_bytes(
            rest.get(at..at + 8)
                .ok_or_else(bad)?
                .try_into()
                .map_err(|_| bad())?,
        );
        let n = u64::from(u32::from_le_bytes(
            rest.get(at + 8..at + 12)
                .ok_or_else(bad)?
                .try_into()
                .map_err(|_| bad())?,
        ));
        let hash = rest.get(at + 12..at + 44).ok_or_else(bad)?;
        let len = (n as usize).checked_mul(SECTOR).ok_or_else(bad)?;
        let bytes = rest.get(at + 44..at + 44 + len).ok_or_else(bad)?;
        if n == 0 || lba.checked_add(n).is_none_or(|end| end > total) {
            return Err(bad());
        }
        out.push((lba, n, hash, bytes));
        at += 44 + len;
    }
    if at != rest.len() {
        return Err(bad());
    }
    Ok((total, out))
}

/// Applies an undo file to `image`, the edited volume, and returns the original. Refused, with
/// nothing changed, when the file is not an undo file, when the image is not the size the edit was
/// made on, or when a range of the image does not hold the bytes the edit left there (the image is
/// not the edited volume, or it was written since).
pub fn apply_undo(image: &[u8], undo: &[u8]) -> Result<Vec<u8>, String> {
    let (total, ranges) = parse_undo(undo)?;
    if (image.len() / SECTOR) as u64 != total || !image.len().is_multiple_of(SECTOR) {
        return Err(format!(
            "the undo file is for a volume of {total} sectors and this file is {} bytes: nothing was written",
            image.len()
        ));
    }
    for (lba, n, hash, _) in &ranges {
        let (from, to) = (*lba as usize * SECTOR, (*lba + *n) as usize * SECTOR);
        if Sha256::digest(&image[from..to]).as_slice() != *hash {
            return Err(format!(
                "sectors {lba} to {} do not hold what the edit left there: this is not the edited volume, or it was written since: nothing was written",
                lba + n - 1
            ));
        }
    }
    let mut out = image.to_vec();
    for (lba, _, _, bytes) in &ranges {
        let from = *lba as usize * SECTOR;
        out[from..from + bytes.len()].copy_from_slice(bytes);
    }
    Ok(out)
}

#[cfg(test)]
#[path = "fatadd_tests.rs"]
mod tests;
