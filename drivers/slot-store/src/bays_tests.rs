//! Tests of the payload bays (layout version 6). Every image is made by the image builder
//! ([`crate::bootimage::build`]) at the board's boot partition geometry (161,792 sectors, FAT16 with
//! 2 KiB clusters) and read back by the same [`crate::bootfat`] code the boot chain uses.
//!
//! The power-cut sweep runs the install with the test layout (small caps, the same volume): every
//! sector write is a cut point, and each cut is torn four ways. After each cut the default still
//! boots, every sector outside the set being installed (and the two pointers the install moves) is
//! byte-identical to the image before it, the cut set reads uncommitted (or committed and whole),
//! and the next install repairs it. The real caps run once uncut and once with a sampled sweep that
//! cuts at every metadata write and at the first and last write of each run of data writes.

use super::*;
use crate::boot_raw::{Boot, BootRaw, Pointer};
use crate::bootfat;
use crate::bootimage::{self, Family, Parts};
use crate::twocopy::Sector;
use std::collections::HashSet;

/// The test layout: small caps on the real volume (the sweep needs every write as a cut point).
fn test_layout() -> Layout {
    Layout::new([2048, 1024, 512, 512, 512]).unwrap()
}

/// The board's caps: the kernel as the next build stores it, gzip -9 (10,168,581 bytes), under an
/// 11 MiB cap; dtb 88 KiB; the overlay and the command line 4 KiB each; initramfs 1 MiB.
fn board_layout() -> Layout {
    Layout::new([11 * 1024 * 1024, 1024 * 1024, 88 * 1024, 4096, 4096]).unwrap()
}

/// The payloads of slot `a`, the one the image is built with (lengths below the test caps).
fn payloads_a() -> [Vec<u8>; 5] {
    [
        vec![1; 1500],
        vec![2; 700],
        vec![3; 300],
        b"console=ttyS0 root=a".to_vec(),
        vec![4; 200],
    ]
}

/// The payloads of slot `b`.
fn payloads_b() -> [Vec<u8>; 5] {
    [
        vec![11; 1200],
        vec![12; 900],
        vec![13; 500],
        b"console=ttyS0 root=b".to_vec(),
        vec![14; 100],
    ]
}

/// The payloads of slot `c`, the one most tests install.
fn payloads_c() -> [Vec<u8>; 5] {
    [
        vec![9; 2000],
        vec![8; 100],
        vec![7; 400],
        b"c-line".to_vec(),
        vec![6; 300],
    ]
}

fn refs(p: &[Vec<u8>; 5]) -> [&[u8]; 5] {
    std::array::from_fn(|i| p[i].as_slice())
}

/// The board's boot partition: 161,792 sectors.
const BOARD_SECTORS: u64 = 161_792;

/// The Pi firmware's parts for an image: the generated root text, as the builder gives it.
fn boot_parts(layout: &Layout) -> Parts {
    Parts {
        family: Family::Tryboot,
        layout: layout.clone(),
        root: vec![
            (
                "config.txt",
                b"# Generated\nkernel_watchdog_timeout=15\n".to_vec(),
            ),
            ("tryboot.txt", b"# Generated\n".to_vec()),
            ("slot-layout.toml", b"version = 6\n".to_vec()),
        ],
    }
}

/// The boot partition the builder makes for `layout`: slot `a` in bay 0, every pointer naming it.
fn volume_image(layout: &Layout) -> Vec<u8> {
    bootimage::build(
        &boot_parts(layout),
        BOARD_SECTORS * 512,
        "a",
        &refs(&payloads_a()),
    )
    .expect("the image builds at the board's geometry")
}

/// The write the device tears, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Torn {
    /// The sector keeps its old bytes (the write never reached the medium).
    Old,
    /// The whole sector is new, and the write reports failure.
    New,
    /// The first 256 bytes are new, the rest old.
    FirstHalfNew,
    /// Random bytes (a fixed seed per cut).
    Random(u64),
}

impl Torn {
    fn all(k: usize) -> [Torn; 4] {
        [
            Torn::Old,
            Torn::New,
            Torn::FirstHalfNew,
            Torn::Random(k as u64 + 1),
        ]
    }
}

/// A block device over an in-memory image. Writes are counted and logged by sector; the write at
/// `cut.0` is torn as `cut.1`, reports failure, and every later write fails without changing anything.
struct Dev {
    img: Vec<u8>,
    writes: usize,
    written: Vec<u64>,
    cut: Option<(usize, Torn)>,
    dead: bool,
}

impl Dev {
    fn new(img: Vec<u8>) -> Self {
        Self {
            img,
            writes: 0,
            written: Vec::new(),
            cut: None,
            dead: false,
        }
    }
}

impl ReadAt for Dev {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let at = offset as usize;
        buf.copy_from_slice(&self.img[at..at + buf.len()]);
        Ok(())
    }
}

impl Sectors for Dev {
    fn read(&mut self, lba: u64, buf: &mut Sector) -> std::io::Result<()> {
        let at = lba as usize * SECTOR;
        buf.copy_from_slice(&self.img[at..at + SECTOR]);
        Ok(())
    }

    fn write(&mut self, lba: u64, buf: &Sector) -> std::io::Result<()> {
        if self.dead {
            return Err(std::io::Error::other("power is off"));
        }
        let idx = self.writes;
        self.writes += 1;
        self.written.push(lba);
        let at = lba as usize * SECTOR;
        match self.cut.filter(|(n, _)| *n == idx) {
            None => {
                self.img[at..at + SECTOR].copy_from_slice(buf);
                Ok(())
            }
            Some((_, torn)) => {
                let s = &mut self.img[at..at + SECTOR];
                match torn {
                    Torn::Old => {}
                    Torn::New => s.copy_from_slice(buf),
                    Torn::FirstHalfNew => s[..256].copy_from_slice(&buf[..256]),
                    Torn::Random(seed) => {
                        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                        for b in s.iter_mut() {
                            x ^= x << 13;
                            x ^= x >> 7;
                            x ^= x << 17;
                            *b = x as u8;
                        }
                    }
                }
                self.dead = true;
                Err(std::io::Error::other("power cut mid-write"))
            }
        }
    }
}

/// What an install into `set` may write: the set's own sectors (its region, the region's FAT
/// sectors in both copies, its directories, its record) and the two pointers it moves.
fn write_set(img: &[u8], set: u8) -> HashSet<u64> {
    let mut dev = Dev::new(img.to_vec());
    let mut out: HashSet<u64> = set_sectors(&mut dev, set).unwrap().into_iter().collect();
    let placed = place(&mut dev).unwrap();
    for lba in [placed.default, placed.previous] {
        out.insert(lba);
        out.insert(lba + 1);
    }
    out
}

/// Every write lies inside `allowed`, and every sector outside it is byte-identical to `pre`.
fn only_inside(pre: &[u8], dev: &Dev, allowed: &HashSet<u64>) -> Result<(), String> {
    if let Some(bad) = dev.written.iter().find(|l| !allowed.contains(l)) {
        return Err(format!("sector {bad} was written, outside the set's own"));
    }
    for (lba, (a, b)) in pre.chunks(SECTOR).zip(dev.img.chunks(SECTOR)).enumerate() {
        if a != b && !allowed.contains(&(lba as u64)) {
            return Err(format!("sector {lba} changed outside the set's own"));
        }
    }
    Ok(())
}

/// The boot chain's view: the default boots `a` (bay 0) or the new slot in `new`, and the previous
/// boots `a`.
fn check_bootable(img: &[u8], layout: &Layout, new: (u8, &str)) -> Result<(), String> {
    let mut dev = Dev::new(img.to_vec());
    let placed = place(&mut dev)?;
    let d = boot_view(&mut dev, layout, placed.default)?;
    if d != (0, "a".to_string()) && d != (new.0, new.1.to_string()) {
        return Err(format!("default boots {d:?}: neither a nor {new:?}"));
    }
    let p = boot_view(&mut dev, layout, placed.previous)?;
    if p != (0, "a".to_string()) {
        return Err(format!("previous boots {p:?}, not a"));
    }
    Ok(())
}

/// The cut set reads uncommitted, or committed with every file whole: never a record over files
/// that do not match it, and never without a record (the region would be lost).
fn check_cut_set(img: &[u8], layout: &Layout, set: u8) -> Result<(), String> {
    let mut dev = Dev::new(img.to_vec());
    let lba = place_sets(&mut dev)?[set as usize];
    match read_info(&mut dev, lba).map_err(|e| e.to_string())? {
        None => Err(format!("bay{set} has no record: its region is lost")),
        Some(info) if info.slot.is_none() => Ok(()),
        Some(info) => check_set(&mut dev, layout, set, &info).map(drop),
    }
}

/// A file of a set, read by path through the boot chain's FAT reader.
fn file(img: &[u8], set: u8, path: &str) -> Vec<u8> {
    bootfat::read_file(
        &mut Dev::new(img.to_vec()),
        &format!("{}/{path}", set_dir(set)),
        64 << 20,
    )
    .unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn the_layout_rejects_a_size_that_is_not_whole_sectors() {
    assert!(Layout::new([2000, 1024, 512, 512, 512]).is_err());
    assert!(Layout::new([0, 1024, 512, 512, 512]).is_err());
}

#[test]
fn the_written_list_has_each_device_tree_copy_the_firmware_files_and_the_generated_file() {
    let layout = test_layout()
        .with_dtbs(&["one.dtb".into(), "two.dtb".into()])
        .with_firmware(&["extra".into()], None)
        .with_files(&["overlay_map.dtb".into()]);
    assert_eq!(layout.count(), 7);
    assert_eq!(
        layout.names(),
        [
            "kernel8.img",
            "initramfs.cpio",
            "one.dtb",
            "two.dtb",
            "cmdline.txt",
            "overlays/lemnos-board.dtbo",
            "overlays/extra.dtbo",
            "overlays/overlay_map.dtb",
            OVERLAY_README,
        ]
    );
    assert_eq!(
        layout.sources(),
        [
            Some(0),
            Some(1),
            Some(2),
            Some(2),
            Some(3),
            Some(4),
            Some(5),
            Some(6),
            None
        ]
    );
    assert_eq!(layout.caps().len(), layout.names().len());
    let uboot = test_layout().with_form(Form::Uboot);
    assert_eq!(uboot.names().last().map(String::as_str), Some(EXTLINUX));
    // Payloads of the wrong count are refused by name.
    let p = payloads_a();
    assert!(
        layout
            .written(&refs(&p))
            .unwrap_err()
            .contains("5 boot files are given")
    );
}

#[test]
fn a_record_round_trips_and_fits_at_the_most_files_and_the_longest_slot() {
    let region = bayfat::Region {
        start: 512,
        clusters: 256,
    };
    let files: Vec<(String, Vec<u8>)> = (0..MAX_MEMBERS)
        .map(|i| (format!("overlays/f{i}.dtbo"), vec![i as u8; i * 7]))
        .collect();
    let slot = "s".repeat(64);
    let info = Info::committed(region, &slot, &files);
    let bytes = info.encode();
    assert!(
        bytes.len() <= crate::twocopy::MAX_PAYLOAD,
        "{} bytes",
        bytes.len()
    );
    assert_eq!(Info::decode(&bytes), Some(info));
    let empty = Info::empty(region);
    assert_eq!(Info::decode(&empty.encode()), Some(empty));
    // Another version is not a record of this one.
    let mut other = bytes.clone();
    other[1] = LAYOUT_VERSION - 1;
    assert_eq!(Info::decode(&other), None);
    // No record of layout version 5 or before reads as one of this layout: each begins with its
    // slot name's length (0 to 64), never with the tag.
    let mut v5 = vec![6u8];
    v5.extend_from_slice(b"ours-a");
    assert_eq!(Info::decode(&v5), None);
    assert_eq!(Info::decode(&[0]), None);
    // An empty record with files is no record.
    let mut bad = Info::empty(region);
    bad.members.push((1, [0; 8]));
    assert_eq!(Info::decode(&bad.encode()), None);
}

#[test]
fn the_built_image_holds_slot_a_at_exact_sizes_in_aligned_disjoint_regions() {
    let layout = test_layout();
    let img = volume_image(&layout);
    let written = layout.written(&refs(&payloads_a())).unwrap();
    for (path, bytes) in &written {
        assert_eq!(&file(&img, 0, path), bytes, "{path} is exact");
    }
    let mut dev = Dev::new(img.clone());
    let v = bootfat::volume(&mut dev).unwrap();
    let mut regions = Vec::new();
    for (set, lba) in place_sets(&mut dev).unwrap().into_iter().enumerate() {
        let info = read_info(&mut dev, lba).unwrap().expect("a record");
        let r = info.region;
        assert_eq!(r.start % bayfat::FAT16_PER_SECTOR, 0);
        assert_eq!(r.clusters % bayfat::FAT16_PER_SECTOR, 0);
        r.check(&v).unwrap();
        // The reserve holds every cluster of the region no file holds.
        let cb = v.spc * SECTOR as u64;
        let used: u64 = if set == 0 {
            written
                .iter()
                .map(|(_, b)| (b.len() as u64).div_ceil(cb).max(1))
                .sum()
        } else {
            0
        };
        let reserve = bootfat::locate_path(
            &mut dev,
            &format!("{}/{}", set_dir(set as u8), bayfat::RESERVE),
            (r.clusters - used) * cb,
        );
        assert!(reserve.is_ok(), "bay{set}: {reserve:?}");
        regions.push(r);
    }
    for (i, a) in regions.iter().enumerate() {
        for b in &regions[i + 1..] {
            assert!(
                a.start + a.clusters <= b.start || b.start + b.clusters <= a.start,
                "{a:?} and {b:?} overlap"
            );
        }
    }
    let placed = place(&mut dev).unwrap();
    assert_eq!(
        boot_view(&mut dev, &layout, placed.default),
        Ok((0, "a".to_string()))
    );
}

#[test]
fn the_set_shows_each_name_exactly_as_written_in_lower_case() {
    let img = volume_image(&test_layout());
    let mut dev = Dev::new(img);
    let shown: Vec<String> = bootfat::list_dir(&mut dev, "slots/bay0")
        .unwrap()
        .iter()
        .map(bootfat::DirEntry::shown)
        .collect();
    for want in [
        "kernel8.img",
        "initramfs.cpio",
        "board.dtb",
        "cmdline.txt",
        "overlays",
        INFO_NAME,
        bayfat::RESERVE,
    ] {
        assert!(shown.iter().any(|s| s == want), "{want} in {shown:?}");
    }
    let overlays: Vec<String> = bootfat::list_dir(&mut dev, "slots/bay0/overlays")
        .unwrap()
        .iter()
        .map(bootfat::DirEntry::shown)
        .collect();
    assert!(overlays.iter().any(|s| s == "README"), "{overlays:?}");
    assert!(
        overlays.iter().any(|s| s == "lemnos-board.dtbo"),
        "{overlays:?}"
    );
}

#[test]
fn an_install_into_a_free_set_writes_only_that_set_and_the_pointers_and_switches_the_default() {
    let layout = test_layout();
    let pre = volume_image(&layout);
    let allowed = write_set(&pre, 2);
    let mut dev = Dev::new(pre.clone());
    assert_eq!(
        install(&mut dev, &layout, 2, "c", &refs(&payloads_c())).unwrap(),
        Done::Installed
    );
    only_inside(&pre, &dev, &allowed).unwrap();
    for (path, bytes) in layout.written(&refs(&payloads_c())).unwrap() {
        assert_eq!(file(&dev.img, 2, &path), bytes, "{path}");
    }
    let placed = place(&mut dev).unwrap();
    assert_eq!(
        boot_view(&mut dev, &layout, placed.default),
        Ok((2, "c".to_string()))
    );
    assert_eq!(
        boot_view(&mut dev, &layout, placed.previous),
        Ok((0, "a".to_string()))
    );
}

#[test]
fn an_install_into_the_default_bay_is_refused_and_writes_nothing() {
    let layout = test_layout();
    let mut dev = Dev::new(volume_image(&layout));
    let e = install(&mut dev, &layout, 0, "c", &refs(&payloads_c())).unwrap_err();
    assert!(matches!(e, Error::Refused(_)), "{e}");
    assert!(dev.written.is_empty());
}

#[test]
fn an_install_into_the_previous_or_the_trial_bay_is_refused_and_writes_nothing() {
    let layout = test_layout();
    let mut dev = Dev::new(volume_image(&layout));
    install(&mut dev, &layout, 1, "b", &refs(&payloads_b())).unwrap();
    // Default bay1, previous bay0.
    dev.written.clear();
    let e = install(&mut dev, &layout, 0, "c", &refs(&payloads_c())).unwrap_err();
    assert!(matches!(e, Error::Refused(_)), "{e}");
    assert!(dev.written.is_empty());
    let mut protected = Protected::from_volume(&mut dev).unwrap();
    protected.trial = Some(2);
    let e = stage_with(&mut dev, &layout, 2, "c", &refs(&payloads_c()), &protected).unwrap_err();
    assert!(matches!(e, Error::Refused(_)), "{e}");
    assert!(dev.written.is_empty());
}

#[test]
fn an_install_while_a_trial_is_pending_is_refused_with_the_reason_and_writes_nothing() {
    let layout = test_layout();
    let mut dev = Dev::new(volume_image(&layout));
    let mut protected = Protected::from_volume(&mut dev).unwrap();
    protected.pending = Some(Trial {
        slot: "b".into(),
        attempts: 2,
    });
    let e = stage_with(&mut dev, &layout, 2, "c", &refs(&payloads_c()), &protected).unwrap_err();
    assert!(e.to_string().contains("slot b is on trial"), "{e}");
    assert!(dev.written.is_empty());
}

#[test]
fn a_payload_larger_than_its_cap_is_refused_by_name_and_writes_nothing() {
    let layout = test_layout();
    let mut dev = Dev::new(volume_image(&layout));
    let mut c = payloads_c();
    c[1] = vec![0; 1025];
    let e = install(&mut dev, &layout, 2, "c", &refs(&c)).unwrap_err();
    assert!(e.to_string().contains("initramfs"), "{e}");
    assert!(dev.written.is_empty());
}

#[test]
fn a_changed_byte_in_a_committed_file_is_found_by_name() {
    let layout = test_layout();
    let mut img = volume_image(&layout);
    let mut dev = Dev::new(img.clone());
    let found = bootfat::locate_path(&mut dev, "slots/bay0/cmdline.txt", 20).unwrap();
    img[found.first_lba as usize * SECTOR] ^= 1;
    let mut dev = Dev::new(img);
    let placed = place(&mut dev).unwrap();
    let e = boot_view(&mut dev, &layout, placed.default).unwrap_err();
    assert!(e.contains("cmdline.txt"), "{e}");
}

#[test]
fn compare_hashes_accepts_the_same_build_and_names_a_changed_file() {
    let layout = test_layout();
    let mut dev = Dev::new(volume_image(&layout));
    let lba = place_sets(&mut dev).unwrap()[0];
    let info = read_info(&mut dev, lba).unwrap().unwrap();
    compare_hashes(&layout, 0, &info, &refs(&payloads_a())).unwrap();
    let mut other = payloads_a();
    other[2][0] ^= 1;
    let e = compare_hashes(&layout, 0, &info, &refs(&other)).unwrap_err();
    assert!(e.contains("board.dtb"), "{e}");
}

#[test]
fn the_layout_is_read_back_from_the_volume() {
    let layout = test_layout()
        .with_dtbs(&["one.dtb".into(), "two.dtb".into()])
        .with_files(&["overlay_map.dtb".into()]);
    let mut p = payloads_a().to_vec();
    p.push(vec![5; 100]);
    let refs: Vec<&[u8]> = p.iter().map(Vec::as_slice).collect();
    let img = bootimage::build(&boot_parts(&layout), BOARD_SECTORS * 512, "a", &refs).unwrap();
    let mut dev = Dev::new(img.clone());
    let found = discover(&mut dev).unwrap();
    assert_eq!(found.names(), layout.names());
    assert_eq!(found.form, Form::Firmware);
    assert_eq!(file(&img, 0, "one.dtb"), file(&img, 0, "two.dtb"));
}

#[allow(clippy::print_stdout)] // the sweep prints its counts as proof
#[test]
fn power_cut_at_every_sector_write_and_each_torn_variant_leaves_a_bootable_default() {
    let layout = test_layout();
    let pre = volume_image(&layout);
    let allowed = write_set(&pre, 2);
    let c = payloads_c();
    let mut uncut = Dev::new(pre.clone());
    assert_eq!(
        install(&mut uncut, &layout, 2, "c", &refs(&c)).unwrap(),
        Done::Installed
    );
    let n = uncut.writes;
    assert!(n > 0, "the install writes");
    let mut runs = 0usize;
    for k in 0..n {
        for torn in Torn::all(k) {
            let mut dev = Dev::new(pre.clone());
            dev.cut = Some((k, torn));
            let r = install(&mut dev, &layout, 2, "c", &refs(&c));
            assert!(r.is_err(), "cut {k} {torn:?}: the install reported success");
            let at = |e: String| format!("cut {k} {torn:?}: {e}");
            check_bootable(&dev.img, &layout, (2, "c")).unwrap_or_else(|e| panic!("{}", at(e)));
            only_inside(&pre, &dev, &allowed).unwrap_or_else(|e| panic!("{}", at(e)));
            check_cut_set(&dev.img, &layout, 2).unwrap_or_else(|e| panic!("{}", at(e)));
            // The next install repairs it.
            let mut again = Dev::new(dev.img.clone());
            let done = install(&mut again, &layout, 2, "c", &refs(&c))
                .unwrap_or_else(|e| panic!("{}", at(format!("re-run: {e}"))));
            assert!(matches!(done, Done::Installed | Done::AlreadyInstalled));
            let placed = place(&mut again).unwrap();
            assert_eq!(
                boot_view(&mut again, &layout, placed.default),
                Ok((2, "c".into())),
                "cut {k} {torn:?}"
            );
            let mut both = Dev::new(again.img.clone());
            both.written = dev.written.iter().chain(&again.written).copied().collect();
            only_inside(&pre, &both, &allowed).unwrap_or_else(|e| panic!("{}", at(e)));
            runs += 1;
        }
    }
    assert_eq!(runs, n * 4);
    println!(
        "power-cut sweep: {n} cut points x 4 torn variants = {runs} runs, all outcomes as required"
    );
}

/// Real-size payloads: each role at about half its cap.
fn half_caps(layout: &Layout) -> [Vec<u8>; 5] {
    std::array::from_fn(|i| {
        let len = (layout.bytes[i] / 2) as usize + i * 37;
        if i == 3 {
            let mut v = vec![b'x'; len.min(1000)];
            v.push(b'\n');
            v
        } else {
            vec![(i as u8).wrapping_add(7); len]
        }
    })
}

#[test]
fn the_board_layout_installs_and_reads_back_uncut_at_real_caps() {
    let layout = board_layout();
    let pre = volume_image(&layout);
    let c = half_caps(&layout);
    let allowed = write_set(&pre, 1);
    let mut dev = Dev::new(pre.clone());
    assert_eq!(
        install(&mut dev, &layout, 1, "c", &refs(&c)).unwrap(),
        Done::Installed
    );
    only_inside(&pre, &dev, &allowed).unwrap();
    let placed = place(&mut dev).unwrap();
    assert_eq!(
        boot_view(&mut dev, &layout, placed.default).unwrap(),
        (1, "c".into())
    );
    assert_eq!(
        boot_view(&mut dev, &layout, placed.previous).unwrap(),
        (0, "a".into())
    );
}

/// The cut points of the real-size sweep: every write that is not file data, and the first and
/// last write of each run of consecutive data writes, plus every 997th data write.
fn real_cuts(written: &[u64], data: &HashSet<u64>) -> Vec<usize> {
    let mut out = Vec::new();
    for (k, lba) in written.iter().enumerate() {
        let is_data = data.contains(lba);
        let edge = k == 0
            || k + 1 == written.len()
            || data.contains(&written[k - 1]) != is_data
            || data.contains(&written[k + 1]) != is_data
            || written[k - 1] + 1 != *lba
            || written[k + 1] != lba + 1;
        if !is_data || edge || k % 997 == 0 {
            out.push(k);
        }
    }
    out
}

#[allow(clippy::print_stdout)] // the sweep prints its counts as proof
#[test]
fn power_cut_sweep_at_the_real_caps_cuts_every_metadata_write_and_each_data_run_edge() {
    let layout = board_layout();
    let pre = volume_image(&layout);
    let c = half_caps(&layout);
    let allowed = write_set(&pre, 1);
    let mut uncut = Dev::new(pre.clone());
    install(&mut uncut, &layout, 1, "c", &refs(&c)).unwrap();
    // The region's data sectors.
    let mut probe = Dev::new(pre.clone());
    let v = bootfat::volume(&mut probe).unwrap();
    let lba = place_sets(&mut probe).unwrap()[1];
    let region = read_info(&mut probe, lba).unwrap().unwrap().region;
    let first = v.cluster_lba(region.start);
    let data: HashSet<u64> = (first..first + region.clusters * v.spc).collect();
    let cuts = real_cuts(&uncut.written, &data);
    assert!(cuts.len() < uncut.writes, "the sample is a sample");
    let mut runs = 0;
    for &k in &cuts {
        for torn in Torn::all(k) {
            let mut dev = Dev::new(pre.clone());
            dev.cut = Some((k, torn));
            assert!(install(&mut dev, &layout, 1, "c", &refs(&c)).is_err());
            let at = |e: String| format!("cut {k} {torn:?}: {e}");
            check_bootable(&dev.img, &layout, (1, "c")).unwrap_or_else(|e| panic!("{}", at(e)));
            only_inside(&pre, &dev, &allowed).unwrap_or_else(|e| panic!("{}", at(e)));
            check_cut_set(&dev.img, &layout, 1).unwrap_or_else(|e| panic!("{}", at(e)));
            runs += 1;
        }
    }
    // The re-run from the last metadata cut completes.
    let last = *cuts.last().unwrap();
    let mut dev = Dev::new(pre.clone());
    dev.cut = Some((last, Torn::Random(7)));
    let _ = install(&mut dev, &layout, 1, "c", &refs(&c));
    let mut again = Dev::new(dev.img);
    install(&mut again, &layout, 1, "c", &refs(&c)).unwrap();
    println!(
        "real-cap power-cut sweep: {} of {} writes cut x 4 torn variants = {runs} runs",
        cuts.len(),
        uncut.writes
    );
}

/// The driver's install of `slot` and its default switch, through [`Boot`] on `dev`.
fn driver_run(dev: Dev, slot: &str, payloads: &[&[u8]; 5]) -> (Dev, Result<(), String>) {
    let mut boot = Boot::on(dev).expect("the boot partition is placed");
    let result = boot
        .install_slot(slot, payloads)
        .map(|_| ())
        .and_then(|()| boot.set_pointer(Pointer::Default, slot));
    (boot.into_device(), result)
}

#[test]
fn the_image_is_installed_trialled_confirmed_reused_and_removed_through_the_driver() {
    let layout = test_layout();
    let mut dev = Dev::new(volume_image(&layout));
    let placed = place(&mut dev).expect("every record and pointer is placed");
    assert_eq!(
        boot_view(&mut dev, &layout, placed.default),
        Ok((0, "a".to_string()))
    );

    // Install b and switch the default to it: the previous becomes a.
    let (dev, r) = driver_run(dev, "b", &refs(&payloads_b()));
    r.unwrap();
    let mut dev = dev;
    assert_eq!(
        boot_view(&mut dev, &layout, placed.default),
        Ok((1, "b".to_string()))
    );
    assert_eq!(
        boot_view(&mut dev, &layout, placed.previous),
        Ok((0, "a".to_string()))
    );

    // Install c, arm it as a trial.
    let mut boot = Boot::on(dev).unwrap();
    boot.install_slot("c", &refs(&payloads_c())).unwrap();
    boot.arm_trial("c", 3).unwrap();
    assert_eq!(boot.pointer(Pointer::Trial).unwrap().as_deref(), Some("c"));
    assert!(boot.free_slot("c").is_err());

    // Confirm: the default moves to c, b is kept as the previous.
    boot.clear_trial().unwrap();
    boot.set_pointer(Pointer::Default, "c").unwrap();
    assert_eq!(
        boot.pointer(Pointer::Previous).unwrap().as_deref(),
        Some("b")
    );
    let mut dev = boot.into_device();
    assert_eq!(
        boot_view(&mut dev, &layout, placed.default),
        Ok((2, "c".to_string()))
    );

    // A third install takes the one bay no pointer names (a's, set 0).
    let mut boot = Boot::on(dev).unwrap();
    boot.install_slot("d", &refs(&payloads_a())).unwrap();
    assert_eq!(boot.bay_of("d").unwrap(), 0);
    // Removal of d empties its record and keeps its region; c, the default, cannot be removed.
    boot.free_slot("d").unwrap();
    assert!(boot.bay_of("d").is_err());
    assert!(boot.free_slot("c").is_err());
    let mut dev = boot.into_device();
    let lba = place_sets(&mut dev).unwrap()[0];
    let info = read_info(&mut dev, lba).unwrap().unwrap();
    assert_eq!(info.slot, None);
    assert!(info.region.clusters > 0);
}

fn listing(names: &[&str]) -> Vec<(String, String)> {
    names
        .iter()
        .map(|n| ((*n).to_string(), "00".repeat(32)))
        .collect()
}

/// The declared set of a board with no firmware overlay: the five role files.
fn declared() -> Vec<String> {
    stored_files().iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_manifest_lists_exactly_the_role_files() {
    let five = stored_files();
    assert_eq!(
        refuse_unstored("os-2", &listing(&five), &declared()),
        Ok(())
    );
    let mut reversed = five;
    reversed.reverse();
    assert_eq!(
        refuse_unstored("os-2", &listing(&reversed), &declared()),
        Ok(())
    );
    let mut extra = five.to_vec();
    extra.push("overlays/extra.dtbo");
    assert!(
        refuse_unstored("os-2", &listing(&extra), &declared())
            .unwrap_err()
            .contains("\"overlays/extra.dtbo\"")
    );
    let short = &five[1..];
    assert!(
        refuse_unstored("os-2", &listing(short), &declared())
            .unwrap_err()
            .contains("does not list \"kernel8.img\"")
    );
    let mut readme = five.to_vec();
    readme.push("overlays/README");
    assert!(
        refuse_unstored("os-2", &listing(&readme), &declared())
            .unwrap_err()
            .contains("\"overlays/README\"")
    );
}

#[path = "bays_sweep_tests.rs"]
mod sweeps;

#[test]
fn a_reinstall_of_the_default_with_identical_boot_files_is_accepted_and_writes_nothing() {
    let layout = test_layout();
    let mut dev = Dev::new(volume_image(&layout));
    assert_eq!(
        install(&mut dev, &layout, 0, "a", &refs(&payloads_a())).unwrap(),
        Done::AlreadyInstalled
    );
    assert!(dev.written.is_empty());
}

#[test]
fn a_default_bay_whose_boot_file_differs_is_refused_by_name_and_never_written() {
    let layout = test_layout();
    let mut dev = Dev::new(volume_image(&layout));
    let mut other = payloads_a();
    other[0][0] ^= 1;
    let e = install(&mut dev, &layout, 0, "a", &refs(&other)).unwrap_err();
    assert!(matches!(e, Error::Refused(_)), "{e}");
    assert!(dev.written.is_empty());
}

#[test]
fn a_bay_nothing_names_is_rewritten_in_place_with_a_new_build() {
    let layout = test_layout();
    let mut dev = Dev::new(volume_image(&layout));
    install(&mut dev, &layout, 1, "b", &refs(&payloads_b())).unwrap();
    install(&mut dev, &layout, 2, "c", &refs(&payloads_c())).unwrap();
    // Default bay2 (c), previous bay1 (b): bay0 holds a, named by nothing.
    let pre = dev.img.clone();
    let allowed = write_set(&pre, 0);
    let mut dev = Dev::new(pre.clone());
    let mut newer = payloads_a();
    newer[0] = vec![42; 1900];
    assert_eq!(
        install(&mut dev, &layout, 0, "a", &refs(&newer)).unwrap(),
        Done::Installed
    );
    only_inside(&pre, &dev, &allowed).unwrap();
    assert_eq!(file(&dev.img, 0, "kernel8.img"), newer[0]);
    let placed = place(&mut dev).unwrap();
    assert_eq!(
        boot_view(&mut dev, &layout, placed.default),
        Ok((0, "a".to_string()))
    );
}

/// `img` with every set's record rewritten in the form of layout version 5: `bay0` committed to
/// `slot` (its name's length, the name, each role's length and SHA-256), the others empty (`[0]`).
pub(crate) fn as_layout_5(img: &[u8], slot: &str) -> Vec<u8> {
    let mut dev = Dev::new(img.to_vec());
    let sets = place_sets(&mut dev).unwrap();
    for (set, lba) in sets.into_iter().enumerate() {
        let payload = if set == 0 {
            let mut out = vec![slot.len() as u8];
            out.extend_from_slice(slot.as_bytes());
            for i in 0..5u64 {
                out.extend_from_slice(&(4096 * (i + 1)).to_le_bytes());
                out.extend_from_slice(&[i as u8; 32]);
            }
            out
        } else {
            vec![0]
        };
        Pair::at(lba)
            .write(&mut dev, Kind::BAY_INFO, &payload)
            .unwrap();
    }
    dev.img
}

#[test]
fn a_disk_of_layout_5_is_refused_by_name_by_every_write_and_nothing_is_written() {
    use crate::boot_raw::{BayVolume, Ownership, ownership};
    let layout = test_layout();
    // "ours-a" is six bytes long: its v5 record begins with the byte 6, this layout's version.
    for slot in ["a", "ours-a"] {
        let v5 = as_layout_5(&volume_image(&layout), slot);
        let why = records_version(&mut Dev::new(v5.clone()))
            .unwrap()
            .expect("a layout 5 record is not this layout's");
        assert_eq!(
            why,
            "slots/bay0/bayinfo.rec: its record is in the form before layout version 6 (layout \
             version 5 and before begin with the slot name's length), and this code reads and \
             writes only layout version 6: the boot partition was made for another layout, \
             nothing is written to it"
        );
        // The daemon's root phase hands the node over read only, and the writer does not open it.
        assert_eq!(
            ownership(&mut Dev::new(v5.clone())),
            Ownership::NotOurs(why.clone())
        );
        assert_eq!(Boot::on(Dev::new(v5.clone())).err(), Some(why.clone()));
        let e = BayVolume::on(Dev::new(v5.clone())).err().expect("refused");
        assert!(e.contains("not made for layout version 6"), "{e}");
        // Every write of the installer, called directly, refuses before its first write.
        let mut dev = Dev::new(v5.clone());
        let none = Protected::default();
        let c = payloads_c();
        let p = refs(&c);
        for set in 0..SETS {
            let e = install(&mut dev, &layout, set, "c", &p).unwrap_err();
            assert!(
                e.to_string().contains("layout version 6"),
                "install bay{set}: {e}"
            );
            let e = stage_with(&mut dev, &layout, set, "c", &p, &none).unwrap_err();
            assert!(
                e.to_string().contains("layout version 6"),
                "stage bay{set}: {e}"
            );
            let e = free_set(&mut dev, set, &none).unwrap_err();
            assert!(
                e.to_string().contains("layout version 6"),
                "free bay{set}: {e}"
            );
            let e = make_default(&mut dev, set, &none).unwrap_err();
            assert!(
                e.to_string().contains("no committed record"),
                "default bay{set}: {e}"
            );
            assert!(set_sectors(&mut dev, set).is_err());
        }
        assert!(lay_out_empty_sets(&mut dev, &layout).is_err());
        assert!(discover(&mut dev).is_err());
        assert!(dev.written.is_empty(), "written: {:?}", dev.written);
        assert!(dev.img == v5, "the image changed");
    }
}
