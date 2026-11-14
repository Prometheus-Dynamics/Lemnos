//! Tests of the payload bays. The volume is the real geometry of the board's boot partition: 79 MiB
//! (161,792 sectors), FAT16 with 2 KiB clusters, built by [`crate::fatimage::volume`] and read by the
//! same [`crate::bootfat`] code the boot chain uses.
//!
//! The power-cut sweep runs the install with the test layout (small bays, the same volume): every
//! sector write is a cut point, and each cut is torn four ways. The real bay sizes run once, uncut.

use super::*;
use crate::fatimage::{self, Node};
use crate::twocopy::{self, Sector};

/// The test layout: small bays on the real volume (the sweep needs every write as a cut point).
fn test_layout() -> Layout {
    Layout::new([2048, 1024, 512, 512, 512]).unwrap()
}

/// The board's layout: the kernel as the next build stores it, gzip -9 (10,168,581 bytes), in an
/// 11 MiB bay; dtb 88 KiB; the overlay, the command line and the README 4 KiB each. The initramfs bay
/// is a placeholder of 1 MiB: its size is unmeasured (section 8.3 of the requirements file).
fn board_layout() -> Layout {
    Layout::new([11 * 1024 * 1024, 1024 * 1024, 88 * 1024, 4096, 4096]).unwrap()
}

/// The payloads of slot `a` (lengths below the test layout's sizes).
fn payloads_a() -> [Vec<u8>; 5] {
    [
        vec![1; 1500],
        vec![2; 700],
        vec![3; 300],
        b"console=ttyS0 root=a".to_vec(),
        vec![4; 200],
    ]
}

/// The payloads of slot `c`, the one the tests install.
fn payloads_c() -> [Vec<u8>; 5] {
    [
        vec![9; 2000],
        vec![8; 100],
        vec![7; 400],
        b"c-line".to_vec(),
        vec![6; 300],
    ]
}

/// The payloads padded to the layout's fixed sizes, as a set stores them.
fn stored(p: &[Vec<u8>; 5], layout: &Layout) -> [Vec<u8>; 5] {
    std::array::from_fn(|i| {
        let mut v = p[i].clone();
        v.resize(layout.bytes[i] as usize, ROLES[i].pad);
        v
    })
}

/// A set's record for `slot` with `payloads`: copy 0 is valid, copy 1 is empty.
fn record(slot: &str, p: &[Vec<u8>; 5], layout: &Layout) -> Vec<u8> {
    let _ = layout;
    let mut files = [(0u64, [0u8; 32]); 5];
    for (i, v) in p.iter().enumerate() {
        files[i] = (v.len() as u64, Sha256::digest(v).into());
    }
    let payload = Info {
        slot: slot.to_string(),
        files,
    }
    .encode();
    let mut sector = [0u8; SECTOR];
    twocopy::encode(Kind::BAY_INFO, 1, &payload, &mut sector).unwrap();
    let mut out = vec![0u8; 1024];
    out[..SECTOR].copy_from_slice(&sector);
    out
}

/// A pointer file: two copies of set `set`'s sector.
fn pointer(set: u8) -> Vec<u8> {
    firmware::pointer_file(&format!("bay{set}"))
        .unwrap()
        .to_vec()
}

/// The set directory's files and record for the image.
fn set_node(set: u8, files: &[Vec<u8>; 5], info: Vec<u8>) -> Node {
    Node::Dir {
        name: ["bay0", "bay1", "bay2"][set as usize],
        children: vec![
            Node::File {
                name: "kernel8.img",
                data: files[0].clone(),
            },
            Node::File {
                name: "initramfs.cpio",
                data: files[1].clone(),
            },
            Node::File {
                name: "board.dtb",
                data: files[2].clone(),
            },
            Node::File {
                name: "cmdline.txt",
                data: files[3].clone(),
            },
            Node::Dir {
                name: "overlays",
                children: vec![Node::File {
                    name: "lemnos-board.dtbo",
                    data: files[4].clone(),
                }],
            },
            Node::File {
                name: "bayinfo.rec",
                data: info,
            },
        ],
    }
}

/// The boot partition before any install: `bay0` holds slot `a`; both pointers name `bay0`; the
/// trial record is empty unless `trial` is given; `bay1` and `bay2` are empty. The volume is the
/// board's 79 MiB geometry.
fn volume_image(layout: &Layout, trial: Option<&str>) -> Vec<u8> {
    let a = stored(&payloads_a(), layout);
    let empty: [Vec<u8>; 5] = std::array::from_fn(|i| vec![ROLES[i].pad; layout.bytes[i] as usize]);
    let trial_record = match trial {
        None => vec![0u8; 1024],
        Some(slot) => {
            let mut sector = [0u8; SECTOR];
            twocopy::encode(Kind::TRIAL, 1, format!("{slot} 2").as_bytes(), &mut sector).unwrap();
            let mut out = vec![0u8; 1024];
            out[..SECTOR].copy_from_slice(&sector);
            out
        }
    };
    let root = vec![
        Node::File {
            name: "default.txt",
            data: pointer(0),
        },
        Node::File {
            name: "previous.txt",
            data: pointer(0),
        },
        Node::File {
            name: "trial.txt",
            data: pointer(0),
        },
        Node::File {
            name: "TRIAL.REC",
            data: trial_record,
        },
        Node::File {
            name: "REASON.REC",
            data: vec![0u8; 1024],
        },
        Node::File {
            name: "EPISODES.REC",
            data: vec![0u8; 1024],
        },
        Node::Dir {
            name: "slots",
            children: vec![
                set_node(0, &a, record("a", &a, layout)),
                set_node(1, &empty, vec![0u8; 1024]),
                set_node(2, &empty, vec![0u8; 1024]),
            ],
        },
    ];
    fatimage::volume(&root, 161_792)
}

/// The board's partition size in sectors (79 MiB).
const BOARD_SECTORS: u64 = 161_792;

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

fn refs(p: &[Vec<u8>; 5]) -> [&[u8]; 5] {
    std::array::from_fn(|i| p[i].as_slice())
}

/// Sectors that an install of any set may write: every bay file, every bay record, both pointers.
fn allowed_sectors(placed: &Placed, layout: &Layout) -> std::collections::HashSet<u64> {
    let mut out = std::collections::HashSet::new();
    for set in 0..SETS as usize {
        for (i, lba) in placed.files[set].iter().enumerate() {
            for k in 0..layout.bytes[i] / SECTOR as u64 {
                out.insert(lba + k);
            }
        }
        out.insert(placed.info[set]);
        out.insert(placed.info[set] + 1);
    }
    for lba in [placed.default, placed.previous] {
        out.insert(lba);
        out.insert(lba + 1);
    }
    out
}

/// The boot chain's view after a cut: the default and the previous read as the firmware reads them.
/// The default must be the old slot or the new one, each with its record and hashes, and the previous
/// must be the old slot.
fn check_bootable(img: &[u8], layout: &Layout) -> Result<(), String> {
    let mut dev = Dev::new(img.to_vec());
    let placed = place(&mut dev, layout)?;
    let d = boot_view(&mut dev, layout, placed.default)?;
    if d != (0, "a".to_string()) && d != (2, "c".to_string()) {
        return Err(format!(
            "default boots {d:?}: neither the old nor the new slot"
        ));
    }
    let p = boot_view(&mut dev, layout, placed.previous)?;
    if p != (0, "a".to_string()) {
        return Err(format!("previous boots {p:?}, not the old default"));
    }
    Ok(())
}

/// The sectors outside the install's write set are byte-identical to the image before it, and every
/// sector written lies inside the write set.
fn check_metadata(pre: &[u8], dev: &Dev, layout: &Layout) -> Result<(), String> {
    let mut reader = Dev::new(dev.img.clone());
    let placed = place(&mut reader, layout)?;
    let allowed = allowed_sectors(&placed, layout);
    if let Some(bad) = dev.written.iter().find(|l| !allowed.contains(l)) {
        return Err(format!(
            "the install wrote sector {bad}, outside its bays and pointers"
        ));
    }
    for lba in 0..BOARD_SECTORS {
        if allowed.contains(&lba) {
            continue;
        }
        let at = lba as usize * SECTOR;
        if pre[at..at + SECTOR] != dev.img[at..at + SECTOR] {
            return Err(format!(
                "sector {lba} changed outside the write set (metadata or data)"
            ));
        }
    }
    Ok(())
}

#[test]
fn the_layout_rejects_a_size_that_is_not_whole_sectors() {
    assert!(Layout::new([2000, 1024, 512, 512, 512]).is_err());
    assert!(Layout::new([0, 1024, 512, 512, 512]).is_err());
    assert!(Layout::new([2048, 1024, 512, 512, 512]).is_ok());
}

#[test]
fn the_board_volume_is_fat16_with_two_kib_clusters_and_the_locator_places_its_files() {
    assert_eq!(fatimage::cluster_sectors(BOARD_SECTORS), 4);
    let (fat, clusters, _) = fatimage::geometry(BOARD_SECTORS, 4);
    // Two FAT copies of 158 sectors (the board's own image has the same two): 40,360 clusters.
    assert_eq!((fat, clusters), (158, 40_360));
    let layout = test_layout();
    let mut dev = Dev::new(volume_image(&layout, None));
    let placed = place(&mut dev, &layout).expect("every bay file, record and pointer is placed");
    assert_eq!(placed.files.len(), SETS as usize);
    assert!(placed.default > 0 && placed.previous > 0 && placed.trial > 0);
}

#[test]
fn an_install_into_a_free_set_round_trips_and_switches_the_default() {
    let layout = test_layout();
    let mut dev = Dev::new(volume_image(&layout, None));
    let c = stored(&payloads_c(), &layout);
    assert_eq!(
        install(&mut dev, &layout, 2, "c", &refs(&c)).unwrap(),
        Done::Installed
    );
    let placed = place(&mut dev, &layout).unwrap();
    assert_eq!(
        boot_view(&mut dev, &layout, placed.default).unwrap(),
        (2, "c".into())
    );
    assert_eq!(
        boot_view(&mut dev, &layout, placed.previous).unwrap(),
        (0, "a".into())
    );
}

#[test]
fn an_install_into_the_default_bay_is_refused_and_writes_nothing() {
    let layout = test_layout();
    let mut dev = Dev::new(volume_image(&layout, None));
    let c = stored(&payloads_c(), &layout);
    let err = install(&mut dev, &layout, 0, "c", &refs(&c)).unwrap_err();
    assert!(
        matches!(err, Error::Refused(ref why) if why.contains("default")),
        "{err}"
    );
    assert!(dev.written.is_empty());
}

/// The caller's protected sets are the only thing that names the previous's and the trial's bays (the
/// U-Boot form's environment): an install into either is refused by what it is, and writes nothing.
#[test]
fn an_install_into_the_previous_or_the_trial_bay_is_refused_and_writes_nothing() {
    let layout = test_layout();
    let protected = Protected {
        default: Some(0),
        previous: Some(1),
        trial: Some(2),
        pending: None,
    };
    for (set, word) in [(1u8, "previous"), (2, "trial")] {
        let mut dev = Dev::new(volume_image(&layout, None));
        let c = stored(&payloads_c(), &layout);
        let err = stage_with(&mut dev, &layout, set, "c", &refs(&c), None, &protected).unwrap_err();
        assert!(
            matches!(err, Error::Refused(ref why) if why.contains(word)),
            "{err}"
        );
        assert!(dev.written.is_empty(), "bay{set}: {:?}", dev.written);
        let err = free_set(&mut dev, &layout, set, &protected).unwrap_err();
        assert!(matches!(err, Error::Refused(_)), "{err}");
        assert!(dev.written.is_empty());
    }
}

#[test]
fn an_install_while_a_trial_is_pending_is_refused_with_the_reason_and_writes_nothing() {
    let layout = test_layout();
    let mut dev = Dev::new(volume_image(&layout, Some("a")));
    let c = stored(&payloads_c(), &layout);
    let err = install(&mut dev, &layout, 2, "c", &refs(&c)).unwrap_err();
    match err {
        Error::Refused(ref why) => {
            assert!(why.contains("on trial"), "{why}");
            assert!(why.contains("nothing was written"), "{why}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(dev.written.is_empty());
}

#[test]
fn a_payload_larger_than_its_bay_is_refused_by_name_and_writes_nothing() {
    let layout = test_layout();
    let mut dev = Dev::new(volume_image(&layout, None));
    let mut big = payloads_c();
    big[0] = vec![5; 3000];
    let b = stored(&big, &layout);
    let err = install(
        &mut dev,
        &layout,
        2,
        "c",
        &[&big[0], &big[1], &big[2], &big[3], &big[4]],
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::Refused(ref why) if why.contains("kernel")),
        "{err}"
    );
    assert!(dev.written.is_empty());
    let _ = b;
}

#[allow(clippy::print_stdout)] // the sweep prints its counts as proof
#[test]
fn power_cut_at_every_sector_write_and_each_torn_variant_leaves_a_bootable_default() {
    let layout = test_layout();
    let pre = volume_image(&layout, None);
    let c = stored(&payloads_c(), &layout);
    let mut uncut = Dev::new(pre.clone());
    assert_eq!(
        install(&mut uncut, &layout, 2, "c", &refs(&c)).unwrap(),
        Done::Installed
    );
    let n = uncut.writes;
    assert!(n > 0, "the install writes");
    let variants = |k: usize| {
        [
            Torn::Old,
            Torn::New,
            Torn::FirstHalfNew,
            Torn::Random(k as u64 + 1),
        ]
    };
    let mut runs = 0usize;
    for k in 0..n {
        for torn in variants(k) {
            let mut dev = Dev::new(pre.clone());
            dev.cut = Some((k, torn));
            let r = install(&mut dev, &layout, 2, "c", &refs(&c));
            assert!(r.is_err(), "cut {k} {torn:?}: the install reported success");
            check_bootable(&dev.img, &layout).unwrap_or_else(|e| panic!("cut {k} {torn:?}: {e}"));
            check_metadata(&pre, &dev, &layout).unwrap_or_else(|e| panic!("cut {k} {torn:?}: {e}"));
            // The re-run completes from the cut state.
            let mut again = Dev::new(dev.img.clone());
            let done = install(&mut again, &layout, 2, "c", &refs(&c))
                .unwrap_or_else(|e| panic!("cut {k} {torn:?}: re-run failed: {e}"));
            assert!(matches!(done, Done::Installed | Done::AlreadyInstalled));
            check_bootable(&again.img, &layout)
                .unwrap_or_else(|e| panic!("cut {k} {torn:?} after re-run: {e}"));
            let placed = place(&mut again, &layout).unwrap();
            assert_eq!(
                boot_view(&mut again, &layout, placed.default).unwrap(),
                (2, "c".into())
            );
            runs += 1;
        }
    }
    assert_eq!(runs, n * 4);
    println!(
        "power-cut sweep: {n} cut points x 4 torn variants = {runs} runs, all outcomes as required"
    );
}

#[test]
fn the_board_layout_installs_and_reads_back_uncut_at_real_bay_sizes() {
    let layout = board_layout();
    let a = stored(&payloads_a(), &layout);
    let mut dev = Dev::new(volume_image(&layout, None));
    let c: [Vec<u8>; 5] =
        std::array::from_fn(|i| vec![(i as u8).wrapping_add(7); (layout.bytes[i] / 2) as usize]);
    let _ = a;
    assert_eq!(
        install(&mut dev, &layout, 1, "c", &refs(&c)).unwrap(),
        Done::Installed
    );
    let placed = place(&mut dev, &layout).unwrap();
    assert_eq!(
        boot_view(&mut dev, &layout, placed.default).unwrap(),
        (1, "c".into())
    );
    assert_eq!(
        boot_view(&mut dev, &layout, placed.previous).unwrap(),
        (0, "a".into())
    );
}

#[test]
fn the_install_writes_fourteen_sectors_nine_payload_one_record_two_previous_two_default() {
    let layout = test_layout();
    let c = stored(&payloads_c(), &layout);
    let mut dev = Dev::new(volume_image(&layout, None));
    assert_eq!(
        install(&mut dev, &layout, 2, "c", &refs(&c)).unwrap(),
        Done::Installed
    );
    assert_eq!(dev.writes, 14);
}

/// The real bay sizes' sweep: every boundary write (the first and last sector of each payload file
/// of the set being installed, each sector of its record and of both pointers), and a deterministic
/// stride through the other payload writes, every cut torn four ways. The uncut install's writes are
/// the candidates: the cut indices are chosen from its log, so the sample follows the install.
#[allow(clippy::print_stdout)] // the sweep prints its counts as proof
#[test]
fn power_cut_sweep_at_the_real_bay_sizes_is_sampled_and_covers_every_boundary() {
    let layout = board_layout();
    let pre = volume_image(&layout, None);
    let c = stored(&payloads_c(), &layout);
    let mut uncut = Dev::new(pre.clone());
    assert_eq!(
        install(&mut uncut, &layout, 2, "c", &refs(&c)).unwrap(),
        Done::Installed
    );
    let n = uncut.writes;
    let placed = place(&mut Dev::new(pre.clone()), &layout).unwrap();
    let mut boundary_lbas = std::collections::HashSet::new();
    for (i, first) in placed.files[2].iter().enumerate() {
        boundary_lbas.insert(*first);
        boundary_lbas.insert(first + layout.bytes[i] / SECTOR as u64 - 1);
    }
    boundary_lbas.extend([placed.info[2], placed.info[2] + 1]);
    boundary_lbas.extend([
        placed.default,
        placed.default + 1,
        placed.previous,
        placed.previous + 1,
    ]);
    let mut cuts: Vec<usize> = Vec::new();
    let mut payload = Vec::new();
    for (idx, lba) in uncut.written.iter().enumerate() {
        if boundary_lbas.contains(lba) {
            cuts.push(idx);
        } else {
            payload.push(idx);
        }
    }
    let boundary_count = cuts.len();
    let stride = (payload.len() / 100).max(1);
    cuts.extend(payload.iter().step_by(stride));
    cuts.sort_unstable();
    cuts.dedup();
    let variants = |k: usize| {
        [
            Torn::Old,
            Torn::New,
            Torn::FirstHalfNew,
            Torn::Random(k as u64 + 1),
        ]
    };
    let mut runs = 0usize;
    for &k in &cuts {
        for torn in variants(k) {
            let mut dev = Dev::new(pre.clone());
            dev.cut = Some((k, torn));
            assert!(
                install(&mut dev, &layout, 2, "c", &refs(&c)).is_err(),
                "cut {k} {torn:?}: reported success"
            );
            check_bootable(&dev.img, &layout).unwrap_or_else(|e| panic!("cut {k} {torn:?}: {e}"));
            check_metadata(&pre, &dev, &layout).unwrap_or_else(|e| panic!("cut {k} {torn:?}: {e}"));
            let mut again = Dev::new(dev.img.clone());
            install(&mut again, &layout, 2, "c", &refs(&c))
                .unwrap_or_else(|e| panic!("cut {k} {torn:?}: re-run failed: {e}"));
            let placed = place(&mut again, &layout).unwrap();
            assert_eq!(
                boot_view(&mut again, &layout, placed.default).unwrap(),
                (2, "c".into())
            );
            runs += 1;
        }
    }
    println!(
        "real-size power-cut sweep: {n} writes in the install; {boundary_count} boundary cut points, \
         {} stride cut points ({} payload writes, stride {stride}); {} cut points x 4 torn variants = {runs} runs, all outcomes as required",
        cuts.len() - boundary_count,
        payload.len(),
        cuts.len()
    );
    assert_eq!(runs, cuts.len() * 4);
}

// ---- The driver path (item 1). The operations the driver makes (install, the default switch, the
// trial, its confirmation, the removal) are made through `boot_raw::Boot` on the image. Each sweep
// run goes through the same calls; the module functions above are not used here.

use crate::boot_raw::{Boot, BootRaw, Pointer};
use crate::bootimage::{self, Family, Fixed, Parts};

/// The payloads of slot `b`.
fn payloads_b() -> [Vec<u8>; 5] {
    [
        vec![7; 1500],
        vec![8; 300],
        vec![9; 200],
        b"console=serial0 root=lemnos-b".to_vec(),
        vec![10; 100],
    ]
}

/// The Pi firmware's parts for an image: the generated root text and the README, as the boot
/// partition's builder gives them.
fn boot_parts(layout: &Layout) -> Parts {
    Parts {
        family: Family::Tryboot,
        layout: *layout,
        root: vec![
            (
                "config.txt",
                b"# Generated\nkernel_watchdog_timeout=15\n".to_vec(),
            ),
            ("tryboot.txt", b"# Generated\n".to_vec()),
            ("slot-layout.toml", b"version = 5\n".to_vec()),
        ],
        fixed: vec![Fixed {
            path: "overlays/README",
            bytes: b"Generated: os_prefix applies to overlays.\n".to_vec(),
            size: 42,
        }],
    }
}

/// The driver's install of `slot` and its default switch, through [`Boot`] on `dev`. The device comes
/// back with the result, so the caller reads what was written.
fn driver_run(dev: Dev, slot: &str, payloads: &[&[u8]; 5]) -> (Dev, Result<(), String>) {
    let mut boot = Boot::on(dev).expect("the boot partition is placed");
    let result = boot
        .install_slot(slot, payloads)
        .map(|_| ())
        .and_then(|()| boot.set_pointer(Pointer::Default, slot));
    (boot.into_device(), result)
}

/// [`check_bootable`] for an install whose new slot sits in `new` (the bay set the install took).
fn check_bootable_new(img: &[u8], layout: &Layout, new: u8) -> Result<(), String> {
    let mut dev = Dev::new(img.to_vec());
    let placed = place(&mut dev, layout)?;
    let d = boot_view(&mut dev, layout, placed.default)?;
    if d != (0, "a".to_string()) && d != (new, "c".to_string()) {
        return Err(format!(
            "default boots {d:?}: neither the old nor the new slot"
        ));
    }
    let p = boot_view(&mut dev, layout, placed.previous)?;
    if p != (0, "a".to_string()) {
        return Err(format!("previous boots {p:?}, not the old default"));
    }
    Ok(())
}

#[test]
fn the_image_is_installed_trialled_confirmed_reused_and_removed_through_the_driver() {
    let layout = test_layout();
    let image = bootimage::build(
        &boot_parts(&layout),
        BOARD_SECTORS * 512,
        "a",
        &refs(&payloads_a()),
    )
    .expect("the image builds at the board's geometry");
    // The every-boot check on the image: the default's bay hashes to its record.
    let mut dev = Dev::new(image);
    let placed = place(&mut dev, &layout).expect("every bay file, record and pointer is placed");
    assert_eq!(
        boot_view(&mut dev, &layout, placed.default),
        Ok((0, "a".to_string()))
    );

    // Install b and switch the default to it: the previous becomes a.
    let mut boot = Boot::on(dev).unwrap();
    assert_eq!(boot.bay_of("a").unwrap(), 0);
    boot.install_slot("b", &refs(&payloads_b())).unwrap();
    boot.set_pointer(Pointer::Default, "b").unwrap();
    let mut dev = boot.into_device();
    assert_eq!(
        boot_view(&mut dev, &layout, placed.default),
        Ok((1, "b".to_string()))
    );
    assert_eq!(
        boot_view(&mut dev, &layout, placed.previous),
        Ok((0, "a".to_string()))
    );

    // Install c, arm it as a trial: the trial pointer names its bay and the record its attempts.
    let mut boot = Boot::on(dev).unwrap();
    boot.install_slot("c", &refs(&payloads_c())).unwrap();
    boot.arm_trial("c", 3).unwrap();
    assert_eq!(boot.pointer(Pointer::Trial).unwrap().as_deref(), Some("c"));
    assert_eq!(
        boot.trial().unwrap().map(|t| (t.slot, t.attempts)),
        Some(("c".to_string(), 3))
    );
    // While the trial is pending, the default still boots b, and removing the trial's slot is refused.
    assert!(boot.free_slot("c").is_err());

    // Confirm: the record goes, the default moves to c, b is kept as the previous.
    boot.clear_trial().unwrap();
    boot.set_pointer(Pointer::Default, "c").unwrap();
    assert_eq!(
        boot.pointer(Pointer::Default).unwrap().as_deref(),
        Some("c")
    );
    assert_eq!(
        boot.pointer(Pointer::Previous).unwrap().as_deref(),
        Some("b")
    );
    let mut dev = boot.into_device();
    assert_eq!(
        boot_view(&mut dev, &layout, placed.default),
        Ok((2, "c".to_string()))
    );
    assert_eq!(
        boot_view(&mut dev, &layout, placed.previous),
        Ok((1, "b".to_string()))
    );

    // A third install takes the one bay no pointer names (a's, set 0), not b's or c's.
    let mut boot = Boot::on(dev).unwrap();
    boot.install_slot("d", &refs(&payloads_a())).unwrap();
    assert_eq!(boot.bay_of("d").unwrap(), 0);
    // Removal of d frees its bay by a record write only; c, the default, cannot be removed.
    boot.free_slot("d").unwrap();
    assert!(boot.bay_of("d").is_err());
    assert!(boot.free_slot("c").is_err());
    assert_eq!(
        boot.pointer(Pointer::Default).unwrap().as_deref(),
        Some("c")
    );
}

#[allow(clippy::print_stderr)] // the test prints its measurement
#[allow(clippy::print_stdout)] // the sweep prints its counts as proof
#[test]
fn driver_path_power_cut_sweep_keeps_a_bootable_default_and_reruns() {
    let layout = test_layout();
    let pre = volume_image(&layout, None);
    let (uncut, result) = driver_run(Dev::new(pre.clone()), "c", &refs(&payloads_c()));
    result.expect("the uncut install and switch complete");
    let writes = uncut.writes;
    // The install's writes: payload sectors, the record, the previous and default pointers.
    assert!(writes > 10, "the driver path makes {writes} writes");
    let new = Boot::on(Dev::new(uncut.img.clone()))
        .unwrap()
        .bay_of("c")
        .unwrap();
    let mut runs = 0;
    for cut in 0..writes {
        for torn in [
            Torn::Old,
            Torn::New,
            Torn::FirstHalfNew,
            Torn::Random(cut as u64 + 1),
        ] {
            let mut dev = Dev::new(pre.clone());
            dev.cut = Some((cut, torn));
            let (after, result) = driver_run(dev, "c", &refs(&payloads_c()));
            assert!(
                result.is_err(),
                "cut {cut} {torn:?}: the cut write must fail the install"
            );
            check_bootable_new(&after.img, &layout, new)
                .unwrap_or_else(|e| panic!("cut {cut} {torn:?}: {e}"));
            check_metadata(&pre, &after, &layout)
                .unwrap_or_else(|e| panic!("cut {cut} {torn:?}: {e}"));
            // The re-run completes and leaves c as the default, with a as the previous.
            let (rerun, again) = driver_run(Dev::new(after.img.clone()), "c", &refs(&payloads_c()));
            again.unwrap_or_else(|e| panic!("cut {cut} {torn:?}: the re-run failed: {e}"));
            check_bootable_new(&rerun.img, &layout, new)
                .unwrap_or_else(|e| panic!("cut {cut} {torn:?}: after the re-run: {e}"));
            let mut check = Dev::new(rerun.img.clone());
            let placed = place(&mut check, &layout).unwrap();
            assert_eq!(
                boot_view(&mut check, &layout, placed.default),
                Ok((new, "c".to_string()))
            );
            runs += 1;
        }
    }
    eprintln!("driver-path power-cut sweep: {writes} writes, {runs} runs (4 torn variants each)");
    assert_eq!(runs, writes * 4);
}

fn listing(names: &[&str]) -> Vec<(String, String)> {
    names
        .iter()
        .map(|n| ((*n).to_string(), "00".repeat(32)))
        .collect()
}

#[test]
fn a_manifest_lists_exactly_the_role_files() {
    let five = stored_files();
    assert_eq!(refuse_unstored("os-2", &listing(&five)), Ok(()));
    // The order of the list does not matter.
    let mut reversed = five;
    reversed.reverse();
    assert_eq!(refuse_unstored("os-2", &listing(&reversed)), Ok(()));
    // An extra entry, by name.
    let mut extra = five.to_vec();
    extra.push("overlays/extra.dtbo");
    assert_eq!(
        refuse_unstored("os-2", &listing(&extra)).unwrap_err(),
        "os-2: boot.files lists \"overlays/extra.dtbo\", which this board's boot partition does not store (it stores: kernel8.img, initramfs.cpio, board.dtb, cmdline.txt, overlays/lemnos-board.dtbo): nothing was written, the default keeps booting"
    );
    // A role file left out, by name.
    let short = &five[1..];
    assert!(
        refuse_unstored("os-2", &listing(short))
            .unwrap_err()
            .contains("does not list \"kernel8.img\"")
    );
    // The README the board writes itself is not a role file.
    let mut readme = five.to_vec();
    readme.push("overlays/README");
    assert!(
        refuse_unstored("os-2", &listing(&readme))
            .unwrap_err()
            .contains("\"overlays/README\"")
    );
}

#[path = "bays_sweep_tests.rs"]
mod sweeps;
