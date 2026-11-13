use super::*;
use crate::bootfat::tests::{Entry, build, data_start};
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
        ],
    );
    let records = Records::place(&mut vol).unwrap();
    let (trial_lba, reason_lba) = records.lbas();
    let made = precreate(&[(Kind::TRIAL, b"os-1 3"), (Kind::REASON, b"")]).unwrap();
    for ((_, bytes), lba) in made.iter().zip([trial_lba, reason_lba]) {
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
