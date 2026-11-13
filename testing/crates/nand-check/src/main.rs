//! Board-check in the QEMU harness: the first-flash NAND writer against the kernel's own NAND
//! simulator (`nandsim`), its MTD layer and its UBI.
//!
//! The harness loads `nandsim` with the chip below (testing/qemu/run.sh): six partitions, bad blocks
//! the kernel's fault model injects, a weak erase block, and bit flips. Every partition is one
//! `/dev/mtdN`, and the stages run in this order:
//!
//! 1. Geometry and the chip's own answers: sysfs and `MEMGETINFO` agree, `MEMGETBADBLOCK` gives the
//!    injected bad blocks and no others.
//! 2. UBI on partition A: the kernel attaches the blank chip (formatting it), a volume is created and
//!    filled through its volume node, and the device is detached. Then A's good erase blocks are read
//!    back through MTD, bad blocks skipped, into an image file.
//! 3. The driver's writer (`nand::Plan`, `nand::Sink`, the code a `system.flash/1` job runs for an
//!    `mtd` target) writes that UBI image to partition B, which has bad blocks; the image is checked
//!    by the driver's own verifier, B's erase counters are compared with the carry-forward rule, the
//!    bad blocks are left alone, and the kernel attaches B and reads the volume back.
//! 4. An image that does not fit once bad blocks are subtracted (partition D) is refused before any
//!    erase: D's bytes are the same afterwards.
//! 5. A raw image on partition E, read back; `MEMERASE` on a good block.
//! 6. ECC: the injected bit flips are corrected and counted by the kernel, and the driver reports the
//!    same count.
//! 7. A write whose erase fails (partition F, a weak block): without `mark_bad` the block stays good;
//!    with it, `MEMSETBADBLOCK` marks it and the kernel reports it bad afterwards.
//!
//! `--corrupt` is the negative run: one byte of the UBI image is changed before the writer sees it, so
//! the volume read in stage 3 must fail. The unsafe requests are in `lemnos-driver-system-flash-sys`.
//!
//! Output: one `ok` or `FAIL` line per assertion, then `lemnos-nand-check: passed (0 failed)` or
//! `failed (N failed)`; the exit status is 0 only when every assertion held.

use std::fs::{self, File, OpenOptions};
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::process::ExitCode;

use lemnos_driver_system_flash::nand::{self, Chip, Device, Geometry, Image, Plan, Sink, Verifier};
use lemnos_driver_system_flash_sys as sys;

/// Output goes through `say!`, the one place a check binary writes its result lines.
macro_rules! say {
    ($($arg:tt)*) => {
        { let _ = std::io::Write::write_fmt(&mut std::io::stdout(), format_args!("{}\n", format_args!($($arg)*))); }
    };
}

const WORK: &str = "/work";
const SYSFS: &str = "/sys";
const ERASE: u64 = 128 << 10;
const PAGE: u64 = 2048;
const BLOCK_PAGES: u64 = ERASE / PAGE;
const VOLUME_BYTES: u64 = 2 << 20;

/// One partition of the harness's chip: its number in `nandsim`'s `parts=`, its size in erase blocks,
/// and its bad erase blocks (partition-relative). The chip's bad blocks are the `badblocks=` list in
/// run.sh, shifted to each partition's start.
struct Part {
    role: &'static str,
    number: u32,
    blocks: u64,
    bad: &'static [u64],
}

const A: Part = Part {
    role: "A",
    number: 0,
    blocks: 64,
    bad: &[5, 9],
};
const B: Part = Part {
    role: "B",
    number: 1,
    blocks: 64,
    bad: &[6, 40],
};
const D: Part = Part {
    role: "D",
    number: 2,
    blocks: 64,
    bad: &[3, 20, 50],
};
const E: Part = Part {
    role: "E",
    number: 3,
    blocks: 8,
    bad: &[],
};
const F: Part = Part {
    role: "F",
    number: 4,
    blocks: 4,
    bad: &[],
};
const G: Part = Part {
    role: "G",
    number: 5,
    blocks: 820,
    bad: &[],
};

/// The image is what A holds: its good erase blocks, so B (two bad blocks, 62 good) takes it exactly.
const UBI_IMAGE: &str = "/work/a.img";

struct Checks {
    failed: u32,
}

impl Checks {
    fn check(&mut self, ok: bool, what: &str) {
        say!("{} {what}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            self.failed += 1;
        }
    }

    fn result<T>(&mut self, what: &str, r: Result<T, String>) -> Option<T> {
        match r {
            Ok(v) => {
                self.check(true, what);
                Some(v)
            }
            Err(why) => {
                self.check(false, &format!("{what}: {why}"));
                None
            }
        }
    }
}

/// xorshift64: a fixed stream, so the same bytes come out on every run.
fn fill(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// The MTD index of a partition, from its sysfs name.
fn mtd_of(part: &Part) -> Result<u32, String> {
    let want = format!("NAND simulator partition {}", part.number);
    for entry in fs::read_dir(format!("{SYSFS}/class/mtd")).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().into_string().unwrap_or_default();
        let Some(index) = name.strip_prefix("mtd").and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let text = fs::read_to_string(entry.path().join("name")).map_err(|e| e.to_string())?;
        if text.trim() == want {
            return Ok(index);
        }
    }
    Err(format!("no MTD partition named '{want}'"))
}

fn open_rw(index: u32) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!("/dev/mtd{index}"))
        .map_err(|e| format!("/dev/mtd{index}: {e}"))
}

fn geometry_of(index: u32) -> Result<Geometry, String> {
    nand::sysfs_geometry(Path::new(SYSFS), index)
}

fn whole(file: &File) -> Result<Vec<u8>, String> {
    let size = sys::mtd_info(file.as_fd()).map_err(|e| e.to_string())?.size as usize;
    let mut out = vec![0u8; size];
    file.read_exact_at(&mut out, 0).map_err(|e| e.to_string())?;
    Ok(out)
}

fn ubi_ctrl() -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/ubi_ctrl")
        .map_err(|e| format!("/dev/ubi_ctrl: {e}"))
}

/// Attaches MTD `index` to UBI and returns the device number.
fn attach(index: u32) -> Result<i32, String> {
    let ctrl = ubi_ctrl()?;
    let ubi = sys::ubi::attach(ctrl.as_fd(), index as i32)
        .map_err(|e| format!("UBI attach of mtd{index}: {e}"))?;
    say!("     ubi{ubi} sysfs: {}", ubi_counts(ubi));
    Ok(ubi)
}

/// The UBI device's own counts, from its sysfs node (for the log of each attach).
fn ubi_counts(ubi: i32) -> String {
    [
        "total_eraseblocks",
        "avail_eraseblocks",
        "bad_peb_count",
        "reserved_for_bad",
        "reserved_for_bad_all",
        "min_io_size",
        "max_ec",
    ]
    .iter()
    .map(|name| {
        let text = fs::read_to_string(format!("{SYSFS}/class/ubi/ubi{ubi}/{name}"))
            .unwrap_or_else(|_| "?".into());
        format!("{name}={}", text.trim())
    })
    .collect::<Vec<_>>()
    .join(" ")
}

fn detach(ubi: i32) -> Result<(), String> {
    let ctrl = ubi_ctrl()?;
    sys::ubi::detach(ctrl.as_fd(), ubi).map_err(|e| format!("UBI detach of ubi{ubi}: {e}"))
}

/// Reads the whole data volume of UBI device `ubi` (volume 0).
fn read_volume(ubi: i32, len: u64) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; len as usize];
    let file = File::open(format!("/dev/ubi{ubi}_0")).map_err(|e| e.to_string())?;
    file.read_exact_at(&mut out, 0).map_err(|e| e.to_string())?;
    Ok(out)
}

/// The UBI erase-counter header of the block at `offset`, or None when it holds none.
fn header_of(chip: &mut dyn Chip, offset: u64) -> Result<Option<nand::EcHeader>, String> {
    let mut page = vec![0u8; PAGE as usize];
    match chip.read(offset, &mut page) {
        Ok(_) => Ok(nand::parse_ec(&page)),
        Err(e) if nand::uncorrectable(&e) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Stage 1: the geometry the driver reads from sysfs is the kernel's own, and the bad blocks match.
fn stage_geometry(c: &mut Checks) {
    for part in [&A, &B, &D, &E, &F, &G] {
        let Some(index) = c.result(
            &format!("partition {} is an MTD device", part.role),
            mtd_of(part),
        ) else {
            continue;
        };
        let geometry = geometry_of(index);
        let info = File::open(format!("/dev/mtd{index}"))
            .map_err(|e| e.to_string())
            .and_then(|f| sys::mtd_info(f.as_fd()).map_err(|e| e.to_string()));
        if let (Ok(g), Ok(i)) = (&geometry, &info) {
            c.check(
                i.erasesize as u64 == g.erase
                    && i.writesize as u64 == g.write
                    && i.size as u64 == g.size
                    && i.oobsize as u64 == g.oob
                    && i.kind == 4,
                &format!(
                    "partition {}: MEMGETINFO (erase {}, write {}, size {}, oob {}, NAND) is what sysfs says",
                    part.role, i.erasesize, i.writesize, i.size, i.oobsize
                ),
            );
            c.check(
                g.erase == ERASE && g.write == PAGE,
                &format!(
                    "partition {}: the geometry is 128 KiB erase blocks and 2 KiB pages",
                    part.role
                ),
            );
        } else {
            c.check(
                false,
                &format!(
                    "partition {}: geometry and MEMGETINFO can be read",
                    part.role
                ),
            );
        }
        let Ok(file) = File::open(format!("/dev/mtd{index}")) else {
            continue;
        };
        let mut bad = Vec::new();
        for block in 0..part.blocks {
            if sys::is_bad_block(file.as_fd(), block * ERASE).unwrap_or(false) {
                bad.push(block);
            }
        }
        c.check(
            bad == part.bad,
            &format!(
                "partition {}: MEMGETBADBLOCK finds bad blocks {:?}, the injected ones are {:?}",
                part.role, bad, part.bad
            ),
        );
    }
}

/// Stage 2: UBI on A. The kernel attaches the blank chip, formats it, a volume is written through its
/// node and read back, and then A's good erase blocks are read through MTD into the image file.
fn stage_ubi_a(c: &mut Checks) {
    let Some(index) = c.result("partition A is an MTD device", mtd_of(&A)) else {
        return;
    };
    let Some(ubi) = c.result(
        &format!("UBI attaches mtd{index} (a blank chip: the kernel formats it)"),
        attach(index),
    ) else {
        return;
    };
    let pattern = fill(0x5eed_0001, VOLUME_BYTES as usize);
    let made = c.result(
        "UBI creates a volume",
        make_volume(ubi, VOLUME_BYTES as i64),
    );
    if made.is_some() {
        let wrote = c.result(
            "UBI volume update takes the whole volume",
            write_volume(ubi, &pattern),
        );
        if wrote.is_some() {
            let back = c.result(
                "the volume reads back through the kernel",
                read_volume(ubi, VOLUME_BYTES),
            );
            if let Some(back) = back {
                c.check(
                    back == pattern,
                    "the kernel's volume reads back what was written",
                );
            }
        }
    }
    c.result(&format!("UBI detaches ubi{ubi}"), detach(ubi));

    // The image: A's good erase blocks, in order, as the driver's writer takes them.
    let Some(file) = c.result("partition A opens for reading", open_rw(index)) else {
        return;
    };
    let mut chip = Device::new(file);
    let Some(geometry) = c.result("A's geometry reads", geometry_of(index)) else {
        return;
    };
    let Some(good) = c.result(
        "A's bad-block table reads",
        nand::good_blocks(&mut chip, &geometry).map_err(|(_, why)| why),
    ) else {
        return;
    };
    let mut image = Vec::with_capacity(good.len() * ERASE as usize);
    let mut block = vec![0u8; ERASE as usize];
    for offset in &good {
        if chip.read(*offset, &mut block).is_err() {
            c.check(false, "an erase block of A reads");
            return;
        }
        image.extend_from_slice(&block);
    }
    c.check(
        image.starts_with(b"UBI#") && good.len() as u64 == A.blocks - A.bad.len() as u64,
        &format!(
            "the image is {} good erase blocks of A, the first a UBI erase block",
            good.len()
        ),
    );
    if let Err(e) = fs::create_dir_all(WORK).and_then(|()| fs::write(UBI_IMAGE, &image)) {
        c.check(false, &format!("the image is written to {UBI_IMAGE}: {e}"));
    }
}

fn make_volume(ubi: i32, bytes: i64) -> Result<i32, String> {
    let dev = File::open(format!("/dev/ubi{ubi}")).map_err(|e| e.to_string())?;
    let mut req =
        sys::ubi::MkvolReq::new("data", bytes, sys::ubi::DYNAMIC).map_err(|e| e.to_string())?;
    sys::ubi::mkvol(dev.as_fd(), &mut req).map_err(|e| e.to_string())
}

fn write_volume(ubi: i32, bytes: &[u8]) -> Result<(), String> {
    let vol = OpenOptions::new()
        .write(true)
        .open(format!("/dev/ubi{ubi}_0"))
        .map_err(|e| e.to_string())?;
    sys::ubi::volume_update(vol.as_fd(), bytes.len() as i64).map_err(|e| e.to_string())?;
    vol.write_all_at(bytes, 0).map_err(|e| e.to_string())
}

/// Writes `image` to `part` with the driver's writer, in pieces as a stream would arrive.
/// Returns the SHA-256 of what was written (the driver's own).
fn drive(
    index: u32,
    image_kind: Image,
    image: &[u8],
    mark_bad: bool,
) -> Result<([u8; 32], u64), String> {
    let geometry = geometry_of(index)?;
    let mut plan_chip = Device::new(open_rw(index)?);
    let plan = Plan::new(
        &mut plan_chip,
        geometry,
        image_kind,
        image.len() as u64,
        mark_bad,
    )
    .map_err(|(_, why)| why)?;
    let mut sink = Sink::new(Box::new(Device::new(open_rw(index)?)), plan);
    let mut at = 0usize;
    let mut piece = 4096usize;
    while at < image.len() {
        let end = (at + piece).min(image.len());
        sink.push(&image[at..end]).map_err(|(_, why)| why)?;
        at = end;
        piece = piece * 3 / 2 + 1; // pieces of every size, not one shape
    }
    sink.finish().map_err(|(_, why)| why)
}

fn verify_partition(index: u32, image_size: u64, expect: [u8; 32]) -> Result<(), String> {
    let geometry = geometry_of(index)?;
    let chip = Box::new(Device::new(open_rw(index)?));
    let mut verifier = Verifier::new(chip, geometry, image_size, expect).map_err(|(_, why)| why)?;
    while verifier.step().map_err(|(_, why)| why)? {}
    Ok(())
}

/// Stage 3: the driver writes the UBI image to B, the kernel attaches B and reads the volume back.
fn stage_write_b(c: &mut Checks, corrupt: bool) {
    let Some(index) = c.result("partition B is an MTD device", mtd_of(&B)) else {
        return;
    };
    let Some(ubi) = c.result(
        &format!("UBI formats mtd{index} (B) for the erase counters"),
        attach(index),
    ) else {
        return;
    };
    // UBI writes the erase-counter headers from its background worker, after the attach returns: the
    // check waits for every good block to hold one before detaching (a detach first leaves some blank).
    let Ok(good_wait) = good_of(index) else {
        c.check(false, "B's good erase blocks read for the header wait");
        return;
    };
    let waited = wait_for_headers(index, &good_wait);
    c.check(
        waited,
        "every good block of B holds a UBI erase-counter header once the kernel has formatted it",
    );
    c.result(
        &format!("UBI detaches ubi{ubi} before the write"),
        detach(ubi),
    );
    let Ok(image) = fs::read(UBI_IMAGE) else {
        c.check(false, &format!("the image {UBI_IMAGE} is there to write"));
        return;
    };

    // The erase counters the target holds before the write: the carry-forward rule's input.
    let Ok(good_b) = good_of(index) else {
        c.check(false, "B's good erase blocks read");
        return;
    };
    let Ok(file) = open_rw(index) else {
        c.check(false, "partition B opens");
        return;
    };
    let mut chip = Device::new(file);
    let mut before = Vec::new();
    for offset in &good_b {
        before.push(header_of(&mut chip, *offset).ok().flatten().map(|h| h.ec));
    }
    let missing: Vec<u64> = good_b
        .iter()
        .zip(&before)
        .filter(|(_, e)| e.is_none())
        .map(|(o, _)| o / ERASE)
        .collect();
    say!(
        "     blocks of B without a valid header before the write (erase block numbers): {missing:?}"
    );
    c.check(
        before.iter().all(|e| e.is_some()),
        "every good block of B holds a valid UBI erase-counter header before the write",
    );

    let mut payload = image.clone();
    if corrupt {
        // The negative run: a writer that changes one byte of every image block's data. The volume
        // lives in some of those blocks, so its read must differ from what was written.
        for block in 0..(image.len() as u64 / ERASE) {
            payload[(block * ERASE + 5000) as usize] ^= 0x40;
        }
    }
    let (hash, _) = match drive(index, Image::Ubi, &payload, false) {
        Ok(v) => {
            c.check(true, "the driver's UBI writer finishes on B");
            v
        }
        Err(why) => {
            c.check(
                false,
                &format!("the driver's UBI writer finishes on B: {why}"),
            );
            return;
        }
    };
    c.result(
        "the driver reads back what it wrote (its own verifier)",
        verify_partition(index, image.len() as u64, hash),
    );

    // Erase counters: each good block carries its count forward by one, with a valid header.
    let Ok(file) = open_rw(index) else {
        c.check(false, "partition B opens again");
        return;
    };
    let mut chip = Device::new(file);
    let mut counts_ok = true;
    let mut sample = Vec::new();
    for (i, offset) in good_b.iter().enumerate() {
        match header_of(&mut chip, *offset) {
            Ok(Some(h)) => {
                if let Some(old) = before[i] {
                    if h.ec != old + 1 {
                        counts_ok = false;
                    }
                    if sample.len() < 4 {
                        sample.push((old, h.ec));
                    }
                } else {
                    counts_ok = false;
                }
            }
            _ => counts_ok = false,
        }
    }
    c.check(
        counts_ok,
        &format!("every good block of B carries its erase count forward by one (first four old,new: {sample:?})"),
    );

    // The bad blocks of B are skipped: they are still marked bad after the write.
    let bad_still = B.bad.iter().all(|block| {
        File::open(format!("/dev/mtd{index}"))
            .ok()
            .and_then(|f| sys::is_bad_block(f.as_fd(), block * ERASE).ok())
            .unwrap_or(false)
    });
    c.check(
        bad_still,
        "B's bad blocks are still marked bad after the write",
    );

    // The kernel attaches what the driver wrote and reads the volume back.
    let Some(ubi) = c.result(
        &format!("UBI attaches mtd{index} (B) after the driver's write"),
        attach(index),
    ) else {
        return;
    };
    let pattern = fill(0x5eed_0001, VOLUME_BYTES as usize);
    let back = read_volume(ubi, VOLUME_BYTES);
    match back {
        Ok(back) => c.check(
            back == pattern,
            "the volume on B reads back the bytes written to A",
        ),
        Err(why) => c.check(false, &format!("the volume on B reads back: {why}")),
    }
    c.result(&format!("UBI detaches ubi{ubi} from B"), detach(ubi));
}

fn good_of(index: u32) -> Result<Vec<u64>, String> {
    let mut chip = Device::new(open_rw(index)?);
    let g = geometry_of(index)?;
    nand::good_blocks(&mut chip, &g)
        .map_err(|(_, why)| why)
        .map(|v| v.into_iter().collect())
}

/// Waits, up to ten seconds, until every block in `good` holds a valid erase-counter header.
fn wait_for_headers(index: u32, good: &[u64]) -> bool {
    let Ok(file) = open_rw(index) else {
        return false;
    };
    let mut chip = Device::new(file);
    for _ in 0..100 {
        let all = good
            .iter()
            .all(|offset| matches!(header_of(&mut chip, *offset), Ok(Some(_))));
        if all {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// Stage 4: an image that does not fit once bad blocks are subtracted is refused, and nothing erased.
fn stage_too_big(c: &mut Checks) {
    let Some(index) = c.result("partition D is an MTD device", mtd_of(&D)) else {
        return;
    };
    let Ok(image) = fs::read(UBI_IMAGE) else {
        c.check(false, "the image is there for the refusal");
        return;
    };
    let Ok(file) = open_rw(index) else {
        c.check(false, "partition D opens");
        return;
    };
    let before = whole(&file);
    let refused = match (before.as_ref(), geometry_of(index), open_rw(index)) {
        (Ok(_), Ok(g), Ok(f)) => {
            let mut chip = Device::new(f);
            Plan::new(&mut chip, g, Image::Ubi, image.len() as u64, false)
                .err()
                .map(|(_, why)| why)
        }
        _ => None,
    };
    c.check(
        refused
            .as_deref()
            .is_some_and(|w| w.contains("good erase blocks")),
        &format!(
            "the image is refused before any erase: {}",
            refused.as_deref().unwrap_or("it was not refused")
        ),
    );
    let after = whole(&file);
    c.check(
        matches!((before, after), (Ok(a), Ok(b)) if a == b),
        "partition D is unchanged by the refused write",
    );
}

/// Stage 5: a raw image that is not whole erase blocks, on E; one good erase block is erased.
fn stage_raw(c: &mut Checks) {
    let Some(index) = c.result("partition E is an MTD device", mtd_of(&E)) else {
        return;
    };
    let size = 6 * ERASE as usize + 3000;
    let raw = fill(0x7a3, size);
    match drive(index, Image::Raw, &raw, false) {
        Ok((hash, _)) => {
            c.check(true, "the raw image is written to E");
            c.result(
                "the raw image reads back as written",
                verify_partition(index, size as u64, hash),
            );
        }
        Err(why) => c.check(false, &format!("the raw image is written to E: {why}")),
    }
    let Ok(file) = open_rw(index) else {
        return;
    };
    let erased = sys::erase(file.as_fd(), ERASE as u32, ERASE as u32).is_ok();
    c.check(erased, "MEMERASE on a good erase block (block 1) succeeds");
    let mut block = vec![0u8; ERASE as usize];
    let _ = file.read_exact_at(&mut block, ERASE);
    c.check(
        block.iter().all(|&b| b == 0xFF),
        "the erased block reads back all 0xFF",
    );
    let mut tail = vec![0u8; 3000];
    let _ = file.read_exact_at(&mut tail, 6 * ERASE);
    c.check(
        tail == raw[6 * ERASE as usize..],
        "the raw image's partial last erase block holds its bytes",
    );
}

/// Stage 6: the kernel's bit flips are corrected and counted, and the driver reports the same counts.
fn stage_ecc(c: &mut Checks) {
    let Some(index) = c.result("partition A is an MTD device for the ECC reads", mtd_of(&A)) else {
        return;
    };
    let Ok(file) = open_rw(index) else {
        c.check(false, "partition A opens for the ECC reads");
        return;
    };
    let Ok(stats_fd) = open_rw(index) else {
        c.check(false, "partition A opens for the ECC counts");
        return;
    };
    let before = sys::ecc_stats(stats_fd.as_fd()).ok();
    let mut chip = Device::new(file);
    let mut corrected = 0u64;
    let mut page = vec![0u8; PAGE as usize];
    let mut failed_reads = 0u64;
    // Bit flips hit written pages only: the kernel's simulator returns erased pages untouched, so the
    // reads cover A's written erase blocks many times over to make a flip near certain.
    for _pass in 0..40 {
        for block in 0..A.blocks {
            if A.bad.contains(&block) {
                continue;
            }
            for p in 0..BLOCK_PAGES {
                match chip.read(block * ERASE + p * PAGE, &mut page) {
                    Ok(n) => corrected += u64::from(n),
                    Err(_) => failed_reads += 1,
                }
            }
        }
    }
    let after = sys::ecc_stats(stats_fd.as_fd()).ok();
    let kernel = match (before, after) {
        (Some(b), Some(a)) => Some(a.corrected.wrapping_sub(b.corrected)),
        _ => None,
    };
    c.check(
        corrected > 0,
        &format!(
            "the injected bit flips are corrected by the ECC ({corrected} corrections in the reads)"
        ),
    );
    c.check(
        kernel == Some(corrected as u32),
        &format!(
            "the driver's count equals the kernel's ECC count ({corrected} against {kernel:?})"
        ),
    );
    c.check(
        failed_reads == 0,
        "no read is uncorrectable: each injected flip is one bit in a 512-byte chunk",
    );
}

/// Stage 7: a write whose erase fails. Without `mark_bad` the block stays good; with it, the kernel
/// reports it bad afterwards.
fn stage_mark_bad(c: &mut Checks) {
    let Some(index) = c.result("partition F is an MTD device", mtd_of(&F)) else {
        return;
    };
    let image = fill(0x9e11, 4 * ERASE as usize);
    let weak = 3u64;
    let first = drive(index, Image::Raw, &image, false);
    c.check(
        first
            .as_ref()
            .err()
            .is_some_and(|w| w.contains("failed to erase")),
        &format!(
            "a weak erase block fails the write without mark_bad: {}",
            first
                .as_ref()
                .err()
                .map(String::as_str)
                .unwrap_or("it succeeded")
        ),
    );
    let Ok(file) = open_rw(index) else {
        return;
    };
    let bad_before = sys::is_bad_block(file.as_fd(), weak * ERASE).unwrap_or(true);
    c.check(
        !bad_before,
        "mark_bad off: the weak block is not marked bad",
    );
    let second = drive(index, Image::Raw, &image, true);
    c.check(
        second
            .as_ref()
            .err()
            .is_some_and(|w| w.contains("marked bad")),
        &format!(
            "with mark_bad the write fails and says the block is marked bad: {}",
            second
                .as_ref()
                .err()
                .map(String::as_str)
                .unwrap_or("it succeeded")
        ),
    );
    let bad_after = sys::is_bad_block(file.as_fd(), weak * ERASE).unwrap_or(false);
    c.check(
        bad_after,
        "mark_bad on: MEMGETBADBLOCK reports the weak block bad afterwards",
    );
}

fn main() -> ExitCode {
    let corrupt = std::env::args().any(|a| a == "--corrupt");
    let mut c = Checks { failed: 0 };
    stage_geometry(&mut c);
    stage_ubi_a(&mut c);
    stage_write_b(&mut c, corrupt);
    stage_too_big(&mut c);
    stage_raw(&mut c);
    stage_ecc(&mut c);
    stage_mark_bad(&mut c);
    if c.failed == 0 {
        say!("lemnos-nand-check: passed (0 failed)");
        ExitCode::SUCCESS
    } else {
        say!("lemnos-nand-check: FAILED ({} failed)", c.failed);
        ExitCode::FAILURE
    }
}
