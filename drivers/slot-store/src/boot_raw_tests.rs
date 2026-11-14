use super::*;
use crate::bootfat::tests::{Entry, build, data_start};
use crate::firmware;
use crate::twocopy::{SECTOR, Sector, precreate};

/// A whole partition device: writes are counted, can be cut after `cut.0` full sector writes (the
/// next write lands its first `cut.1` bytes), and every later write fails and changes nothing.
#[derive(Clone)]
struct Cut {
    image: Vec<u8>,
    writes: usize,
    cut: Option<(usize, usize)>,
    dead: bool,
}

impl Cut {
    fn new(image: Vec<u8>) -> Self {
        Self {
            image,
            writes: 0,
            cut: None,
            dead: false,
        }
    }
}

impl Sectors for Cut {
    fn read(&mut self, lba: u64, buf: &mut Sector) -> io::Result<()> {
        let at = lba as usize * SECTOR;
        buf.copy_from_slice(&self.image[at..at + SECTOR]);
        Ok(())
    }

    fn write(&mut self, lba: u64, buf: &Sector) -> io::Result<()> {
        if self.dead {
            return Err(io::Error::other("power is off"));
        }
        let at = lba as usize * SECTOR;
        if let Some((_, tear)) = self.cut.filter(|&(n, _)| self.writes == n) {
            self.image[at..at + tear].copy_from_slice(&buf[..tear]);
            self.dead = true;
            self.writes += 1;
            return Err(io::Error::other("power cut mid-write"));
        }
        self.image[at..at + SECTOR].copy_from_slice(buf);
        self.writes += 1;
        Ok(())
    }
}

/// A FAT16 partition with both records pre-created: the trial holds `os-1 3`, the reason is empty.
/// Returns the image and the placed records.
fn partition() -> (Vec<u8>, Records) {
    let mut vol = build(
        false,
        1,
        16_384,
        &[
            Entry {
                name: TRIAL_FILE,
                size: 1024,
                chain: vec![3, 4],
            },
            Entry {
                name: REASON_FILE,
                size: 1024,
                chain: vec![5, 6],
            },
            Entry {
                name: EPISODES_FILE,
                size: 1024,
                chain: vec![7, 8],
            },
        ],
    );
    let records = Records::place(&mut vol).unwrap();
    let (trial_lba, reason_lba) = records.lbas();
    let made = precreate(&[
        (Kind::TRIAL, b"os-1 3"),
        (Kind::REASON, b""),
        (Kind::EPISODES, b""),
    ])
    .unwrap();
    for ((_, bytes), lba) in made
        .iter()
        .zip([trial_lba, reason_lba, records.episodes_lba()])
    {
        let at = lba as usize * SECTOR;
        vol.0[at..at + bytes.len()].copy_from_slice(bytes);
    }
    (vol.0, records)
}

/// Every byte outside the two sectors of the record at `lba` is the same as in `before`.
fn only_that_record_changed(before: &[u8], after: &[u8], lba: u64) {
    let lo = lba as usize * SECTOR;
    let hi = lo + 2 * SECTOR;
    assert_eq!(after[..lo], before[..lo], "bytes before the record changed");
    assert_eq!(after[hi..], before[hi..], "bytes after the record changed");
}

#[test]
fn the_records_are_placed_at_their_files_sectors() {
    let (_, records) = partition();
    let (trial, reason) = records.lbas();
    assert_eq!(trial, data_start(false, 16_384, 1) + (3 - 2));
    assert_eq!(reason, data_start(false, 16_384, 1) + (5 - 2));
    let mut dev = Cut::new(partition().0);
    assert_eq!(
        records.trial(&mut dev).unwrap(),
        Some(Trial {
            slot: "os-1".into(),
            attempts: 3
        })
    );
    assert_eq!(records.reason(&mut dev).unwrap(), None);
}

#[test]
fn a_record_refused_at_placement_names_its_file() {
    // A reason file that is not where the image builder put it: fragmented, refused by name.
    let mut vol = build(
        false,
        1,
        16_384,
        &[
            Entry {
                name: TRIAL_FILE,
                size: 1024,
                chain: vec![3, 4],
            },
            Entry {
                name: REASON_FILE,
                size: 1024,
                chain: vec![5, 9],
            },
        ],
    );
    assert!(matches!(
        Records::place(&mut vol),
        Err(bootfat::Refusal::Fragmented)
    ));
}

#[test]
fn decode_takes_only_a_well_formed_trial() {
    assert_eq!(
        decode_trial(b"os-2 2"),
        Some(Trial {
            slot: "os-2".into(),
            attempts: 2
        })
    );
    assert_eq!(decode_trial(b""), None, "empty is no trial");
    assert_eq!(decode_trial(b"os-2 9"), None, "attempts past the maximum");
    assert_eq!(decode_trial(b"os-2"), None, "no attempts");
    assert_eq!(decode_trial(b"bad id 1"), None, "an invalid slot id");
    assert_eq!(decode_trial(&[0xFF, 0x20, b'1']), None, "not UTF-8");
}

#[test]
fn an_invalid_trial_is_refused_before_any_write() {
    let (image, records) = partition();
    let mut dev = Cut::new(image);
    assert!(records.set_trial(&mut dev, "bad id", 2).is_err());
    assert!(records.set_trial(&mut dev, "os-2", 4).is_err());
    assert_eq!(dev.writes, 0, "a refused update writes nothing");
}

/// The update under test is one sector write. A cut at it, at every tear length that matters,
/// leaves the old trial or the new one, and nothing outside the record changes.
#[test]
fn a_cut_at_every_point_of_a_trial_update_leaves_old_or_new() {
    let old = Trial {
        slot: "os-1".into(),
        attempts: 3,
    };
    let new = Trial {
        slot: "os-2".into(),
        attempts: 2,
    };
    let (image, records) = partition();
    let (trial_lba, _) = records.lbas();

    let mut dev = Cut::new(image.clone());
    records
        .set_trial(&mut dev, &new.slot, new.attempts)
        .unwrap();
    let total = dev.writes;
    assert_eq!(total, 1, "a trial update is one sector write");

    for n in 0..=total {
        for tear in [0, 1, 17, 255, 507, 508, 511, SECTOR] {
            let mut dev = Cut::new(image.clone());
            dev.cut = Some((n, tear));
            let _ = records.set_trial(&mut dev, &new.slot, new.attempts);
            let mut after = Cut::new(dev.image.clone());
            let got = records
                .trial(&mut after)
                .unwrap()
                .expect("a trial is pending");
            assert!(
                got == old || got == new,
                "cut {n} tear {tear}: read {got:?}"
            );
            if n < total && tear < SECTOR {
                assert_eq!(got, old, "a torn sector must not be taken (tear {tear})");
            }
            only_that_record_changed(&image, &dev.image, trial_lba);
        }
    }
}

#[test]
fn a_cut_at_every_point_of_a_clear_leaves_the_trial_or_none() {
    let (image, records) = partition();
    let (trial_lba, _) = records.lbas();
    let mut dev = Cut::new(image.clone());
    records.clear_trial(&mut dev).unwrap();
    let total = dev.writes;
    for n in 0..=total {
        for tear in [0, 1, 255, 511, SECTOR] {
            let mut dev = Cut::new(image.clone());
            dev.cut = Some((n, tear));
            let _ = records.clear_trial(&mut dev);
            let mut after = Cut::new(dev.image.clone());
            let got = records.trial(&mut after).unwrap();
            let old = Some(Trial {
                slot: "os-1".into(),
                attempts: 3,
            });
            assert!(
                got == old || got.is_none(),
                "cut {n} tear {tear}: read {got:?}"
            );
            only_that_record_changed(&image, &dev.image, trial_lba);
        }
    }
}

#[test]
fn a_cut_at_every_point_of_a_reason_update_leaves_old_or_new() {
    let (image, records) = partition();
    let (_, reason_lba) = records.lbas();
    let mut dev = Cut::new(image.clone());
    records
        .set_reason(&mut dev, "boot did not reach the daemon")
        .unwrap();
    let total = dev.writes;
    for n in 0..=total {
        for tear in [0, 17, 508, SECTOR] {
            let mut dev = Cut::new(image.clone());
            dev.cut = Some((n, tear));
            let _ = records.set_reason(&mut dev, "boot did not reach the daemon");
            let mut after = Cut::new(dev.image.clone());
            let got = records.reason(&mut after).unwrap();
            assert!(
                got.is_none() || got.as_deref() == Some("boot did not reach the daemon"),
                "cut {n} tear {tear}: read {got:?}"
            );
            only_that_record_changed(&image, &dev.image, reason_lba);
        }
    }
}

/// A run of updates, each cut at its one write and then recovered: the trial is always the last
/// one that completed, or the one being written, and each recovery writes on the medium as it is.
#[test]
fn updates_survive_a_cut_at_each_step_and_recover() {
    let (mut image, records) = partition();
    let mut committed = Some(("os-1".to_string(), 3u8));
    for step in 0..12u8 {
        let slot = format!("os-{}", 2 + step % 4);
        let attempts = step % 4;
        let tear = (usize::from(step) * 97) % (SECTOR + 1);
        let mut dev = Cut::new(image.clone());
        dev.cut = Some((0, tear));
        let result = records.set_trial(&mut dev, &slot, attempts);
        image = dev.image;
        let mut after = Cut::new(image.clone());
        let got = records
            .trial(&mut after)
            .unwrap()
            .map(|t| (t.slot, t.attempts));
        let new = Some((slot.clone(), attempts));
        match result {
            Ok(()) => {
                assert_eq!(got, new);
                committed = new;
            }
            Err(_) => {
                assert!(
                    got == committed || got == new,
                    "step {step}: cut left {got:?}, neither {committed:?} nor {new:?}"
                );
                let mut dev = Cut::new(image.clone());
                records.set_trial(&mut dev, &slot, attempts).unwrap();
                image = dev.image;
                committed = new;
            }
        }
    }
}

#[test]
fn an_episodes_record_round_trips_and_a_cut_leaves_old_or_new() {
    let (image, records) = partition();
    let mut dev = Cut::new(image.clone());
    assert_eq!(records.episodes(&mut dev).unwrap(), None);
    records.set_episodes(&mut dev, "os-1", 1).unwrap();
    assert_eq!(
        records.episodes(&mut dev).unwrap(),
        Some(("os-1".into(), 1))
    );
    records.set_episodes(&mut dev, "os-1", 2).unwrap();
    assert_eq!(
        records.episodes(&mut dev).unwrap(),
        Some(("os-1".into(), 2))
    );
    records.clear_episodes(&mut dev).unwrap();
    assert_eq!(records.episodes(&mut dev).unwrap(), None);

    // A cut at every point of the update: old (1 episode) or new (2), never neither.
    let old = Some(("os-1".to_string(), 1u8));
    let new = Some(("os-1".to_string(), 2u8));
    let mut base = Cut::new(image.clone());
    records.set_episodes(&mut base, "os-1", 1).unwrap();
    let start = base.image.clone();
    let mut run = Cut::new(start.clone());
    records.set_episodes(&mut run, "os-1", 2).unwrap();
    for n in 0..=run.writes {
        for tear in [0, 1, 255, 511, SECTOR] {
            let mut dev = Cut::new(start.clone());
            dev.cut = Some((n, tear));
            let _ = records.set_episodes(&mut dev, "os-1", 2);
            let mut after = Cut::new(dev.image.clone());
            let got = records.episodes(&mut after).unwrap();
            assert!(got == old || got == new, "cut {n} tear {tear}: {got:?}");
        }
    }
}

#[test]
fn an_episodes_count_of_zero_or_a_bad_slot_is_refused() {
    let (image, records) = partition();
    let mut dev = Cut::new(image);
    assert!(records.set_episodes(&mut dev, "bad id", 1).is_err());
    assert_eq!(dev.writes, 0);
    assert_eq!(
        decode_episodes(b"os-1 0"),
        None,
        "zero is no episode record"
    );
    assert_eq!(decode_episodes(b"os-1 300"), None, "past one byte");
    assert_eq!(decode_episodes(b"os-1 2"), Some(("os-1".into(), 2)));
}

/// The partition of [`partition`] with the three pointer files added, each two sectors (the one form),
/// all pointing at `os-1`.
fn partition_with_pointers() -> (Vec<u8>, Records, Pointers) {
    let mut vol = build(
        false,
        1,
        16_384,
        &[
            Entry {
                name: TRIAL_FILE,
                size: 1024,
                chain: vec![3, 4],
            },
            Entry {
                name: REASON_FILE,
                size: 1024,
                chain: vec![5, 6],
            },
            Entry {
                name: EPISODES_FILE,
                size: 1024,
                chain: vec![7, 8],
            },
            Entry {
                name: DEFAULT_POINTER,
                size: 2 * SECTOR as u32,
                chain: vec![9, 10],
            },
            Entry {
                name: PREVIOUS_POINTER,
                size: 2 * SECTOR as u32,
                chain: vec![11, 12],
            },
            Entry {
                name: TRIAL_POINTER,
                size: 2 * SECTOR as u32,
                chain: vec![13, 14],
            },
        ],
    );
    let records = Records::place(&mut vol).unwrap();
    let pointers = Pointers::place(&mut vol).unwrap();
    let (trial_lba, reason_lba) = records.lbas();
    let made = precreate(&[
        (Kind::TRIAL, b""),
        (Kind::REASON, b""),
        (Kind::EPISODES, b""),
    ])
    .unwrap();
    for ((_, bytes), lba) in made
        .iter()
        .zip([trial_lba, reason_lba, records.episodes_lba()])
    {
        let at = lba as usize * SECTOR;
        vol.0[at..at + bytes.len()].copy_from_slice(bytes);
    }
    for which in [Pointer::Default, Pointer::Previous, Pointer::Trial] {
        let file = firmware::pointer_file("os-1").unwrap();
        let at = pointers.lba(which) as usize * SECTOR;
        vol.0[at..at + file.len()].copy_from_slice(&file);
    }
    (vol.0, records, pointers)
}

#[test]
fn the_pointer_files_are_placed_at_their_sectors_and_read_back() {
    let (image, _, pointers) = partition_with_pointers();
    let mut dev = Cut::new(image);
    for which in [Pointer::Default, Pointer::Previous, Pointer::Trial] {
        assert_eq!(
            pointers.read(&mut dev, which).unwrap().as_deref(),
            Some("os-1")
        );
    }
    assert_ne!(
        pointers.lba(Pointer::Default),
        pointers.lba(Pointer::Previous)
    );
}

#[test]
fn a_pointer_write_changes_only_its_own_two_sectors() {
    let (image, _, pointers) = partition_with_pointers();
    let mut dev = Cut::new(image.clone());
    pointers.write(&mut dev, Pointer::Default, "os-22").unwrap();
    let lba = pointers.lba(Pointer::Default) as usize * SECTOR;
    let end = lba + firmware::POINTER_FILE_BYTES;
    assert_eq!(
        dev.image[..lba],
        image[..lba],
        "bytes before the pointer changed"
    );
    assert_eq!(dev.image[end..], image[end..], "bytes after changed");
    assert_eq!(
        pointers
            .read(&mut dev, Pointer::Default)
            .unwrap()
            .as_deref(),
        Some("os-22")
    );
    assert_eq!(
        pointers
            .read(&mut dev, Pointer::Previous)
            .unwrap()
            .as_deref(),
        Some("os-1")
    );
}

#[test]
fn a_pointer_cut_at_every_byte_is_the_old_slot_the_new_slot_or_refused() {
    // Each pair has ids of different lengths: a cut inside the id must never read as a third slot.
    for (old, new) in [
        ("a", "bb"),
        ("bb", "a"),
        ("x.y-1", "z_2"),
        ("q", "q.r"),
        ("os-1", "os-2"),
    ] {
        let (image, _, pointers) = partition_with_pointers();
        let mut seed = Cut::new(image);
        pointers.write(&mut seed, Pointer::Default, old).unwrap();
        let before = seed.image.clone();
        let mut refused = 0;
        // The two writes of a pointer: write 0 is the second sector (the commit), write 1 the first.
        for write in 0..2usize {
            for tear in 0..=SECTOR {
                let mut dev = Cut::new(before.clone());
                if tear < SECTOR {
                    dev.cut = Some((write, tear));
                }
                let lba = pointers.lba(Pointer::Default);
                let wrote = pointers.write(&mut dev, Pointer::Default, new);
                assert_eq!(
                    wrote.is_err(),
                    tear < SECTOR,
                    "{old}->{new} write {write} tear {tear}"
                );
                let read = pointers.read(&mut dev, Pointer::Default).unwrap();
                match read.as_deref() {
                    None => refused += 1,
                    Some(id) => assert!(
                        id == old || id == new,
                        "{old}->{new} write {write} tear {tear} read {id}"
                    ),
                }
                // Nothing but the pointer's two sectors changed.
                let at = lba as usize * SECTOR;
                let end = at + firmware::POINTER_FILE_BYTES;
                assert_eq!(
                    dev.image[..at],
                    before[..at],
                    "{old}->{new} write {write} tear {tear}"
                );
                assert_eq!(
                    dev.image[end..],
                    before[end..],
                    "{old}->{new} write {write} tear {tear}"
                );
            }
        }
        // Two copies: one cut tears one sector, and the other still holds a whole line, so no single
        // cut leaves the pointer refused.
        assert_eq!(refused, 0, "{old}->{new}: one cut left no whole line");
    }
}

/// The Pi firmware's image from the image builder with `os-1` in bay 0 and every pointer naming it.
fn bay_image() -> Vec<u8> {
    use crate::bootimage::{self, Family, Parts};
    let parts = Parts {
        family: Family::Tryboot,
        layout: bays::Layout::new([2048, 1024, 512, 512, 512]).unwrap(),
        root: vec![("slot-layout.toml", b"version = 5\n".to_vec())],
        fixed: Vec::new(),
    };
    let payloads = bay_payloads("os-1");
    let refs: [&[u8]; 5] = std::array::from_fn(|i| payloads[i].as_slice());
    let size = bootimage::min_size(&parts).unwrap();
    bootimage::build(&parts, size, "os-1", &refs).unwrap()
}

fn bay_payloads(id: &str) -> [Vec<u8>; 5] {
    [
        format!("kernel {id}").into_bytes(),
        format!("initramfs {id}").into_bytes(),
        format!("device tree {id}").into_bytes(),
        format!("console=tty1 os.slot={id}").into_bytes(),
        format!("overlay {id}").into_bytes(),
    ]
}

#[test]
fn a_boot_on_a_real_file_places_every_file_and_round_trips_the_pointers_and_trial() {
    let dir = crate::tests::TempDir::new("boot-raw");
    let node = dir.0.join("boot.img");
    std::fs::write(&node, bay_image()).unwrap();
    let mut boot = Boot::open(&node).unwrap();
    assert_eq!(
        boot.pointer(Pointer::Default).unwrap().as_deref(),
        Some("os-1")
    );
    let two = bay_payloads("os-2");
    let refs: [&[u8]; 5] = std::array::from_fn(|i| two[i].as_slice());
    boot.install_slot("os-2", &refs).unwrap();
    boot.set_pointer(Pointer::Default, "os-2").unwrap();
    assert_eq!(
        boot.pointer(Pointer::Default).unwrap().as_deref(),
        Some("os-2")
    );
    assert_eq!(
        boot.pointer(Pointer::Previous).unwrap().as_deref(),
        Some("os-1")
    );
    boot.arm_trial("os-1", 2).unwrap();
    assert_eq!(
        boot.trial().unwrap(),
        Some(Trial {
            slot: "os-1".into(),
            attempts: 2
        })
    );
    assert_eq!(
        boot.pointer(Pointer::Trial).unwrap().as_deref(),
        Some("os-1")
    );
    boot.clear_trial().unwrap();
    assert_eq!(boot.trial().unwrap(), None);
    // The bytes on the file are the bytes the pointers and records read back: a reopen agrees.
    drop(boot);
    let mut again = Boot::open(&node).unwrap();
    assert_eq!(
        again.pointer(Pointer::Default).unwrap().as_deref(),
        Some("os-2")
    );
}

#[test]
fn a_partition_without_its_pointer_files_is_refused_by_the_pointers_name() {
    let dir = crate::tests::TempDir::new("boot-raw-missing");
    let node = dir.0.join("boot.img");
    // The records are there, the pointer files are not: refused, and the message names the file.
    let (image, _) = partition();
    std::fs::write(&node, &image).unwrap();
    let why = Boot::open(&node).err().expect("refused");
    assert!(why.contains("default.txt"), "{why}");
}

#[test]
fn an_arm_cut_at_every_byte_never_leaves_a_trial_record_without_its_pointer() {
    // Arming writes the trial pointer, then the record. A cut in either write, at any byte, must
    // leave: no record, or a record whose slot the trial pointer names. The pointer is always one of
    // the two slots or refused.
    let (image, records, pointers) = partition_with_pointers();
    for write in 0..2usize {
        for tear in 0..=SECTOR {
            let mut dev = Cut::new(image.clone());
            dev.cut = Some((write, tear));
            let _ = arm_trial_raw(&records, &pointers, &mut dev, "os-2", 2);
            let trial = records.trial(&mut dev).unwrap();
            let pointer = pointers.read(&mut dev, Pointer::Trial).unwrap();
            if let Some(t) = trial {
                assert_eq!(t.slot, "os-2", "write {write} tear {tear}");
                assert_eq!(
                    pointer.as_deref(),
                    Some("os-2"),
                    "write {write} tear {tear}: record without pointer"
                );
            }
            assert!(matches!(
                pointer.as_deref(),
                None | Some("os-1") | Some("os-2")
            ));
        }
    }
}
