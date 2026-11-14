//! Growing the data partition to the end of the boot disk at first start (M36, item g).
//!
//! The data partition is entry 2 of the MBR, the last one, type `0x83`, labelled `lemnos-data`.
//! Its length is set from the disk's size in four steps, in this order, and each step is derived
//! from the state it finds, so the whole pass is safe to repeat and a cut at any point is
//! finished by the next pass:
//!
//! 1. The table. [`lemnos_slot_store::mbr::plan_grow`] validates it and gives the new length; the
//!    only write is the four bytes of that entry's sector count (474 to 477 of sector 0). The
//!    sector is read back and must differ from the old one in those four bytes and nowhere else.
//! 2. The kernel's view. `BLKPG_RESIZE_PARTITION` on partition 2 to the table's length, then
//!    `/sys/class/block/<p2>/size` must say the new length.
//! 3. The filesystem. `EXT4_IOC_RESIZE_FS` on `/data` to the table's length in blocks, then the
//!    superblock must say the new count.
//!
//! A pass that finds nothing to do writes nothing. It refuses a table that is not the expected
//! layout, by name, and writes nothing then. It never touches partition 1 or any byte of sector 0
//! but the four above.
//!
//! A failed step is retried by the driver with a backoff capped at [`BACKOFF_CAP`]; a pass never
//! blocks the driver's start, and it is never given up. The pieces the kernel sees (the ioctls, the
//! `/sys` sizes, the disk node) are the [`Disk`] trait: the tests run every cut point on a file
//! standing for the disk, with an injected ioctl seam, and the real [`Real`] is the same code over
//! the kernel.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_slot_store::mbr::{self, Plan, SECTOR};

/// The data partition's entry in the table (0-based: entry 2 is index 1).
pub const ENTRY: usize = 1;
/// The data partition's number as the kernel names it.
pub const PARTNO: i32 = 2;
/// The label the data partition must carry.
pub const LABEL: &str = "lemnos-data";
/// Where the sector count of [`ENTRY`] sits in sector 0.
pub const COUNT_AT: u64 = 474;
/// The ext4 superblock, from the start of the partition.
const SUPER_AT: u64 = 1024;
const SUPER_BYTES: usize = 1024;
const MAGIC: u16 = 0xEF53;
const BLOCKS_LO: usize = 0x04;
const LOG_BLOCK: usize = 0x18;
const MAGIC_AT: usize = 0x38;
const FEATURE_INCOMPAT: usize = 0x60;
const LABEL_AT: usize = 0x78;
const LABEL_BYTES: usize = 16;
const BLOCKS_HI: usize = 0x150;
const INCOMPAT_64BIT: u32 = 0x80;

/// The first retry waits this long; each failure doubles it.
pub const BACKOFF_FIRST: Duration = Duration::from_secs(1);
/// The longest wait between two tries of a failed step.
pub const BACKOFF_CAP: Duration = Duration::from_secs(8);

/// Whether the grow runs, from the board file's `grow_data`: on unless the board says `false`. The
/// grow acts only on a disk that validates as the Lemnos layout, so the default is safe.
pub fn enabled(board: Option<bool>) -> bool {
    board.unwrap_or(true)
}

/// The next wait after a failure that waited `previous`: double it, capped.
pub fn next_backoff(previous: Duration) -> Duration {
    (previous * 2).min(BACKOFF_CAP)
}

/// The kernel and the disk, as one pass sees them. The real one is [`Real`]; the tests use a file.
pub trait Disk {
    /// The whole disk's size in sectors (`/sys/block/<disk>/size`).
    fn disk_sectors(&self) -> io::Result<u64>;
    /// Reads `buf.len()` bytes at byte `offset` of the disk.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    /// Writes `bytes` at byte `offset` of the disk, and nothing else.
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()>;
    /// Flushes the disk's writes.
    fn sync(&mut self) -> io::Result<()>;
    /// Partition 2's start in sectors, as the kernel has it (`/sys/class/block/<p2>/start`).
    fn part_start(&self) -> io::Result<u64>;
    /// Partition 2's length in sectors, as the kernel has it (`/sys/class/block/<p2>/size`).
    fn part_sectors(&self) -> io::Result<u64>;
    /// `BLKPG_RESIZE_PARTITION` on partition 2: its start and length in bytes.
    fn resize_partition(&mut self, start_bytes: u64, length_bytes: u64) -> io::Result<()>;
    /// `EXT4_IOC_RESIZE_FS` on the mounted data filesystem, to `blocks` blocks.
    fn resize_fs(&mut self, blocks: u64) -> io::Result<()>;
}

/// The block size and block count of an ext4 superblock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Super {
    block_size: u64,
    blocks: u64,
}

/// Reads partition 2's superblock: its magic, its label and its size. Refuses what is not ext4 by
/// name.
fn superblock(disk: &dyn Disk, start: u64) -> Result<(Super, String), String> {
    let mut sb = [0u8; SUPER_BYTES];
    disk.read_at(start * SECTOR as u64 + SUPER_AT, &mut sb)
        .map_err(|e| format!("partition 2's superblock can't be read: {e}"))?;
    if u16::from_le_bytes([sb[MAGIC_AT], sb[MAGIC_AT + 1]]) != MAGIC {
        return Err("partition 2 is not an ext4 filesystem: nothing changed".into());
    }
    let label = &sb[LABEL_AT..LABEL_AT + LABEL_BYTES];
    let label = String::from_utf8_lossy(label)
        .trim_end_matches('\0')
        .to_string();
    let le32 = |at: usize| u32::from_le_bytes([sb[at], sb[at + 1], sb[at + 2], sb[at + 3]]);
    let log = le32(LOG_BLOCK);
    let block_size = 1024u64
        .checked_shl(log)
        .ok_or("partition 2's block size is out of range: nothing changed")?;
    let mut blocks = u64::from(le32(BLOCKS_LO));
    if le32(FEATURE_INCOMPAT) & INCOMPAT_64BIT != 0 {
        blocks |= u64::from(le32(BLOCKS_HI)) << 32;
    }
    Ok((Super { block_size, blocks }, label))
}

fn sector0(disk: &dyn Disk) -> Result<[u8; SECTOR], String> {
    let mut sector = [0u8; SECTOR];
    disk.read_at(0, &mut sector)
        .map_err(|e| format!("the disk's table can't be read: {e}"))?;
    Ok(sector)
}

/// One pass: makes the table, the kernel's partition and the filesystem all the disk's end, or
/// says why it cannot. `Ok` means nothing is left to do.
///
/// # Errors
///
/// The reason in words, with nothing written past the step that failed. A refusal (a table that is
/// not the expected layout) writes nothing at all.
pub fn grow(disk: &mut dyn Disk) -> Result<(), String> {
    let disk_sectors = disk
        .disk_sectors()
        .map_err(|e| format!("the disk's size can't be read: {e}"))?;
    let mut sector = sector0(disk)?;
    let table = mbr::entries(&sector).map_err(|r| r.to_string())?;
    let start = u64::from(table[ENTRY].start);
    let kernel_start = disk
        .part_start()
        .map_err(|e| format!("partition 2's start can't be read: {e}"))?;
    if kernel_start != start {
        return Err(format!(
            "the kernel has partition 2 at sector {kernel_start} and the table at {start}: nothing changed"
        ));
    }
    let (_, label) = superblock(disk, start)?;
    if label != LABEL {
        return Err(format!(
            "partition 2 is labelled {label:?}, not {LABEL:?}: nothing changed"
        ));
    }

    // 1. The table.
    match mbr::plan_grow(&sector, ENTRY, disk_sectors).map_err(|r| r.to_string())? {
        Plan::Grow { sectors } => {
            let before = sector;
            disk.write_at(COUNT_AT, &sectors.to_le_bytes())
                .map_err(|e| format!("the table can't be written: {e}"))?;
            disk.sync()
                .map_err(|e| format!("the table can't be flushed: {e}"))?;
            sector = sector0(disk)?;
            let written = mbr::changed_bytes(&before, &sector);
            let expect: Vec<usize> = (COUNT_AT as usize..COUNT_AT as usize + 4).collect();
            let entry = mbr::entries(&sector).map_err(|r| r.to_string())?[ENTRY];
            if written != expect || entry.sectors != sectors {
                return Err(format!(
                    "the table after the write is not the one written ({} bytes changed, {} sectors read back for {sectors}): the write is retried; nothing else was changed",
                    written.len(),
                    entry.sectors
                ));
            }
        }
        Plan::Done => {}
    }
    let table_sectors = u64::from(mbr::entries(&sector).map_err(|r| r.to_string())?[ENTRY].sectors);

    // 2. The kernel's partition.
    let kernel = disk
        .part_sectors()
        .map_err(|e| format!("partition 2's size can't be read: {e}"))?;
    if kernel > table_sectors {
        return Err(format!(
            "the kernel gives partition 2 {kernel} sectors and the table {table_sectors}: the table is smaller than the kernel's view: nothing changed"
        ));
    }
    if kernel < table_sectors {
        if !mbr::sane_new_size(
            kernel as u32,
            table_sectors as u32,
            start as u32,
            disk_sectors,
        ) {
            return Err(format!(
                "the new length {table_sectors} sectors of partition 2 does not fit the disk: nothing changed"
            ));
        }
        disk.resize_partition(start * SECTOR as u64, table_sectors * SECTOR as u64)
            .map_err(|e| format!("the kernel refused to resize partition 2: {e}"))?;
        let now = disk
            .part_sectors()
            .map_err(|e| format!("partition 2's size can't be read: {e}"))?;
        if now != table_sectors {
            return Err(format!(
                "the kernel still gives partition 2 {now} sectors after the resize to {table_sectors}: retried"
            ));
        }
    }

    // 3. The filesystem.
    let (sb, _) = superblock(disk, start)?;
    let want = table_sectors * SECTOR as u64 / sb.block_size;
    if sb.blocks > want {
        return Err(format!(
            "the filesystem has {} blocks and partition 2 holds {want}: nothing changed",
            sb.blocks
        ));
    }
    if sb.blocks < want {
        disk.resize_fs(want)
            .map_err(|e| format!("the filesystem refused to grow to {want} blocks: {e}"))?;
        let (after, _) = superblock(disk, start)?;
        if after.blocks != want {
            return Err(format!(
                "the filesystem has {} blocks after the grow to {want}: retried",
                after.blocks
            ));
        }
    }
    Ok(())
}

/// The driver's retry state for a grow that has not finished.
#[derive(Debug, Clone)]
pub struct Retry {
    /// The time (driver clock, ns) before which nothing is tried again.
    pub next_ns: u64,
    /// The wait that follows the next failure.
    backoff: Duration,
    /// The last reason, so the same one is noted once.
    pub last: String,
}

impl Retry {
    /// A retry state that tries at once.
    pub fn new() -> Self {
        Self {
            next_ns: 0,
            backoff: BACKOFF_FIRST,
            last: String::new(),
        }
    }

    /// Records a failure at `now_ns`: the next try is after the current backoff, which then grows.
    /// Returns the time of that try.
    pub fn failed(&mut self, now_ns: u64, why: &str) -> u64 {
        self.last = why.to_string();
        self.next_ns =
            now_ns.saturating_add(u64::try_from(self.backoff.as_nanos()).unwrap_or(u64::MAX));
        self.backoff = next_backoff(self.backoff);
        self.next_ns
    }
}

impl Default for Retry {
    fn default() -> Self {
        Self::new()
    }
}

/// The grow of one boot: where the boot disk is (or why it is not known), where the data
/// filesystem is, and the retry state. Held by the driver until a pass finishes.
#[derive(Debug, Clone)]
pub struct Task {
    /// The boot disk's whole-disk node, or the reason it is not known.
    pub source: Result<PathBuf, String>,
    /// The data partition's mount point.
    pub data: PathBuf,
    /// The retry state.
    pub retry: Retry,
}

impl Task {
    /// A task that tries at once.
    pub fn new(source: Result<PathBuf, String>, data: PathBuf) -> Self {
        Self {
            source,
            data,
            retry: Retry::new(),
        }
    }

    /// Partition 2's length in MiB as the kernel has it now, `None` when the boot disk is not known
    /// or the size cannot be read. Read for a refusal's words only.
    pub fn partition_mib(&self) -> Option<u64> {
        let disk = self.source.as_ref().ok()?;
        let sectors = Disk::part_sectors(&Real::new(disk, &self.data)).ok()?;
        Some((sectors * SECTOR as u64) >> 20)
    }

    /// One pass against the real disk.
    ///
    /// # Errors
    ///
    /// The reason, from [`grow`], or that the boot disk is not known.
    pub fn attempt(&self) -> Result<(), String> {
        let disk = self.source.as_ref().map_err(|why| why.clone())?;
        grow(&mut Real::new(disk, &self.data))
    }
}

/// Partition 2's name for a disk named `name`: `<name>p2` when the name ends in a digit (`mmcblk0`,
/// `nvme0n1`), `<name>2` otherwise.
pub fn partition_name(name: &str) -> String {
    if name.ends_with(|c: char| c.is_ascii_digit()) {
        format!("{name}p{PARTNO}")
    } else {
        format!("{name}{PARTNO}")
    }
}

/// The kernel and disk of this board: the boot disk's node, its partition 2 and their `/sys`
/// entries, and the data filesystem.
pub struct Real {
    disk: PathBuf,
    sys_disk: PathBuf,
    sys_part: PathBuf,
    data: PathBuf,
}

impl Real {
    /// The disk named by its whole-disk node. Partition 2's node is `<disk>p2` when the disk's name
    /// ends in a digit (`mmcblk0p2`, `nvme0n1p2`), `<disk>2` otherwise.
    pub fn new(disk: &Path, data: &Path) -> Self {
        let name = disk
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let part_name = partition_name(&name);
        Self {
            disk: disk.to_path_buf(),
            sys_disk: PathBuf::from("/sys/block").join(&name),
            sys_part: PathBuf::from("/sys/class/block").join(&part_name),
            data: data.to_path_buf(),
        }
    }

    fn sys_number(path: &Path) -> io::Result<u64> {
        let text = fs::read_to_string(path)?;
        text.trim()
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, path.display().to_string()))
    }

    fn open_rw(&self) -> io::Result<File> {
        OpenOptions::new().read(true).write(true).open(&self.disk)
    }
}

impl Disk for Real {
    fn disk_sectors(&self) -> io::Result<u64> {
        Self::sys_number(&self.sys_disk.join("size"))
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        File::open(&self.disk)?.read_exact_at(buf, offset)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.open_rw()?.write_all_at(bytes, offset)
    }

    fn sync(&mut self) -> io::Result<()> {
        self.open_rw()?.sync_all()
    }

    fn part_start(&self) -> io::Result<u64> {
        Self::sys_number(&self.sys_part.join("start"))
    }

    fn part_sectors(&self) -> io::Result<u64> {
        Self::sys_number(&self.sys_part.join("size"))
    }

    fn resize_partition(&mut self, start_bytes: u64, length_bytes: u64) -> io::Result<()> {
        let disk = self.open_rw()?;
        let start =
            i64::try_from(start_bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let length = i64::try_from(length_bytes)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        lemnos_linux_sys::blockdev::resize_partition(disk.as_fd(), PARTNO, start, length)
    }

    fn resize_fs(&mut self, blocks: u64) -> io::Result<()> {
        let dir = File::open(&self.data)?;
        lemnos_linux_sys::ext4::resize_fs(dir.as_fd(), blocks)
    }
}

#[cfg(test)]
mod tests;
