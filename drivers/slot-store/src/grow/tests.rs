//! The grow on a file that stands for the disk, with the kernel's side (the sizes, the partition
//! resize and the filesystem resize) as a fake that can refuse, lose power at any step, or tear the
//! table write. The file's bytes are the disk's: every assertion on what changed reads them.
//!
//! What the fake does not share with the kernel: the kernel's own checks on a mounted partition
//! (`BLKPG` while `/data` is open), the page cache, and the device's atomicity for a sector write.
//! Those are the board check (`testing/board-runs`), not proved here.

use super::*;
use crate::mbr;
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

const SECT: u64 = 512;
/// The disk: 32 000 000 sectors (16 GB).
const DISK: u64 = 32_000_000;
const P1_START: u32 = 8192;
const P1_SECTORS: u32 = 161_792;
const P2_START: u32 = P1_START + P1_SECTORS;
/// The data partition before the grow: 100 000 sectors.
const P2_OLD: u32 = 100_000;
/// The grown length: from the partition's start to the last whole MiB before the disk's end.
const TARGET: u64 = 31_830_016;
const BLOCK: u64 = 4096;
const EBUSY: i32 = 16;
const EINVAL: i32 = 22;
const ENOSPC: i32 = 28;

/// A scratch directory under TMPDIR, removed when the test ends, whether it passed or panicked.
struct Scratch(std::path::PathBuf);

impl std::ops::Deref for Scratch {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir(name: &str) -> Scratch {
    let d = std::env::temp_dir().join(format!("lemnos-grow-{}-{name}", std::process::id()));
    // Whatever is at the name is not ours to keep: a file left there would stop the directory.
    if fs::symlink_metadata(&d).is_ok_and(|m| m.is_dir()) {
        let _ = fs::remove_dir_all(&d);
    } else {
        let _ = fs::remove_file(&d);
    }
    fs::create_dir_all(&d).unwrap();
    Scratch(d)
}

fn entry(kind: u8, start: u32, sectors: u32) -> [u8; 16] {
    let mut e = [0u8; 16];
    e[1..4].copy_from_slice(&[0xfe, 0xff, 0xff]);
    e[4] = kind;
    e[5..8].copy_from_slice(&[0xfe, 0xff, 0xff]);
    e[8..12].copy_from_slice(&start.to_le_bytes());
    e[12..16].copy_from_slice(&sectors.to_le_bytes());
    e
}

/// An image standing for the disk: p1 (FAT), p2 (`kind`, `p2_sectors` long, labelled `label`, with
/// an ext4 superblock sized to the partition, or none), an optional third entry, and a signature
/// unless `signed` is false.
#[allow(clippy::too_many_arguments)]
fn make_custom(
    path: &Path,
    disk: u64,
    p2_sectors: u32,
    kind: u8,
    third: bool,
    label: &str,
    magic: bool,
    signed: bool,
) -> PathBuf {
    let f = File::create(path).unwrap();
    f.set_len(disk * SECT).unwrap();
    let mut s = [0u8; 512];
    s[446..462].copy_from_slice(&entry(0x0c, P1_START, P1_SECTORS));
    s[462..478].copy_from_slice(&entry(kind, P2_START, p2_sectors));
    if third {
        s[478..494].copy_from_slice(&entry(0x83, P2_START + p2_sectors, 2048));
    }
    if signed {
        s[510] = 0x55;
        s[511] = 0xAA;
    }
    f.write_all_at(&s, 0).unwrap();
    let mut sb = [0u8; 1024];
    if magic {
        sb[0x38..0x3A].copy_from_slice(&0xEF53u16.to_le_bytes());
    }
    sb[0x18..0x1C].copy_from_slice(&2u32.to_le_bytes());
    let blocks = (u64::from(p2_sectors) * SECT / BLOCK) as u32;
    sb[0x04..0x08].copy_from_slice(&blocks.to_le_bytes());
    sb[0x78..0x78 + label.len()].copy_from_slice(label.as_bytes());
    f.write_all_at(&sb, u64::from(P2_START) * SECT + 1024)
        .unwrap();
    path.to_path_buf()
}

fn make(path: &Path) -> PathBuf {
    make_custom(path, DISK, P2_OLD, 0x83, false, LABEL, true, true)
}

fn sector0_of(path: &Path) -> [u8; 512] {
    let mut s = [0u8; 512];
    File::open(path).unwrap().read_exact_at(&mut s, 0).unwrap();
    s
}

/// The first MiB (the table and the boot partition's start) and the start of partition 2 with its
/// superblock. What a grow may change lies in these.
fn snapshot(path: &Path) -> Vec<u8> {
    let f = File::open(path).unwrap();
    let mut out = vec![0u8; 1 << 20];
    f.read_exact_at(&mut out, 0).unwrap();
    let mut p2 = vec![0u8; 4096];
    f.read_exact_at(&mut p2, u64::from(P2_START) * SECT)
        .unwrap();
    out.extend(p2);
    out
}

fn fs_blocks(path: &Path) -> u64 {
    let mut b = [0u8; 4];
    File::open(path)
        .unwrap()
        .read_exact_at(&mut b, u64::from(P2_START) * SECT + 1024 + 4)
        .unwrap();
    u64::from(u32::from_le_bytes(b))
}

fn poke(path: &Path, offset: u64, bytes: &[u8]) {
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .write_all_at(bytes, offset)
        .unwrap();
}

/// The fake kernel and disk over an image file. `boot` is a power-on: the kernel reads the table;
/// `restarted` is a daemon restart with the kernel's view kept.
struct Fake {
    file: PathBuf,
    disk_sectors: u64,
    kernel_start: u64,
    kernel_sectors: u64,
    mutations: usize,
    /// Power is lost before mutation `n` (1-based): it does not happen.
    die_before: Option<usize>,
    /// Power is lost after mutation `n`: it happens, and nothing else does.
    die_after: Option<usize>,
    dead: bool,
    /// The table write lands with its top byte wrong.
    tear: bool,
    /// The partition resize is refused this many times, `EBUSY`.
    blkpg_fails: u32,
    /// The filesystem resize is refused this many times, `ENOSPC`.
    fs_fails: u32,
    /// A mounted ext4 keeps its new block count in the journal: a resize that succeeds leaves the
    /// superblock on the device at the old count until a checkpoint.
    journal_holds_count: bool,
    /// The filesystem's block count as the kernel's view (statfs) has it.
    view_blocks: u64,
    /// The kernel's view never shows a resize (the size it reports stays what it was at boot).
    view_frozen: bool,
    /// The filesystem resize waits on this gate, as a long ioctl does.
    gate: Option<Arc<Gate>>,
    /// The data partition is mounted read-only.
    read_only: bool,
}

/// A gate the fake resize waits on until a test releases it.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    moved: Condvar,
}

impl Gate {
    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.moved.wait(open).unwrap();
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.moved.notify_all();
    }
}

impl Fake {
    fn boot(file: &Path) -> Self {
        let disk_sectors = fs::metadata(file).unwrap().len() / SECT;
        // An unreadable table (no signature) leaves the kernel with no partition: grow refuses it.
        let (kernel_start, kernel_sectors) = kernel_values(file);
        Self {
            file: file.to_path_buf(),
            disk_sectors,
            kernel_start,
            kernel_sectors,
            view_blocks: fs_blocks(file),
            ..Self::idle(file)
        }
    }

    fn restarted(of: &Fake) -> Self {
        Self {
            file: of.file.clone(),
            disk_sectors: of.disk_sectors,
            kernel_start: of.kernel_start,
            kernel_sectors: of.kernel_sectors,
            view_blocks: of.view_blocks,
            view_frozen: of.view_frozen,
            ..Self::idle(&of.file)
        }
    }

    fn idle(file: &Path) -> Self {
        Self {
            file: file.to_path_buf(),
            disk_sectors: 0,
            kernel_start: 0,
            kernel_sectors: 0,
            mutations: 0,
            die_before: None,
            die_after: None,
            dead: false,
            tear: false,
            blkpg_fails: 0,
            fs_fails: 0,
            journal_holds_count: false,
            view_blocks: 0,
            view_frozen: false,
            gate: None,
            read_only: false,
        }
    }

    fn open_rw(&self) -> io::Result<File> {
        OpenOptions::new().read(true).write(true).open(&self.file)
    }

    /// The stored baseline record, beside the image: it survives a power cut and a restart.
    fn record(&self) -> PathBuf {
        self.file.with_extension("grow")
    }

    fn alive(&self) -> io::Result<()> {
        if self.dead {
            Err(io::Error::other("the power is off"))
        } else {
            Ok(())
        }
    }

    fn mutate(&mut self, what: impl FnOnce(&mut Self) -> io::Result<()>) -> io::Result<()> {
        self.alive()?;
        self.mutations += 1;
        let n = self.mutations;
        if self.die_before == Some(n) {
            self.dead = true;
            return Err(io::Error::other("power cut before the write"));
        }
        what(self)?;
        if self.die_after == Some(n) {
            self.dead = true;
            return Err(io::Error::other("power cut after the write"));
        }
        Ok(())
    }
}

impl Disk for Fake {
    fn data_read_only(&self) -> io::Result<bool> {
        Ok(self.read_only)
    }

    fn disk_sectors(&self) -> io::Result<u64> {
        Ok(self.disk_sectors)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.alive()?;
        File::open(&self.file)?.read_exact_at(buf, offset)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let tear = self.tear;
        self.mutate(move |s| {
            let mut b = bytes.to_vec();
            if tear && offset == COUNT_AT {
                b[3] = 0xFF;
            }
            s.open_rw()?.write_all_at(&b, offset)
        })
    }

    fn sync(&mut self) -> io::Result<()> {
        self.alive()
    }

    fn part_start(&self) -> io::Result<u64> {
        self.alive()?;
        Ok(self.kernel_start)
    }

    fn part_sectors(&self) -> io::Result<u64> {
        self.alive()?;
        Ok(self.kernel_sectors)
    }

    fn resize_partition(&mut self, start_bytes: u64, length_bytes: u64) -> io::Result<()> {
        self.alive()?;
        if self.blkpg_fails > 0 {
            self.blkpg_fails -= 1;
            return Err(io::Error::from_raw_os_error(EBUSY));
        }
        let len = length_bytes / SECT;
        self.mutate(move |s| {
            // The kernel never shrinks a partition here, and never runs past the disk.
            if start_bytes / SECT != s.kernel_start
                || len < s.kernel_sectors
                || start_bytes / SECT + len > s.disk_sectors
            {
                return Err(io::Error::from_raw_os_error(EINVAL));
            }
            s.kernel_sectors = len;
            Ok(())
        })
    }

    fn fs_bytes(&self) -> io::Result<u64> {
        self.alive()?;
        Ok(self.view_blocks * BLOCK)
    }

    fn read_record(&self) -> io::Result<Option<Vec<u8>>> {
        self.alive()?;
        match fs::read(self.record()) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write_record(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.alive()?;
        fs::write(self.record(), bytes)
    }

    fn clear_record(&mut self) -> io::Result<()> {
        self.alive()?;
        match fs::remove_file(self.record()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn resize_fs(&mut self, blocks: u64) -> io::Result<()> {
        self.alive()?;
        if let Some(gate) = self.gate.clone() {
            gate.wait();
        }
        if self.fs_fails > 0 {
            self.fs_fails -= 1;
            return Err(io::Error::from_raw_os_error(ENOSPC));
        }
        self.mutate(move |s| {
            // The filesystem cannot grow past its partition.
            if blocks * BLOCK > s.kernel_sectors * SECT {
                return Err(io::Error::from_raw_os_error(ENOSPC));
            }
            if !s.view_frozen {
                s.view_blocks = blocks;
            }
            if s.journal_holds_count {
                return Ok(());
            }
            let at = s.kernel_start * SECT + 1024 + 4;
            s.open_rw()?
                .write_all_at(&(blocks as u32).to_le_bytes(), at)
        })
    }
}

/// The result of a grow with nothing going wrong: the snapshot, the kernel's length and the
/// filesystem's blocks.
fn reference(tag: &str) -> (Vec<u8>, u64, u64) {
    let scratch1 = dir(&format!("reference-{tag}"));
    let img = make(&scratch1.join("disk.img"));
    let mut fake = Fake::boot(&img);
    grow(&mut fake).unwrap();
    (snapshot(&img), fake.kernel_sectors, fs_blocks(&img))
}

#[test]
fn a_last_data_partition_grows_its_table_the_kernel_and_the_filesystem_and_nothing_else() {
    let scratch2 = dir("full");
    let img = make(&scratch2.join("disk.img"));
    let before = snapshot(&img);
    let before_mbr = sector0_of(&img);
    let mut fake = Fake::boot(&img);
    grow(&mut fake).unwrap();
    assert_eq!(
        fake.mutations, 3,
        "the table, the kernel and the filesystem, once each"
    );
    let after_mbr = sector0_of(&img);
    let t = mbr::entries(&after_mbr).unwrap();
    assert_eq!(u64::from(t[ENTRY].sectors), TARGET);
    assert_eq!(fake.kernel_sectors, TARGET);
    assert_eq!(fs_blocks(&img), TARGET * SECT / BLOCK);
    // Only the four bytes of entry 2's sector count changed in sector 0.
    assert_eq!(
        mbr::changed_bytes(&before_mbr, &after_mbr),
        vec![474, 475, 476, 477]
    );
    // In the partition's first 4 KiB only the superblock's block count changed.
    let after = snapshot(&img);
    let changed: Vec<usize> = (0..before.len())
        .filter(|&i| before[i] != after[i])
        .collect();
    let p2 = 1 << 20;
    let mbr_changed: Vec<usize> = changed.iter().copied().filter(|&i| i < p2).collect();
    assert_eq!(
        mbr_changed,
        vec![474, 475, 476, 477],
        "only the entry's count moved in sector 0"
    );
    // In the partition's start only the superblock's block count moved: its low bytes, since the
    // new count's top byte is zero as the old one was.
    let p2_changed: Vec<usize> = changed.iter().copied().filter(|&i| i >= p2).collect();
    assert!(!p2_changed.is_empty());
    assert!(
        p2_changed
            .iter()
            .all(|&i| (p2 + 1024 + 4..p2 + 1024 + 8).contains(&i)),
        "{p2_changed:?} moved outside the block count"
    );
}

#[test]
fn a_grown_disk_is_left_alone_by_the_next_pass() {
    let scratch3 = dir("again");
    let img = make(&scratch3.join("disk.img"));
    grow(&mut Fake::boot(&img)).unwrap();
    let grown = snapshot(&img);
    let mut again = Fake::boot(&img);
    grow(&mut again).unwrap();
    assert_eq!(again.mutations, 0, "already grown: nothing is written");
    assert_eq!(snapshot(&img), grown);
}

#[test]
fn a_power_cut_or_a_restart_at_any_step_is_finished_by_the_next_pass() {
    let (want, want_kernel, want_blocks) = reference("cuts");
    for n in 1..=3 {
        for after in [false, true] {
            for restart in [false, true] {
                let case = format!("cut-{n}-{after}-{restart}");
                let scratch4 = dir(&case);
                let img = make(&scratch4.join("disk.img"));
                let mut cut = Fake::boot(&img);
                if after {
                    cut.die_after = Some(n);
                } else {
                    cut.die_before = Some(n);
                }
                assert!(
                    grow(&mut cut).is_err(),
                    "{case}: the cut must stop the pass"
                );
                // A power cut is a boot: the kernel reads the table. A restart keeps the kernel's view.
                let mut next = if restart {
                    Fake::restarted(&cut)
                } else {
                    Fake::boot(&img)
                };
                grow(&mut next).unwrap_or_else(|why| panic!("{case}: {why}"));
                assert_eq!(
                    snapshot(&img),
                    want,
                    "{case}: the end state is the uninterrupted one"
                );
                assert_eq!(next.kernel_sectors, want_kernel, "{case}");
                assert_eq!(fs_blocks(&img), want_blocks, "{case}");
            }
        }
    }
}

#[test]
fn a_refused_layout_writes_nothing_and_says_why() {
    let root = dir("refused-layout");
    let cases: Vec<(&str, PathBuf, &str)> = vec![
        (
            "not last",
            make_custom(
                &root.join("r-last.img"),
                DISK,
                P2_OLD,
                0x83,
                true,
                LABEL,
                true,
                true,
            ),
            "data partition is not the last one: nothing grown",
        ),
        (
            "type",
            make_custom(
                &root.join("r-type.img"),
                DISK,
                P2_OLD,
                0x0c,
                false,
                LABEL,
                true,
                true,
            ),
            "entry 2 has type 0x0c, not the data partition's 0x83: nothing changed",
        ),
        (
            "label",
            make_custom(
                &root.join("r-label.img"),
                DISK,
                P2_OLD,
                0x83,
                false,
                "other",
                true,
                true,
            ),
            "partition 2 is labelled \"other\", not \"lemnos-data\": nothing changed",
        ),
        (
            "disk smaller",
            make_custom(
                &root.join("r-small.img"),
                u64::from(P2_START) + 50_000,
                P2_OLD,
                0x83,
                false,
                LABEL,
                true,
                true,
            ),
            "smaller than its partition table",
        ),
        (
            "not ext4",
            make_custom(
                &root.join("r-ext4.img"),
                DISK,
                P2_OLD,
                0x83,
                false,
                LABEL,
                false,
                true,
            ),
            "partition 2 is not an ext4 filesystem: nothing changed",
        ),
        (
            "unsigned table",
            make_custom(
                &root.join("r-sig.img"),
                DISK,
                P2_OLD,
                0x83,
                false,
                LABEL,
                true,
                false,
            ),
            "the MBR has no 0x55 0xAA signature: nothing changed",
        ),
    ];
    for (case, img, words) in cases {
        let before = snapshot(&img);
        let mut fake = Fake::boot(&img);
        let why = grow(&mut fake).unwrap_err();
        assert!(why.contains(words), "{case}: {why}");
        assert_eq!(fake.mutations, 0, "{case}");
        assert_eq!(snapshot(&img), before, "{case}: the disk is byte-identical");
    }
}

#[test]
fn a_gain_under_one_mib_writes_nothing() {
    let guard_gain = dir("gain");
    let img = make_custom(
        &guard_gain.join("d.img"),
        u64::from(P2_START) + u64::from(P2_OLD) + 2047,
        P2_OLD,
        0x83,
        false,
        LABEL,
        true,
        true,
    );
    let before = snapshot(&img);
    let mut fake = Fake::boot(&img);
    grow(&mut fake).unwrap();
    assert_eq!(fake.mutations, 0);
    assert_eq!(snapshot(&img), before);
}

#[test]
fn a_kernel_view_longer_than_the_table_is_refused_and_nothing_is_written() {
    let guard_kernel_long = dir("kernel-long");
    let img = make_custom(
        &guard_kernel_long.join("d.img"),
        u64::from(P2_START) + u64::from(P2_OLD) + 2047,
        P2_OLD,
        0x83,
        false,
        LABEL,
        true,
        true,
    );
    let mut fake = Fake::boot(&img);
    fake.kernel_sectors = u64::from(P2_OLD) + 4096;
    let before = snapshot(&img);
    let why = grow(&mut fake).unwrap_err();
    assert!(
        why.contains("smaller than the kernel's view: nothing changed"),
        "{why}"
    );
    assert_eq!(fake.mutations, 0);
    assert_eq!(snapshot(&img), before);
}

#[test]
fn a_filesystem_larger_than_its_partition_is_refused_and_not_shrunk() {
    let scratch5 = dir("fs-long");
    let img = make(&scratch5.join("d.img"));
    poke(
        &img,
        u64::from(P2_START) * SECT + 1024 + 4,
        &4_000_000u32.to_le_bytes(),
    );
    let mut fake = Fake::boot(&img);
    let why = grow(&mut fake).unwrap_err();
    assert!(why.contains("the filesystem has 4000000 blocks"), "{why}");
    assert_eq!(fs_blocks(&img), 4_000_000, "the filesystem is not touched");
}

#[test]
fn a_torn_table_write_is_refused_on_the_next_start_and_only_the_count_bytes_differ() {
    let scratch6 = dir("torn");
    let img = make(&scratch6.join("d.img"));
    let before = sector0_of(&img);
    let mut torn = Fake::boot(&img);
    torn.tear = true;
    let why = grow(&mut torn).unwrap_err();
    assert!(why.contains("is not the one written"), "{why}");
    let after = sector0_of(&img);
    assert!(
        mbr::changed_bytes(&before, &after)
            .iter()
            .all(|i| (474..478).contains(i)),
        "only entry 2's count moved"
    );
    let torn_snapshot = snapshot(&img);
    // The next start reads the value and refuses it: it is past the disk.
    let mut next = Fake::boot(&img);
    let why = grow(&mut next).unwrap_err();
    assert!(why.contains("smaller than its partition table"), "{why}");
    assert_eq!(next.mutations, 0);
    assert_eq!(snapshot(&img), torn_snapshot);
}

#[test]
fn a_new_count_whose_top_byte_is_the_old_one_is_written_and_not_refused() {
    // A disk under 2^24 sectors (8 GB): the count's top byte is 0 before and after, so only three
    // bytes of the sector change. The write is right and must not be refused for that.
    let disk = 8_000_000u64;
    let guard_top_byte = dir("top-byte");
    let img = make_custom(
        &guard_top_byte.join("disk.img"),
        disk,
        P2_OLD,
        0x83,
        false,
        LABEL,
        true,
        true,
    );
    let target = mbr::grow_target(u64::from(P2_START), disk).unwrap();
    assert!(
        target < 1 << 24,
        "the test needs a count with a zero top byte"
    );
    assert_eq!(sector0_of(&img)[477], 0, "the old count's top byte");
    let mut fake = Fake::boot(&img);
    grow(&mut fake).unwrap_or_else(|why| panic!("refused: {why}"));
    assert_eq!(
        fake.mutations, 3,
        "the table, the kernel and the filesystem, once each"
    );
    let t = mbr::entries(&sector0_of(&img)).unwrap();
    assert_eq!(u64::from(t[ENTRY].sectors), target);
}

#[test]
fn a_resize_the_kernel_accepted_is_finished_without_reading_the_superblock_back() {
    // The mounted filesystem's count is not on the device until a checkpoint: the pass must end on
    // the kernel's answer, not on a superblock read that still shows the old count.
    let scratch7 = dir("journal-held");
    let img = make(&scratch7.join("disk.img"));
    let mut fake = Fake::boot(&img);
    fake.journal_holds_count = true;
    let mut task = Confirm::default();
    let pass = grow(&mut fake).unwrap();
    task.confirm(&mut fake, pass)
        .unwrap_or_else(|why| panic!("retried on a stale superblock: {why}"));
    assert_eq!(fake.mutations, 3, "one pass, each step once");
    assert_eq!(fake.kernel_sectors, TARGET);
    assert_eq!(
        fs_blocks(&img),
        100_000 * SECT / BLOCK,
        "the device still holds the old count"
    );
    assert!(task.unconfirmed().is_none());
}

#[test]
fn a_kernel_view_that_never_grows_reaches_grown_unconfirmed_after_the_bound_and_keeps_trying() {
    // The superblock never updates and the kernel's view never shows the resize: the pass must
    // name `grown-unconfirmed` with the sizes it saw after CONFIRM_TRIES checks, and keep trying.
    let scratch8 = dir("never-grows");
    let img = make(&scratch8.join("disk.img"));
    let mut fake = Fake::boot(&img);
    fake.journal_holds_count = true;
    fake.view_frozen = true;
    let old = 100_000 * SECT;
    let mut task = Confirm::default();
    for n in 1..=CONFIRM_TRIES {
        let pass = grow(&mut fake).unwrap();
        assert_eq!(
            pass,
            Pass::Resized {
                baseline: Baseline::Stored(old)
            },
            "the superblock says the old size, so each pass resizes again"
        );
        let why = task.confirm(&mut fake, pass).unwrap_err();
        if n < CONFIRM_TRIES {
            assert!(!why.contains("grown-unconfirmed"), "check {n}: {why}");
            assert!(task.unconfirmed().is_none(), "check {n}");
        } else {
            assert!(why.starts_with("grown-unconfirmed"), "{why}");
            assert!(why.contains(&format!("{old} bytes")), "{why}");
            let u = task.unconfirmed().expect("the state is set at the bound");
            assert_eq!((u.baseline, u.seen), (Some(old), old), "the sizes seen");
        }
    }
    // Still not given up: later passes keep retrying and keep the state.
    for _ in 0..5 {
        let pass = grow(&mut fake).unwrap();
        let why = task.confirm(&mut fake, pass).unwrap_err();
        assert!(why.starts_with("grown-unconfirmed"), "{why}");
    }
    let u = task.unconfirmed().expect("the state is kept");
    assert_eq!((u.baseline, u.seen), (Some(old), old));
}

#[test]
fn a_view_that_shows_the_grow_clears_the_unconfirmed_state() {
    let scratch9 = dir("view-clears");
    let img = make(&scratch9.join("disk.img"));
    let mut fake = Fake::boot(&img);
    fake.journal_holds_count = true;
    fake.view_frozen = true;
    let mut task = Confirm::default();
    for _ in 0..CONFIRM_TRIES {
        let pass = grow(&mut fake).unwrap();
        let _ = task.confirm(&mut fake, pass);
    }
    assert!(task.unconfirmed().is_some());
    fake.view_frozen = false;
    let pass = grow(&mut fake).unwrap();
    task.confirm(&mut fake, pass).unwrap();
    assert!(task.unconfirmed().is_none());
}

#[test]
fn a_driver_restarted_after_the_resize_before_the_confirmation_reads_ok() {
    // The first start resized, and stopped before it confirmed. The view had already grown, and the
    // superblock on the device still shows the old count (the journal holds it). The restarted
    // driver reads the stored baseline and confirms against it: ok, and the record is cleared.
    let old = 100_000 * SECT;
    let scratch10 = dir("restart-after");
    let img = make(&scratch10.join("disk.img"));
    let mut first = Fake::boot(&img);
    first.journal_holds_count = true;
    let pass = grow(&mut first).unwrap();
    assert_eq!(
        pass,
        Pass::Resized {
            baseline: Baseline::Stored(old)
        }
    );
    // The driver stops here, before the confirmation.
    assert_eq!(
        first.view_blocks * BLOCK,
        TARGET * SECT / BLOCK * BLOCK,
        "the view has grown"
    );
    assert!(
        first.record().exists(),
        "the baseline is stored while the resize is unconfirmed"
    );

    let mut restarted = Fake::restarted(&first);
    restarted.journal_holds_count = true;
    let mut task = Confirm::default();
    let pass = grow(&mut restarted).unwrap();
    assert_eq!(
        pass,
        Pass::Resized {
            baseline: Baseline::Stored(old)
        },
        "the restart reads the stored size, not the grown one"
    );
    task.confirm(&mut restarted, pass)
        .unwrap_or_else(|why| panic!("restarted after the resize: {why}"));
    assert!(task.unconfirmed().is_none(), "not grown-unconfirmed");
    assert!(
        !restarted.record().exists(),
        "confirmed: the baseline is cleared"
    );
}

#[test]
fn a_missing_or_corrupt_stored_baseline_reaches_grown_unconfirmed_and_names_it() {
    let cases: [(&str, Option<Vec<u8>>, &str); 2] = [
        ("missing", None, "missing"),
        (
            "corrupt",
            Some(b"lemnos-grow 1\nbaseline 9\n".to_vec()),
            "corrupt",
        ),
    ];
    for (case, damage, words) in cases {
        let scratch11 = dir(&format!("baseline-{case}"));
        let img = make(&scratch11.join("disk.img"));
        let mut first = Fake::boot(&img);
        first.journal_holds_count = true;
        grow(&mut first).unwrap_or_else(|why| panic!("{case}: {why}"));
        match damage {
            None => fs::remove_file(first.record()).unwrap(),
            Some(bytes) => fs::write(first.record(), bytes).unwrap(),
        }
        let mut restarted = Fake::restarted(&first);
        restarted.journal_holds_count = true;
        let mut task = Confirm::default();
        let pass = grow(&mut restarted).unwrap_or_else(|why| panic!("{case}: {why}"));
        let why = task
            .confirm(&mut restarted, pass)
            .expect_err("a resize with no usable baseline is not confirmed");
        assert!(why.starts_with("grown-unconfirmed"), "{case}: {why}");
        assert!(why.contains(words), "{case} must be named: {why}");
        let u = task.unconfirmed().expect("the state is set at once");
        assert_eq!(u.baseline, None, "{case}");
        assert_eq!(u.reason, why, "{case}: the reason is the state's");
    }
}

#[test]
fn a_record_round_trips_and_a_damaged_one_does_not_decode() {
    let bytes = encode_record(51_200_000, 31_830_016);
    assert_eq!(decode_record(&bytes), Some((51_200_000, 31_830_016)));
    let mut flipped = bytes.clone();
    let at = flipped.iter().position(|b| *b == b'2').unwrap();
    flipped[at] = b'3';
    assert_eq!(
        decode_record(&flipped),
        None,
        "a changed digit fails the checksum"
    );
    assert_eq!(
        decode_record(&bytes[..bytes.len() - 3]),
        None,
        "a torn record"
    );
    assert_eq!(decode_record(b""), None);
}

#[test]
fn a_refused_partition_resize_is_finished_by_the_next_pass() {
    let (want, want_kernel, want_blocks) = reference("busy");
    let scratch12 = dir("busy");
    let img = make(&scratch12.join("d.img"));
    let mut busy = Fake::boot(&img);
    busy.blkpg_fails = 1;
    let why = grow(&mut busy).unwrap_err();
    assert!(
        why.contains("the kernel refused to resize partition 2"),
        "{why}"
    );
    let mut next = Fake::restarted(&busy);
    grow(&mut next).unwrap();
    assert_eq!(snapshot(&img), want);
    assert_eq!(next.kernel_sectors, want_kernel);
    assert_eq!(fs_blocks(&img), want_blocks);
}

#[test]
fn a_filesystem_that_cannot_grow_yet_is_finished_by_a_later_pass() {
    let (want, _, want_blocks) = reference("enospc");
    let scratch13 = dir("enospc");
    let img = make(&scratch13.join("d.img"));
    let mut full = Fake::boot(&img);
    full.fs_fails = 1;
    let why = grow(&mut full).unwrap_err();
    assert!(why.contains("the filesystem refused to grow"), "{why}");
    let mut next = Fake::restarted(&full);
    grow(&mut next).unwrap();
    assert_eq!(snapshot(&img), want);
    assert_eq!(fs_blocks(&img), want_blocks);
}

#[test]
fn a_failed_step_is_retried_with_a_backoff_capped_at_eight_seconds_and_never_given_up() {
    let mut retry = Retry::new();
    let mut now = 0u64;
    let mut waits = Vec::new();
    for _ in 0..20 {
        let next = retry.failed(now, "busy");
        waits.push((next - now) / 1_000_000_000);
        now = next;
    }
    assert_eq!(&waits[..6], &[1, 2, 4, 8, 8, 8]);
    assert!(waits.iter().all(|w| *w <= 8));
    assert_eq!(waits.len(), 20, "every failure gets another try");
    assert_eq!(retry.last, "busy");
    assert_eq!(next_backoff(Duration::from_secs(8)), Duration::from_secs(8));
}

#[cfg(feature = "linux")]
#[test]
fn the_partition_two_node_follows_the_kernels_naming() {
    assert_eq!(partition_name("mmcblk0"), "mmcblk0p2");
    assert_eq!(partition_name("nvme0n1"), "nvme0n1p2");
    assert_eq!(partition_name("sda"), "sda2");
    let mmc = Real::new(Path::new("/dev/mmcblk0"), Path::new("/data"));
    assert_eq!(mmc.sys_part, PathBuf::from("/sys/class/block/mmcblk0p2"));
    assert_eq!(mmc.sys_disk, PathBuf::from("/sys/block/mmcblk0"));
}

fn make_disk_bytes(name: &str, bytes: u64) -> (Scratch, PathBuf) {
    let guard = dir(name);
    let img = make_custom(
        &guard.join("disk.img"),
        bytes / SECT,
        P2_OLD,
        0x83,
        false,
        LABEL,
        true,
        true,
    );
    (guard, img)
}

#[test]
fn a_grow_to_more_than_256_gib_is_refused_by_name_and_nothing_is_written() {
    let (_dir, img) = make_disk_bytes("too-large", 2 << 40);
    let before = snapshot(&img);
    let before_mbr = sector0_of(&img);
    let mut fake = Fake::boot(&img);
    let pass = grow(&mut fake).unwrap();
    let Pass::Refused(refusal) = pass else {
        panic!("a 2 TiB disk must be refused, got {pass:?}");
    };
    assert!(
        refusal.reason.starts_with("grow-too-large:"),
        "{}",
        refusal.reason
    );
    let (target, max) = refusal.sizes.expect("the sizes are named");
    assert!(target > max);
    assert_eq!(max, 256 << 30);
    assert_eq!(refusal.state, "grow-too-large");
    assert!(refusal.reason.contains(&target.to_string()));
    assert!(refusal.reason.contains(&max.to_string()));
    assert_eq!(fake.mutations, 0, "a refused grow writes nothing");
    assert_eq!(sector0_of(&img), before_mbr);
    assert_eq!(snapshot(&img), before);
}

#[test]
fn a_grow_to_256_gib_or_less_is_allowed() {
    let (_dir, img) = make_disk_bytes("at-the-cap", 256 << 30);
    let mut fake = Fake::boot(&img);
    assert!(matches!(grow(&mut fake).unwrap(), Pass::Resized { .. }));
    let t = mbr::entries(&sector0_of(&img)).unwrap();
    assert!(u64::from(t[ENTRY].sectors) * SECT <= 256 << 30);
    assert_eq!(fs_blocks(&img), u64::from(t[ENTRY].sectors) * SECT / BLOCK);
}

fn kernel_values(file: &Path) -> (u64, u64) {
    let mut second = [0u8; 512];
    File::open(file)
        .unwrap()
        .read_exact_at(&mut second, 512)
        .unwrap();
    if &second[..8] == b"EFI PART" {
        let disk = fs::metadata(file).unwrap().len() / SECT;
        // A damaged primary leaves the kernel with no partition (the backup is not read here).
        let Ok(header) = gpt::parse_header(&second, 1) else {
            return (0, 0);
        };
        let mut array = vec![0u8; gpt::ARRAY_BYTES];
        File::open(file)
            .unwrap()
            .read_exact_at(&mut array, header.array_lba * SECT)
            .unwrap();
        let (first, last, _) = gpt::entry(&array, ENTRY);
        let _ = disk;
        return (first, last - first + 1);
    }
    match mbr::entries(&sector0_of(file)) {
        Ok(t) => (u64::from(t[ENTRY].start), u64::from(t[ENTRY].sectors)),
        Err(_) => (0, 0),
    }
}

use super::gpt;

/// A GPT disk of `disk` sectors: a protective MBR, partition 1 (type GUID [1; 16]), partition 2 (the
/// data partition, `[2; 16]`, `p2_sectors` long, with an ext4 superblock labelled `lemnos-data`), the
/// primary and backup headers and arrays with their CRCs.
fn make_gpt_disk(path: &Path, disk: u64, p2_sectors: u64) -> PathBuf {
    let f = File::create(path).unwrap();
    f.set_len(disk * SECT).unwrap();
    let mut mbr_sector = [0u8; 512];
    mbr_sector[446 + 4] = 0xEE;
    mbr_sector[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    mbr_sector[446 + 12..446 + 16]
        .copy_from_slice(&((disk - 1).min(0xFFFF_FFFF) as u32).to_le_bytes());
    mbr_sector[510] = 0x55;
    mbr_sector[511] = 0xAA;
    f.write_all_at(&mbr_sector, 0).unwrap();
    let mut array = vec![0u8; gpt::ARRAY_BYTES];
    let put_entry = |array: &mut [u8], index: usize, guid: [u8; 16], first: u64, last: u64| {
        let at = index * 128;
        array[at..at + 16].copy_from_slice(&guid);
        array[at + 16..at + 32].copy_from_slice(&[3; 16]);
        array[at + 32..at + 40].copy_from_slice(&first.to_le_bytes());
        array[at + 40..at + 48].copy_from_slice(&last.to_le_bytes());
    };
    put_entry(
        &mut array,
        0,
        [1; 16],
        u64::from(P1_START),
        u64::from(P1_START) + u64::from(P1_SECTORS) - 1,
    );
    let p2_start = u64::from(P2_START);
    put_entry(&mut array, 1, [2; 16], p2_start, p2_start + p2_sectors - 1);
    let crc = gpt::array_crc(&array);
    let base = gpt::Header {
        my_lba: 1,
        alt_lba: disk - 1,
        first_usable: 34,
        last_usable: disk - 34,
        disk_guid: [9; 16],
        array_lba: 2,
        array_crc: crc,
    };
    f.write_all_at(&array, 2 * SECT).unwrap();
    f.write_all_at(&gpt::encode_header(&base), SECT).unwrap();
    let backup_array = disk - 1 - gpt::ARRAY_SECTORS;
    f.write_all_at(&array, backup_array * SECT).unwrap();
    let backup = gpt::Header {
        my_lba: disk - 1,
        alt_lba: 1,
        array_lba: backup_array,
        ..base
    };
    f.write_all_at(&gpt::encode_header(&backup), (disk - 1) * SECT)
        .unwrap();
    let mut sb = [0u8; 1024];
    sb[0x38..0x3A].copy_from_slice(&0xEF53u16.to_le_bytes());
    sb[0x18..0x1C].copy_from_slice(&2u32.to_le_bytes());
    let blocks = (p2_sectors * SECT / BLOCK) as u32;
    sb[0x04..0x08].copy_from_slice(&blocks.to_le_bytes());
    sb[0x78..0x78 + LABEL.len()].copy_from_slice(LABEL.as_bytes());
    f.write_all_at(&sb, p2_start * SECT + 1024).unwrap();
    path.to_path_buf()
}

/// Reads one copy of the table from the image: the header at `lba` and its array. `None` when it
/// does not parse or fails its CRC.
fn read_table_copy(path: &Path, lba: u64) -> Option<(gpt::Header, Vec<u8>)> {
    let f = File::open(path).unwrap();
    let mut sector = [0u8; 512];
    f.read_exact_at(&mut sector, lba * SECT).ok()?;
    let disk = f.metadata().unwrap().len() / SECT;
    let expect = if lba == 1 { 1 } else { disk - 1 };
    let header = gpt::parse_header(&sector, expect).ok()?;
    let mut array = vec![0u8; gpt::ARRAY_BYTES];
    f.read_exact_at(&mut array, header.array_lba * SECT).ok()?;
    gpt::check_array(&array, &header).ok()?;
    Some((header, array))
}

/// The table as a reader sees it: the primary if it is good, else the backup. `None` when neither
/// is: that is the state a grow must never leave.
fn readable_table(path: &Path) -> Option<(gpt::Header, Vec<u8>)> {
    read_table_copy(path, 1)
        .or_else(|| read_table_copy(path, path.metadata().unwrap().len() / SECT - 1))
}

/// The partition 2 end the readable table gives.
fn table_end(path: &Path) -> Option<u64> {
    readable_table(path).map(|(_, array)| gpt::entry(&array, ENTRY).1)
}

/// The old and new partition 2 ends on a disk of `disk` sectors, with partition 2 at its start.
fn ends(disk: u64) -> (u64, u64) {
    let old = u64::from(P2_START) + P2_OLD_GPT - 1;
    let new = gpt::target_end(disk);
    (old, new)
}

const P2_OLD_GPT: u64 = 100_000;

/// A GPT disk of [`DISK`] sectors, grown in place: its backup is at the end and the old one sits
/// where the image's end was (extended by a file set_len, see [`make_gpt_extended`]).
fn make_gpt_extended(name: &str, old_disk: u64, new_disk: u64) -> (Scratch, PathBuf) {
    let scratch17 = dir(name);
    let img = make_gpt_disk(&scratch17.join("disk.img"), old_disk, P2_OLD_GPT);
    let f = OpenOptions::new().write(true).open(&img).unwrap();
    f.set_len(new_disk * SECT).unwrap();
    (scratch17, img)
}

#[test]
fn a_gpt_last_partition_grows_and_both_copies_move_to_the_new_end() {
    let disk = 2 * DISK;
    let (_dir, img) = make_gpt_extended("gpt-full", DISK, disk);
    let (_, new_end) = ends(disk);
    let mut fake = Fake::boot(&img);
    assert!(matches!(grow(&mut fake).unwrap(), Pass::Resized { .. }));
    let (primary, array) = read_table_copy(&img, 1).expect("the primary reads");
    assert_eq!(primary.alt_lba, disk - 1);
    assert_eq!(primary.last_usable, disk - 34);
    assert_eq!(gpt::entry(&array, ENTRY).1, new_end);
    let (backup, backup_array) = read_table_copy(&img, disk - 1).expect("the backup reads");
    assert_eq!(backup.my_lba, disk - 1);
    assert_eq!(backup.alt_lba, 1);
    assert_eq!(backup_array, array, "the backup array is the primary's");
    assert_eq!(fake.kernel_sectors, new_end + 1 - u64::from(P2_START));
    assert_eq!(fs_blocks(&img), fake.kernel_sectors * SECT / BLOCK);
}

#[test]
fn a_stale_backup_gpt_past_the_old_end_is_zeroed_before_the_grow_writes() {
    let disk = 2 * DISK;
    let (_dir, img) = make_gpt_extended("gpt-stale", DISK, disk);
    let old_backup = DISK - 1;
    let mut before = [0u8; 512];
    File::open(&img)
        .unwrap()
        .read_exact_at(&mut before, old_backup * SECT)
        .unwrap();
    assert!(
        gpt::parse_header(&before, old_backup).is_ok(),
        "the old backup is there before the grow"
    );
    let mut fake = Fake::boot(&img);
    grow(&mut fake).unwrap();
    let mut sector = [0u8; 512];
    File::open(&img)
        .unwrap()
        .read_exact_at(&mut sector, old_backup * SECT)
        .unwrap();
    // Inside the grown partition now: the stale header is gone, the sector is the data's (zero).
    assert!(
        sector.iter().all(|b| *b == 0),
        "the stale backup header is zeroed"
    );
    assert!(read_table_copy(&img, disk - 1).is_some());
}

#[test]
fn a_gpt_grow_cut_after_any_write_reads_as_the_old_table_or_the_new_one() {
    let disk = 2 * DISK;
    let (old_end, new_end) = ends(disk);
    for cut in 1..=12usize {
        let (_dir, img) = make_gpt_extended(&format!("gpt-cut-{cut}"), DISK, disk);
        let mut fake = Fake::boot(&img);
        fake.die_after = Some(cut);
        let _ = grow(&mut fake);
        let end =
            table_end(&img).unwrap_or_else(|| panic!("cut after write {cut}: neither table reads"));
        assert!(
            end == old_end || end == new_end,
            "cut after write {cut}: the table ends at {end}, not {old_end} or {new_end}"
        );
        // The restarted driver finishes the grow from whichever table it finds.
        let mut again = Fake::restarted(&fake);
        for _ in 0..4 {
            match grow(&mut again).unwrap() {
                Pass::Done => break,
                Pass::Resized { .. } => continue,
                Pass::Refused(r) => panic!("cut {cut}: refused on restart: {}", r.reason),
            }
        }
        assert_eq!(
            table_end(&img),
            Some(new_end),
            "cut after write {cut}: the restart finishes the table"
        );
        assert_eq!(
            fs_blocks(&img),
            (new_end + 1 - u64::from(P2_START)) * SECT / BLOCK,
            "cut {cut}: the filesystem follows"
        );
    }
}

#[test]
fn a_damaged_primary_is_repaired_from_the_backup_when_the_table_is_already_grown() {
    let disk = 2 * DISK;
    let (_dir, img) = make_gpt_extended("gpt-repair", DISK, disk);
    let mut fake = Fake::boot(&img);
    grow(&mut fake).unwrap();
    poke(&img, SECT + 60, &[0xff]); // the primary's header CRC
    assert!(read_table_copy(&img, 1).is_none(), "the primary is damaged");
    let mut again = Fake::restarted(&fake);
    assert!(matches!(
        grow(&mut again).unwrap(),
        Pass::Done | Pass::Resized { .. }
    ));
    assert!(
        read_table_copy(&img, 1).is_some(),
        "the primary reads again"
    );
}

#[test]
fn a_gpt_with_no_readable_copy_is_the_named_unavailable_state_and_nothing_is_written() {
    let (_dir, img) = make_gpt_extended("gpt-dead", DISK, 2 * DISK);
    poke(&img, SECT + 60, &[0xff]);
    poke(&img, (2 * DISK - 1) * SECT + 60, &[0xff]);
    let before = snapshot(&img);
    let mut fake = Fake::boot(&img);
    let Pass::Refused(refusal) = grow(&mut fake).unwrap() else {
        panic!("a GPT with no readable copy is refused");
    };
    assert_eq!(refusal.state, "gpt-unavailable");
    assert_eq!(fake.mutations, 0);
    assert_eq!(snapshot(&img), before);
}

#[test]
fn a_gpt_partition_two_that_is_not_the_last_is_the_named_unavailable_state() {
    let (_dir, img) = make_gpt_extended("gpt-not-last", DISK, 2 * DISK);
    // A used entry 3 starting after partition 2: rewrite the primary array and its header.
    let (mut primary, mut array) = read_table_copy(&img, 1).unwrap();
    let at = 2 * 128;
    array[at..at + 16].copy_from_slice(&[4; 16]);
    array[at + 32..at + 40].copy_from_slice(&(u64::from(P2_START) + P2_OLD_GPT + 10).to_le_bytes());
    array[at + 40..at + 48].copy_from_slice(&(u64::from(P2_START) + P2_OLD_GPT + 20).to_le_bytes());
    primary.array_crc = gpt::array_crc(&array);
    let f = OpenOptions::new().write(true).open(&img).unwrap();
    f.write_all_at(&array, 2 * SECT).unwrap();
    f.write_all_at(&gpt::encode_header(&primary), SECT).unwrap();
    drop(f);
    let mut fake = Fake::boot(&img);
    let Pass::Refused(refusal) = grow(&mut fake).unwrap() else {
        panic!("a later partition is refused");
    };
    assert_eq!(refusal.state, "gpt-unavailable");
    assert!(
        refusal.reason.contains("must be the last one"),
        "{}",
        refusal.reason
    );
    assert_eq!(fake.mutations, 0);
}

#[test]
fn a_read_only_data_partition_is_a_named_state_and_nothing_is_written() {
    let scratch18 = dir("read-only");
    let img = make(&scratch18.join("disk.img"));
    let before = snapshot(&img);
    let mut fake = Fake::boot(&img);
    fake.read_only = true;
    let Pass::Refused(refusal) = grow(&mut fake).unwrap() else {
        panic!("a read-only data partition is refused");
    };
    assert_eq!(refusal.state, "data-read-only");
    assert!(refusal.sizes.is_none());
    assert_eq!(fake.mutations, 0);
    assert_eq!(snapshot(&img), before);
}

#[test]
fn a_state_round_trips_through_its_file_form_and_a_damaged_one_does_not_decode() {
    let states = [
        State::Running {
            boot_id: "b1".into(),
            started_ms: 5,
            bound_ms: 40_000,
            target_bytes: 1 << 35,
        },
        State::NotNeeded {
            reason: String::new(),
        },
        State::NotNeeded {
            reason: "off in the board file".into(),
        },
        State::Grown,
        State::Refused {
            state: "data-read-only".into(),
            reason: "data-read-only: /data is mounted read-only".into(),
        },
        State::Failed {
            reason: "busy, twice".into(),
        },
        State::Unconfirmed {
            reason: "grown-unconfirmed: never shown".into(),
            baseline: None,
            seen: 9,
        },
        State::Unconfirmed {
            reason: "grown-unconfirmed: short".into(),
            baseline: Some(7),
            seen: 8,
        },
    ];
    for state in states {
        assert_eq!(decode_state(&encode_state(&state)), Some(state));
    }
    assert_eq!(decode_state("bogus\n"), None);
    assert_eq!(
        decode_state("attempt started b1 1 2\n"),
        None,
        "an attempt names four fields"
    );
    assert_eq!(decode_state(""), None);
}

#[test]
fn a_boot_grow_reports_running_then_grown_and_the_disk_is_grown() {
    let scratch = dir("boot-grow");
    let img = make(&scratch.join("disk.img"));
    let reports = std::cell::RefCell::new(Vec::new());
    let state = run_boot(
        "b1",
        &|| Ok(Box::new(Fake::boot(&img)) as Box<dyn Disk + Send>),
        &|d| std::thread::sleep(d.min(Duration::from_millis(2))),
        &|| 0,
        &|s| reports.borrow_mut().push(s.clone()),
    );
    assert_eq!(state, Some(State::Grown));
    let reports = reports.into_inner();
    assert!(
        matches!(reports.first(), Some(State::Running { .. })),
        "the first report is the pass in flight: {reports:?}"
    );
    assert_eq!(reports.last(), Some(&State::Grown));
    let table = mbr::entries(&sector0_of(&img)).unwrap();
    assert_eq!(
        u64::from(table[ENTRY].sectors),
        TARGET,
        "the table holds the new length"
    );
}

#[test]
fn a_read_only_data_partition_is_the_refused_boot_state_and_nothing_is_written() {
    let scratch = dir("boot-ro");
    let img = make(&scratch.join("disk.img"));
    let before = snapshot(&img);
    let state = run_boot(
        "b1",
        &|| {
            let mut fake = Fake::boot(&img);
            fake.read_only = true;
            Ok(Box::new(fake) as Box<dyn Disk + Send>)
        },
        &|d| std::thread::sleep(d.min(Duration::from_millis(2))),
        &|| 0,
        &|_| {},
    );
    let Some(State::Refused { state, reason }) = state else {
        panic!("a read-only data partition is refused, not retried");
    };
    assert_eq!(state, "data-read-only");
    assert!(reason.starts_with("data-read-only"), "{reason}");
    assert_eq!(snapshot(&img), before);
}

#[test]
fn a_disk_that_cannot_be_opened_is_a_failed_boot_state_and_the_boot_goes_on() {
    let state = run_boot("b1", &|| Err("no node".into()), &|_| {}, &|| 0, &|_| {});
    assert_eq!(
        state,
        Some(State::Failed {
            reason: "no node".into()
        })
    );
}

#[test]
fn a_call_that_never_returns_stalls_the_boot_at_the_hard_bound_and_leaves_its_marker_first() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let scratch = dir("stall");
    let img = make(&scratch.join("disk.img"));
    // The stand-in call blocks on the gate until this test releases it, as a kernel call that never
    // returns would.
    let gate = Arc::new(Gate::default());
    let clock_ms = AtomicU64::new(0);
    let fed_ms = AtomicU64::new(0);
    let reports = std::cell::RefCell::new(Vec::new());
    let state = run_boot(
        "boot-a",
        &|| {
            let mut fake = Fake::boot(&img);
            fake.gate = Some(gate.clone());
            Ok(Box::new(fake) as Box<dyn Disk + Send>)
        },
        &|time| {
            fed_ms.fetch_add(time.as_millis() as u64, Ordering::SeqCst);
            clock_ms.fetch_add(time.as_millis() as u64, Ordering::SeqCst);
        },
        &|| clock_ms.load(Ordering::SeqCst),
        &|s| reports.borrow_mut().push(s.clone()),
    );
    gate.release();
    assert!(
        state.is_none(),
        "a call past the hard bound is a stall: nothing more is fed"
    );
    let hard_ms = (overrun_bound(DISK * SECT) + STALL_MARGIN).as_millis() as u64;
    let fed = fed_ms.load(Ordering::SeqCst);
    assert!(
        fed <= hard_ms,
        "fed {fed} ms, past the hard bound {hard_ms} ms"
    );
    assert!(
        fed + 200 >= hard_ms,
        "fed up to the hard bound: {fed} of {hard_ms} ms"
    );
    let reports = reports.into_inner();
    assert!(
        matches!(reports.first(), Some(State::Running { boot_id, .. }) if boot_id == "boot-a"),
        "the attempt marker is written before the first write: {reports:?}"
    );
}
