//! Growing the data partition to the end of the boot disk at first start (M36, item g).
//!
//! The data partition is entry 2 of the MBR, the last one, type `0x83`, labelled `lemnos-data`.
//! Its length is set from the disk's size in four steps, in this order, and each step is derived
//! from the state it finds, so the whole pass is safe to repeat and a cut at any point is
//! finished by the next pass:
//!
//! 0. The baseline. The filesystem's size on the mount and the target length are stored in
//!    `board/slots/grow` before the table's first write, and the record is cleared when the
//!    resize is confirmed (see [`Task::confirm`]).
//! 1. The table. [`lemnos_slot_store::mbr::plan_grow`] validates it and gives the new length; the
//!    only write is the four bytes of that entry's sector count (474 to 477 of sector 0). The
//!    sector is read back and must differ from the old one in those four bytes and nowhere else.
//! 2. The kernel's view. `BLKPG_RESIZE_PARTITION` on partition 2 to the table's length, then
//!    `/sys/class/block/<p2>/size` must say the new length.
//! 3. The filesystem. `EXT4_IOC_RESIZE_FS` on `/data` to the table's length in blocks. The kernel's
//!    return is the check: the superblock on the device may still show the old count while the
//!    filesystem is mounted, because the journal holds the new one until a checkpoint.
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

#[cfg(feature = "linux")]
use std::fs::{self, File, OpenOptions};
use std::io;
#[cfg(feature = "linux")]
use std::os::fd::AsFd;
#[cfg(feature = "linux")]
use std::os::unix::fs::FileExt;
#[cfg(feature = "linux")]
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::mbr::{self, MIN_GAIN_SECTORS, Plan, SECTOR};
#[cfg(feature = "linux")]
use crate::store;

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
    /// The data filesystem's size as the kernel reports it on the mount (`statfs`), in bytes.
    fn fs_bytes(&self) -> io::Result<u64>;
    /// Whether the data filesystem is mounted read-only (`/proc/mounts`).
    fn data_read_only(&self) -> io::Result<bool>;
    /// The stored baseline record (`board/slots/grow` on the data partition), `None` when missing.
    fn read_record(&self) -> io::Result<Option<Vec<u8>>>;
    /// Replaces the stored record whole, durably: written, synced and renamed into place.
    fn write_record(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// Removes the stored record; a missing one is not an error.
    fn clear_record(&mut self) -> io::Result<()>;
}

/// The largest the data partition may be after a grow: 256 GiB. A target above it is refused by name
/// (`grow-too-large`) before anything is written; the boot continues with the partition as it is.
/// The media this runs on is at most about 16 GB, and the bench cards 64 GB.
pub const MAX_BYTES: u64 = 256 << 30;

/// A grow refused by name before any write: the named state (`grow-too-large`, `data-read-only`,
/// `gpt-unavailable`), its words, and the sizes (target and limit, bytes) when there are two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub state: &'static str,
    pub reason: String,
    pub sizes: Option<(u64, u64)>,
}

/// What one pass did with the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pass {
    /// Nothing was resized: the table, the kernel and the filesystem already reach the disk's end.
    /// The stored baseline has been cleared.
    Done,
    /// The filesystem was resized (the kernel accepted it). `baseline` is what the confirmation
    /// compares the kernel's view with.
    Resized { baseline: Baseline },
    /// The grow is refused by name and nothing was written (see [`MAX_BYTES`]).
    Refused(Refusal),
}

/// The kernel's size of the filesystem before the resize, as stored before the grow's first write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Baseline {
    /// The stored size (bytes) before the resize.
    Stored(u64),
    /// No record, although the table already holds the grown length: the record was lost.
    Missing,
    /// The record does not decode, or is for another size.
    Corrupt,
}

/// How many tries the kernel's view gets to show a resize that the kernel accepted. The tries
/// follow the retry backoff (1, 2, 4, 8, 8, 8 s), so the view is checked for 31 s after the resize.
pub const CONFIRM_TRIES: u32 = 6;

/// A resize the kernel accepted and its view has not confirmed: the named state `grown-unconfirmed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unconfirmed {
    /// The stored size before the resize (bytes), `None` when there is no valid record.
    pub baseline: Option<u64>,
    /// The kernel's size of the filesystem when it was last checked (bytes).
    pub seen: u64,
    /// The reason in words.
    pub reason: String,
}

/// The stored baseline record's first line, and the fixed words of its format.
const RECORD_MAGIC: &str = "lemnos-grow 1";

/// The record: the magic, the baseline and the target, and a checksum of the three lines before it.
/// A torn or foreign file does not decode, and the confirmation names it.
fn encode_record(baseline: u64, target: u64) -> Vec<u8> {
    let body = format!("{RECORD_MAGIC}\nbaseline {baseline}\ntarget {target}\n");
    format!("{body}sum {:016x}\n", fnv1a(body.as_bytes())).into_bytes()
}

fn decode_record(bytes: &[u8]) -> Option<(u64, u64)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let [magic, baseline, target, _] = lines.as_slice() else {
        return None;
    };
    let body = format!("{magic}\n{baseline}\n{target}\n");
    if *magic != RECORD_MAGIC || text != format!("{body}sum {:016x}\n", fnv1a(body.as_bytes())) {
        return None;
    }
    let baseline = baseline.strip_prefix("baseline ")?.parse().ok()?;
    let target = target.strip_prefix("target ")?.parse().ok()?;
    Some((baseline, target))
}

/// FNV-1a, 64 bits: a checksum against torn and damaged records, not against an adversary.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// What the stored record says.
enum Found {
    Missing,
    Corrupt,
    Valid { baseline: u64, target: u64 },
}

fn found(disk: &dyn Disk) -> Result<Found, String> {
    match disk
        .read_record()
        .map_err(|e| format!("the stored baseline can't be read: {e}"))?
    {
        None => Ok(Found::Missing),
        Some(bytes) => Ok(match decode_record(&bytes) {
            Some((baseline, target)) => Found::Valid { baseline, target },
            None => Found::Corrupt,
        }),
    }
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

/// A refusal before any write: the named state, its words, and the sizes when there are two
/// (the target and the limit, in bytes).
fn refuse(state: &'static str, reason: String, sizes: Option<(u64, u64)>) -> Pass {
    Pass::Refused(Refusal {
        state,
        reason,
        sizes,
    })
}

/// Partition 2's start as the kernel has it must be the table's.
fn check_kernel_start(disk: &dyn Disk, start: u64) -> Result<(), String> {
    let kernel_start = disk
        .part_start()
        .map_err(|e| format!("partition 2's start can't be read: {e}"))?;
    if kernel_start != start {
        return Err(format!(
            "the kernel has partition 2 at sector {kernel_start} and the table at {start}: nothing changed"
        ));
    }
    Ok(())
}

/// The data filesystem on partition 2 must be ext4 labelled [`LABEL`]; returns its size.
fn check_label(disk: &dyn Disk, start: u64) -> Result<Super, String> {
    let (sb, label) = superblock(disk, start)?;
    if label != LABEL {
        return Err(format!(
            "partition 2 is labelled {label:?}, not {LABEL:?}: nothing changed"
        ));
    }
    Ok(sb)
}

/// Step 0, the baseline the confirmation compares with. It is stored before the table's first write,
/// so a grow that has begun always has one: a restart after the resize finds it, and a record that is
/// missing or does not decode is named by the confirmation, not taken for a new start.
fn baseline_step(disk: &mut dyn Disk, expected: u64, grows: bool) -> Result<Baseline, String> {
    Ok(match (found(disk)?, grows) {
        (Found::Valid { baseline, target }, _) if target == expected => Baseline::Stored(baseline),
        (Found::Valid { .. } | Found::Corrupt, _) => Baseline::Corrupt,
        (Found::Missing, true) => {
            let bytes = disk
                .fs_bytes()
                .map_err(|e| format!("the filesystem's size can't be read: {e}"))?;
            disk.write_record(&encode_record(bytes, expected))
                .map_err(|e| format!("the baseline can't be stored: {e}: nothing changed"))?;
            Baseline::Stored(bytes)
        }
        (Found::Missing, false) => Baseline::Missing,
    })
}

/// Step 2 and 3's kernel half: `BLKPG_RESIZE_PARTITION` to the table's length, when the kernel has
/// less, and the kernel's view must then show it.
fn kernel_step(
    disk: &mut dyn Disk,
    start: u64,
    table_sectors: u64,
    disk_sectors: u64,
) -> Result<(), String> {
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
    Ok(())
}

/// Step 3, the filesystem, and the end of the pass. `Pass::Resized` when the kernel accepted the
/// resize; `Pass::Done` (the record cleared) when the filesystem already reaches the table's end.
fn fs_step(
    disk: &mut dyn Disk,
    start: u64,
    table_sectors: u64,
    baseline: Baseline,
) -> Result<Pass, String> {
    let (sb, _) = superblock(disk, start)?;
    let want = table_sectors * SECTOR as u64 / sb.block_size;
    if sb.blocks > want {
        return Err(format!(
            "the filesystem has {} blocks and partition 2 holds {want}: nothing changed",
            sb.blocks
        ));
    }
    if sb.blocks < want {
        // The kernel's answer is the first check: EXT4_IOC_RESIZE_FS returns 0 only once the mounted
        // filesystem has `want` blocks. The superblock is not read back here. While the filesystem
        // is mounted its new count is held by the journal until a checkpoint, so a read of the
        // device can still show the old count after a resize that succeeded (QEMU, GROW stage 0:
        // every retry showed the old count for 48 s). The kernel's view is checked by
        // [`Task::confirm`], for a bounded number of tries.
        disk.resize_fs(want)
            .map_err(|e| format!("the filesystem refused to grow to {want} blocks: {e}"))?;
        return Ok(Pass::Resized { baseline });
    }
    // Finished: the stored baseline has nothing left to confirm.
    disk.clear_record()
        .map_err(|e| format!("the stored baseline can't be cleared: {e}"))?;
    Ok(Pass::Done)
}

/// One pass: makes the table, the kernel's partition and the filesystem all the disk's end, or
/// says why it cannot. `Ok` means nothing is left to do.
///
/// # Errors
///
/// The reason in words, with nothing written past the step that failed. A refusal (a table that is
/// not the expected layout) writes nothing at all.
pub fn grow(disk: &mut dyn Disk) -> Result<Pass, String> {
    let disk_sectors = disk
        .disk_sectors()
        .map_err(|e| format!("the disk's size can't be read: {e}"))?;
    if disk
        .data_read_only()
        .map_err(|e| format!("the data partition's mount can't be read: {e}"))?
    {
        return Ok(refuse(
            "data-read-only",
            "data-read-only: /data is mounted read-only, so its baseline can't be stored and its filesystem can't grow: nothing changed"
                .into(),
            None,
        ));
    }
    let mut sector = sector0(disk)?;
    let mut second = [0u8; SECTOR];
    disk.read_at(SECTOR as u64, &mut second)
        .map_err(|e| format!("the disk's second sector can't be read: {e}"))?;
    if &second[..8] == b"EFI PART" {
        return grow_gpt(disk, disk_sectors);
    }
    let table = mbr::entries(&sector).map_err(|r| r.to_string())?;
    let start = u64::from(table[ENTRY].start);
    check_kernel_start(disk, start)?;
    check_label(disk, start)?;

    let plan = mbr::plan_grow(&sector, ENTRY, disk_sectors).map_err(|r| r.to_string())?;
    let grows = matches!(plan, Plan::Grow { .. });
    let target = match plan {
        Plan::Grow { sectors } => u64::from(sectors),
        Plan::Done => u64::from(table[ENTRY].sectors),
    };
    if grows && target * SECTOR as u64 > MAX_BYTES {
        return Ok(too_large(target * SECTOR as u64));
    }
    let baseline = baseline_step(disk, target, grows)?;

    // 1. The table: the four bytes of the entry's count, nothing else.
    if let Plan::Grow { sectors } = plan {
        let before = sector;
        disk.write_at(COUNT_AT, &sectors.to_le_bytes())
            .map_err(|e| format!("the table can't be written: {e}"))?;
        disk.sync()
            .map_err(|e| format!("the table can't be flushed: {e}"))?;
        // The sector must be the old one with exactly the new count. Comparing the bytes that
        // changed would refuse a write whose count shares a byte with the old one (the top byte
        // of any count under 2^24 sectors is 0 both before and after).
        let mut expect = before;
        expect[COUNT_AT as usize..COUNT_AT as usize + 4].copy_from_slice(&sectors.to_le_bytes());
        sector = sector0(disk)?;
        if sector != expect {
            let changed = mbr::changed_bytes(&before, &sector);
            return Err(format!(
                "the table after the write is not the one written ({} bytes changed, at {changed:?}; the count is 4 bytes at {COUNT_AT}): the write is retried; nothing else was changed",
                changed.len(),
            ));
        }
    }
    let table_sectors = u64::from(mbr::entries(&sector).map_err(|r| r.to_string())?[ENTRY].sectors);
    kernel_step(disk, start, table_sectors, disk_sectors)?;
    fs_step(disk, start, table_sectors, baseline)
}

/// The refusal for a target above [`MAX_BYTES`].
fn too_large(target_bytes: u64) -> Pass {
    refuse(
        "grow-too-large",
        format!(
            "grow-too-large: the data partition would be {target_bytes} bytes ({} GiB) and the maximum is {MAX_BYTES} bytes (256 GiB): nothing changed; the boot continues with the partition as it is",
            target_bytes >> 30
        ),
        Some((target_bytes, MAX_BYTES)),
    )
}

/// A GPT disk's grow. The table is read from the primary, or from the backup when the primary is
/// damaged. A table the grow cannot handle is the named state `gpt-unavailable`, before any write.
fn grow_gpt(disk: &mut dyn Disk, disk_sectors: u64) -> Result<Pass, String> {
    let loaded = match gpt::load(disk, disk_sectors) {
        Ok(loaded) => loaded,
        Err(why) => {
            return Ok(refuse(
                "gpt-unavailable",
                format!("gpt-unavailable: {why}: nothing changed"),
                None,
            ));
        }
    };
    let (start, old_end, type_guid) = gpt::entry(&loaded.array, ENTRY);
    if type_guid == [0u8; 16] {
        return Ok(refuse(
            "gpt-unavailable",
            "gpt-unavailable: partition 2 is unused in the GPT: nothing changed".into(),
            None,
        ));
    }
    check_kernel_start(disk, start)?;
    check_label(disk, start)?;
    if let Some(why) = gpt::not_last(&loaded.array, ENTRY, start) {
        return Ok(refuse(
            "gpt-unavailable",
            format!("gpt-unavailable: {why}: nothing changed"),
            None,
        ));
    }
    let target_end = gpt::target_end(disk_sectors);
    let grows = target_end >= old_end + MIN_GAIN_SECTORS;
    let target_sectors = if grows {
        target_end + 1 - start
    } else {
        old_end + 1 - start
    };
    if grows {
        if target_sectors * SECTOR as u64 > MAX_BYTES {
            return Ok(too_large(target_sectors * SECTOR as u64));
        }
        if loaded.from_backup {
            return Ok(refuse(
                "gpt-unavailable",
                "gpt-unavailable: the primary GPT is damaged and the backup still holds the old table, so the grow can't start from it: nothing changed".into(),
                None,
            ));
        }
        if loaded.header.alt_lba <= old_end + gpt::ARRAY_SECTORS
            || loaded.header.alt_lba >= disk_sectors
        {
            return Ok(refuse(
                "gpt-unavailable",
                format!(
                    "gpt-unavailable: the backup GPT is at LBA {}, not past partition 2's end {old_end} inside the disk: nothing changed",
                    loaded.header.alt_lba
                ),
                None,
            ));
        }
    }
    let baseline = baseline_step(disk, target_sectors, grows)?;
    let table_sectors = if grows {
        gpt::grow_table(disk, &loaded, target_end, disk_sectors)?;
        target_sectors
    } else {
        if loaded.from_backup {
            gpt::repair_primary(disk, &loaded, disk_sectors)?;
        }
        old_end + 1 - start
    };
    kernel_step(disk, start, table_sectors, disk_sectors)?;
    fs_step(disk, start, table_sectors, baseline)
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

/// The overrun bound: a fixed part plus a part per TiB of disk. The resize cannot be interrupted, so
/// the bound only names an overrun; the result is still taken when it arrives.
pub const OVERRUN_BASE: Duration = Duration::from_secs(10);
/// Per TiB of disk (the resize's cost grows with the number of groups; QEMU, GROW2: 4.3 s per TiB).
pub const OVERRUN_PER_TIB: Duration = Duration::from_secs(30);
/// How often the tick looks at a pass in flight.
pub const POLL_EVERY: Duration = Duration::from_millis(100);

/// The bound for a disk of `disk_bytes`: [`OVERRUN_BASE`] plus [`OVERRUN_PER_TIB`] for each TiB,
/// rounded up.
pub fn overrun_bound(disk_bytes: u64) -> Duration {
    let tib = disk_bytes.div_ceil(1 << 40);
    OVERRUN_BASE + OVERRUN_PER_TIB * u32::try_from(tib).unwrap_or(u32::MAX)
}

/// The confirmation of the resizes one boot makes: the kernel's view, checked after each pass. The
/// fields carry the checks made so far and the state the view is in (see [`Confirm::confirm`]).
#[derive(Debug, Default)]
pub struct Confirm {
    checks: u32,
    unconfirmed: Option<Unconfirmed>,
}

impl Confirm {
    /// The state the kernel's view is in, when a resize is accepted and not yet confirmed.
    pub fn unconfirmed(&self) -> Option<&Unconfirmed> {
        self.unconfirmed.as_ref()
    }

    /// Checks the kernel's view after a pass. A resize the kernel accepted is confirmed when the
    /// filesystem's size on the mount is larger than the stored baseline; the stored record is then
    /// cleared. A baseline that is missing or corrupt cannot confirm anything, and is named as the
    /// `grown-unconfirmed` state at once. A view that has not shown the resize after
    /// [`CONFIRM_TRIES`] checks is `grown-unconfirmed` with the sizes seen; the checks go on, on the
    /// retry backoff, and the state stays until the view shows the grow.
    ///
    /// # Errors
    ///
    /// The reason in words, when the resize is not confirmed yet, or the record cannot be cleared.
    pub fn confirm(&mut self, disk: &mut dyn Disk, pass: Pass) -> Result<(), String> {
        let baseline = match pass {
            // A finished pass cleared its record in `grow`; nothing is left to confirm.
            Pass::Done => {
                self.unconfirmed = None;
                self.checks = 0;
                return Ok(());
            }
            Pass::Resized { baseline } => baseline,
            // A refusal is recorded by `finish`; nothing was written and nothing is confirmed.
            Pass::Refused(_) => return Ok(()),
        };
        let seen = disk
            .fs_bytes()
            .map_err(|e| format!("the filesystem's size can't be read after the grow: {e}"))?;
        let stored = match baseline {
            Baseline::Stored(stored) => stored,
            Baseline::Missing | Baseline::Corrupt => {
                let reason = match baseline {
                    Baseline::Missing => {
                        "grown-unconfirmed: the stored baseline is missing, so the resize cannot be confirmed"
                    }
                    _ => {
                        "grown-unconfirmed: the stored baseline is corrupt or for another size, so the resize cannot be confirmed"
                    }
                };
                self.unconfirmed = Some(Unconfirmed {
                    baseline: None,
                    seen,
                    reason: reason.to_string(),
                });
                return Err(reason.to_string());
            }
        };
        if seen > stored {
            disk.clear_record()
                .map_err(|e| format!("the stored baseline can't be cleared: {e}"))?;
            self.checks = 0;
            self.unconfirmed = None;
            return Ok(());
        }
        self.checks += 1;
        if self.checks < CONFIRM_TRIES {
            return Err(format!(
                "the kernel's view has not shown the grow yet ({seen} bytes, was {stored}): checked again"
            ));
        }
        let reason = format!(
            "grown-unconfirmed: the kernel's view shows {seen} bytes, was {stored}, after {CONFIRM_TRIES} checks: the grow is retried"
        );
        self.unconfirmed = Some(Unconfirmed {
            baseline: Some(stored),
            seen,
            reason: reason.clone(),
        });
        Err(reason)
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

#[cfg(feature = "linux")]
/// The kernel and disk of this board: the boot disk's node, its partition 2 and their `/sys`
/// entries, and the data filesystem.
pub struct Real {
    disk: PathBuf,
    sys_disk: PathBuf,
    sys_part: PathBuf,
    data: PathBuf,
}

#[cfg(feature = "linux")]
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

    /// The stored baseline record: the state directory's `grow` file (`Layout::state_dir`).
    fn record(&self) -> PathBuf {
        self.data.join("board").join("slots").join("grow")
    }

    fn open_rw(&self) -> io::Result<File> {
        OpenOptions::new().read(true).write(true).open(&self.disk)
    }
}

#[cfg(feature = "linux")]
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

    fn fs_bytes(&self) -> io::Result<u64> {
        lemnos_linux_sys::statfs::total_bytes(&self.data)
    }

    fn data_read_only(&self) -> io::Result<bool> {
        let mounts = fs::read_to_string("/proc/mounts")?;
        let mount = self.data.to_string_lossy();
        // The last line for the mount point is the one in force.
        let mut read_only = false;
        for line in mounts.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 4 && fields[1] == mount {
                read_only = fields[3].split(',').any(|option| option == "ro");
            }
        }
        Ok(read_only)
    }

    fn read_record(&self) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.record()) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write_record(&mut self, bytes: &[u8]) -> io::Result<()> {
        let path = self.record();
        fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        store::replace(&path, bytes)
    }

    fn clear_record(&mut self) -> io::Result<()> {
        let path = self.record();
        match fs::remove_file(&path) {
            Ok(()) => store::sync_dir(path.parent().unwrap_or(Path::new("."))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// The file, under `board/slots` on the data partition, where the boot's grow leaves its state for the
/// daemon to report (see [`State`]).
pub const STATE_FILE: &str = "grow-state";

/// The time past the overrun bound a grow may run before the boot stops feeding the watchdog: the
/// hard bound is the overrun bound plus this margin. Past it the boot stops feeding, so the watchdog
/// resets the board; the next boot refuses the attempt by name (see [`run_boot`]).
pub const STALL_MARGIN: Duration = Duration::from_secs(15);

/// The state name a refused, unfinished attempt carries (the grow did not return in an earlier boot).
pub const STALLED: &str = "grow-stalled";

/// What the boot's grow is doing, or did: written by the boot (slot-init) and read by the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// An attempt is in flight, or was when the boot ended: the boot that started it, when (unix ms),
    /// its overrun bound (ms), and the target (bytes). Written before the first write of the pass.
    Running {
        boot_id: String,
        started_ms: u64,
        bound_ms: u64,
        target_bytes: u64,
    },
    /// Nothing was done. The reason is empty when the partition already reaches the disk's end; it
    /// is `off in the board file` when the board file opts out (`<prefix>.grow=0` on the command line).
    NotNeeded { reason: String },
    /// The partition was grown and the kernel's view confirmed it.
    Grown,
    /// Refused by name before any write: the state's name (`grow-too-large`, `data-read-only`, ...) and its words.
    Refused { state: String, reason: String },
    /// Every try in this boot failed. The reason is the last one; the next boot tries again.
    Failed { reason: String },
    /// The kernel accepted the resize and its view did not confirm it: the words, the baseline (bytes,
    /// when the record had one) and the size the view showed.
    Unconfirmed {
        reason: String,
        baseline: Option<u64>,
        seen: u64,
    },
}

fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// The state as one line of text: a word for its kind, then its fields.
pub fn encode_state(state: &State) -> String {
    match state {
        State::Running {
            boot_id,
            started_ms,
            bound_ms,
            target_bytes,
        } => format!("attempt started {boot_id} {started_ms} {bound_ms} {target_bytes}\n"),
        State::NotNeeded { reason } if reason.is_empty() => "not-needed\n".into(),
        State::NotNeeded { reason } => format!("not-needed {}\n", one_line(reason)),
        State::Grown => "grown\n".into(),
        State::Refused { state, reason } => format!("refused {state} {}\n", one_line(reason)),
        State::Failed { reason } => format!("failed {}\n", one_line(reason)),
        State::Unconfirmed {
            reason,
            baseline,
            seen,
        } => {
            let baseline = baseline.map_or("-".to_string(), |b| b.to_string());
            format!("unconfirmed {baseline} {seen} {}\n", one_line(reason))
        }
    }
}

/// The state a file holds, `None` when it does not decode.
pub fn decode_state(text: &str) -> Option<State> {
    let line = text.lines().next()?;
    let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
    if let Some(attempt) = line.strip_prefix("attempt started ") {
        let mut words = attempt.split(' ');
        return Some(State::Running {
            boot_id: words.next()?.to_string(),
            started_ms: words.next()?.parse().ok()?,
            bound_ms: words.next()?.parse().ok()?,
            target_bytes: words.next()?.parse().ok()?,
        });
    }
    match kind {
        "not-needed" => Some(State::NotNeeded {
            reason: rest.to_string(),
        }),
        "grown" => Some(State::Grown),
        "refused" => {
            let (state, reason) = rest.split_once(' ').unwrap_or((rest, ""));
            Some(State::Refused {
                state: state.to_string(),
                reason: reason.to_string(),
            })
        }
        "failed" => Some(State::Failed {
            reason: rest.to_string(),
        }),
        "unconfirmed" => {
            let (baseline, rest) = rest.split_once(' ')?;
            let (seen, reason) = rest.split_once(' ').unwrap_or((rest, ""));
            Some(State::Unconfirmed {
                reason: reason.to_string(),
                baseline: if baseline == "-" {
                    None
                } else {
                    Some(baseline.parse().ok()?)
                },
                seen: seen.parse().ok()?,
            })
        }
        _ => None,
    }
}

/// Runs the boot's grow to its end and returns the state it leaves, or `None` when the call stalled.
///
/// The attempt is reported first (a [`State::Running`] carrying `boot_id`), before the pass's first
/// write, so a boot that dies in the call leaves the marker for the next boot to refuse. The pass runs
/// on its own thread, one try at a time; the boot polls it with `wait` (which feeds the watchdog) and
/// `report` gets each change. A failed try is tried again on the backoff while `overrun_bound` allows,
/// then the boot goes on with `Failed`; the next boot tries again. Past the hard bound (the overrun
/// bound plus [`STALL_MARGIN`]) with the call still out, `None` is returned and nothing more is fed:
/// the caller stops feeding the watchdog and the board resets. The call is not cut short.
///
/// `open` opens the disk for the boot; a disk that can't be opened is a failed try.
pub fn run_boot(
    boot_id: &str,
    open: &dyn Fn() -> Result<Box<dyn Disk + Send>, String>,
    wait: &dyn Fn(Duration),
    now_ms: &dyn Fn() -> u64,
    report: &dyn Fn(&State),
) -> Option<State> {
    let started_ms = now_ms();
    let mut disk = match open() {
        Ok(disk) => disk,
        Err(reason) => return Some(finish_failed(report, reason)),
    };
    let disk_bytes = disk
        .disk_sectors()
        .unwrap_or(0)
        .saturating_mul(SECTOR as u64);
    let bound = overrun_bound(disk_bytes);
    let bound_ms = u64::try_from(bound.as_millis()).unwrap_or(u64::MAX);
    let hard = bound + STALL_MARGIN;
    let hard_ms = u64::try_from(hard.as_millis()).unwrap_or(u64::MAX);
    // Time the boot has fed the watchdog while waiting, counted with the clock: a clock that does not
    // move still reaches the hard bound.
    let mut polled = Duration::ZERO;
    let mut backoff = BACKOFF_FIRST;
    // The time the failed tries have waited, counted as well as the clock: a clock that does not move
    // still reaches the bound.
    let mut waited = Duration::ZERO;
    let mut last_failure: Option<String> = None;
    let mut confirm = Confirm::default();
    // The attempt is written before the pass's first write: a boot that does not return from the call
    // leaves this marker, and the next boot refuses the attempt by name.
    report(&State::Running {
        boot_id: boot_id.to_string(),
        started_ms,
        bound_ms,
        target_bytes: disk_bytes,
    });
    loop {
        let (sender, results) = mpsc::channel();
        let job = disk;
        let worker = thread::Builder::new().name("grow".into()).spawn(move || {
            let mut job = job;
            let outcome = grow(job.as_mut());
            let _ = sender.send((job, outcome));
        });
        if let Err(error) = worker {
            return Some(finish_failed(
                report,
                format!("the grow's thread can't start: {error}"),
            ));
        }
        let (back, outcome) = loop {
            match results.try_recv() {
                Ok(done) => break done,
                Err(mpsc::TryRecvError::Empty) => {
                    // Past the hard bound the call is taken as stalled: nothing more is fed, and the
                    // caller stops feeding the watchdog. The thread is left to the reset.
                    let elapsed_ms = now_ms().saturating_sub(started_ms);
                    if elapsed_ms >= hard_ms || polled >= hard {
                        return None;
                    }
                    wait(POLL_EVERY);
                    polled += POLL_EVERY;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Some(finish_failed(
                        report,
                        "the grow's thread ended without a result".into(),
                    ));
                }
            }
        };
        disk = back;
        match outcome {
            Ok(Pass::Refused(refusal)) => {
                let state = State::Refused {
                    state: refusal.state.to_string(),
                    reason: refusal.reason,
                };
                report(&state);
                return Some(state);
            }
            Ok(Pass::Done) => {
                let state = State::NotNeeded {
                    reason: String::new(),
                };
                report(&state);
                return Some(state);
            }
            Ok(pass) => {
                // Confirmed when the kernel's view shows the resize; a view that does not is checked
                // again on the backoff until `Confirm` names it unconfirmed.
                loop {
                    match confirm.confirm(disk.as_mut(), pass.clone()) {
                        Ok(()) => {
                            report(&State::Grown);
                            return Some(State::Grown);
                        }
                        Err(_) if confirm.unconfirmed().is_some() => {
                            let unconfirmed =
                                confirm.unconfirmed().cloned().unwrap_or(Unconfirmed {
                                    baseline: None,
                                    seen: 0,
                                    reason: String::new(),
                                });
                            let state = State::Unconfirmed {
                                reason: unconfirmed.reason,
                                baseline: unconfirmed.baseline,
                                seen: unconfirmed.seen,
                            };
                            report(&state);
                            return Some(state);
                        }
                        Err(_) => wait(backoff),
                    }
                    backoff = next_backoff(backoff);
                }
            }
            Err(reason) => {
                let elapsed_ms = now_ms().saturating_sub(started_ms);
                if elapsed_ms >= bound_ms || waited >= bound {
                    return Some(finish_failed(report, reason));
                }
                if last_failure.as_deref() != Some(reason.as_str()) {
                    report(&State::Failed {
                        reason: reason.clone(),
                    });
                }
                last_failure = Some(reason);
                wait(backoff);
                waited += backoff;
                backoff = next_backoff(backoff);
            }
        }
    }
}

fn finish_failed(report: &dyn Fn(&State), reason: String) -> State {
    let state = State::Failed { reason };
    report(&state);
    state
}

mod gpt;

#[cfg(test)]
mod tests;
