//! The grow on a file that stands for the disk, with the kernel's side (the sizes, the partition
//! resize and the filesystem resize) as a fake that can refuse, lose power at any step, or tear the
//! table write. The file's bytes are the disk's: every assertion on what changed reads them.
//!
//! What the fake does not share with the kernel: the kernel's own checks on a mounted partition
//! (`BLKPG` while `/data` is open), the page cache, and the device's atomicity for a sector write.
//! Those are the board check (`testing/board-runs`), not proved here.

use super::*;
use std::fs::OpenOptions;
use std::os::unix::fs::FileExt;

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

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lemnos-grow-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
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
}

impl Fake {
    fn boot(file: &Path) -> Self {
        let disk_sectors = fs::metadata(file).unwrap().len() / SECT;
        // An unreadable table (no signature) leaves the kernel with no partition: grow refuses it.
        let (kernel_start, kernel_sectors) = match mbr::entries(&sector0_of(file)) {
            Ok(t) => (u64::from(t[ENTRY].start), u64::from(t[ENTRY].sectors)),
            Err(_) => (0, 0),
        };
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
    let img = make(&dir(&format!("reference-{tag}")).join("disk.img"));
    let mut fake = Fake::boot(&img);
    grow(&mut fake).unwrap();
    (snapshot(&img), fake.kernel_sectors, fs_blocks(&img))
}

#[test]
fn a_last_data_partition_grows_its_table_the_kernel_and_the_filesystem_and_nothing_else() {
    let img = make(&dir("full").join("disk.img"));
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
    let img = make(&dir("again").join("disk.img"));
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
                let img = make(&dir(&case).join("disk.img"));
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
    let cases: Vec<(&str, PathBuf, &str)> = vec![
        (
            "not last",
            make_custom(
                &dir("r-last").join("d.img"),
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
                &dir("r-type").join("d.img"),
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
                &dir("r-label").join("d.img"),
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
                &dir("r-small").join("d.img"),
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
                &dir("r-ext4").join("d.img"),
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
                &dir("r-sig").join("d.img"),
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
    let img = make_custom(
        &dir("gain").join("d.img"),
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
    let img = make_custom(
        &dir("kernel-long").join("d.img"),
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
    let img = make(&dir("fs-long").join("d.img"));
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
    let img = make(&dir("torn").join("d.img"));
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
    let img = make_custom(
        &dir("top-byte").join("disk.img"),
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
    let img = make(&dir("journal-held").join("disk.img"));
    let mut fake = Fake::boot(&img);
    fake.journal_holds_count = true;
    let mut task = Task::new(Err("no disk".into()), PathBuf::from("/data"));
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
    let img = make(&dir("never-grows").join("disk.img"));
    let mut fake = Fake::boot(&img);
    fake.journal_holds_count = true;
    fake.view_frozen = true;
    let old = 100_000 * SECT;
    let mut task = Task::new(Err("no disk".into()), PathBuf::from("/data"));
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
    let img = make(&dir("view-clears").join("disk.img"));
    let mut fake = Fake::boot(&img);
    fake.journal_holds_count = true;
    fake.view_frozen = true;
    let mut task = Task::new(Err("no disk".into()), PathBuf::from("/data"));
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
    let img = make(&dir("restart-after").join("disk.img"));
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
    let mut task = Task::new(Err("no disk".into()), PathBuf::from("/data"));
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
        let img = make(&dir(&format!("baseline-{case}")).join("disk.img"));
        let mut first = Fake::boot(&img);
        first.journal_holds_count = true;
        grow(&mut first).unwrap_or_else(|why| panic!("{case}: {why}"));
        match damage {
            None => fs::remove_file(first.record()).unwrap(),
            Some(bytes) => fs::write(first.record(), bytes).unwrap(),
        }
        let mut restarted = Fake::restarted(&first);
        restarted.journal_holds_count = true;
        let mut task = Task::new(Err("no disk".into()), PathBuf::from("/data"));
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
    let img = make(&dir("busy").join("d.img"));
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
    let img = make(&dir("enospc").join("d.img"));
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

#[test]
fn a_boot_disk_that_is_not_known_is_reported_in_words_on_each_try() {
    let mut task = Task::new(
        Err("the boot disk is not known".into()),
        PathBuf::from("/data"),
    );
    assert_eq!(task.attempt().unwrap_err(), "the boot disk is not known");
}

#[test]
fn the_grow_is_on_by_default_and_a_board_can_opt_out() {
    assert!(enabled(None), "unset: on");
    assert!(enabled(Some(true)));
    assert!(!enabled(Some(false)), "grow_data = false opts out");
}

#[test]
fn the_partition_two_node_follows_the_kernels_naming() {
    assert_eq!(partition_name("mmcblk0"), "mmcblk0p2");
    assert_eq!(partition_name("nvme0n1"), "nvme0n1p2");
    assert_eq!(partition_name("sda"), "sda2");
    let mmc = Real::new(Path::new("/dev/mmcblk0"), Path::new("/data"));
    assert_eq!(mmc.sys_part, PathBuf::from("/sys/class/block/mmcblk0p2"));
    assert_eq!(mmc.sys_disk, PathBuf::from("/sys/block/mmcblk0"));
}
