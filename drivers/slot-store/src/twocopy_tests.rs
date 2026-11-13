use super::*;
use std::io;

/// A record device over a byte image. Writes can be cut: after `cut.0` full sector writes, the next
/// write lands only its first `cut.1` bytes and the device is dead: every later write fails and
/// changes nothing. Reads still see what the medium holds.
struct Cutting {
    image: Vec<u8>,
    writes: usize,
    cut: Option<(usize, usize)>,
    dead: bool,
}

impl Cutting {
    fn new(image: Vec<u8>) -> Self {
        Self {
            image,
            writes: 0,
            cut: None,
            dead: false,
        }
    }
}

impl Sectors for Cutting {
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

/// An image of `sectors` zeroed sectors, with `pair` holding `old` as `kind` at sequence 1.
fn image_with(pair: Pair, kind: Kind, old: &[u8], sectors: usize) -> Vec<u8> {
    let mut dev = Cutting::new(vec![0xAB; sectors * SECTOR]);
    // Make the record's sectors zero first, so the test's starting state is known.
    for lba in [pair.lba(), pair.lba() + 1] {
        let at = lba as usize * SECTOR;
        dev.image[at..at + SECTOR].fill(0);
    }
    pair.write(&mut dev, kind, old).unwrap();
    dev.image
}

fn sector_of(image: &[u8], lba: u64) -> Sector {
    let at = lba as usize * SECTOR;
    let mut s = [0u8; SECTOR];
    s.copy_from_slice(&image[at..at + SECTOR]);
    s
}

#[test]
fn crc32_matches_the_standard_check_value() {
    // The IEEE CRC-32 check value: crc("123456789") = 0xCBF43926.
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b""), 0);
}

#[test]
fn a_copy_round_trips_and_every_corruption_is_refused() {
    let mut s = [0u8; SECTOR];
    encode(Kind::TRIAL, 7, b"os-2 3 armed", &mut s).unwrap();
    let c = decode(&s).unwrap();
    assert_eq!(
        (c.kind, c.seq, c.payload),
        (Kind::TRIAL, 7, &b"os-2 3 armed"[..])
    );

    // Flip each byte in turn: a copy that changed anywhere is not a copy. This covers the magic,
    // version, kind, sequence, length, payload and the CRC itself.
    for i in 0..SECTOR {
        let mut bad = s;
        bad[i] ^= 0x01;
        assert!(decode(&bad).is_none(), "byte {i} changed and still decoded");
    }
}

#[test]
fn an_oversized_payload_is_refused_not_truncated() {
    let mut s = [0u8; SECTOR];
    let big = vec![b'x'; MAX_PAYLOAD + 1];
    assert!(matches!(
        encode(Kind::TRIAL, 1, &big, &mut s),
        Err(Error::TooLong(_))
    ));
    let max = vec![b'x'; MAX_PAYLOAD];
    encode(Kind::TRIAL, 1, &max, &mut s).unwrap();
    assert_eq!(decode(&s).unwrap().payload, &max[..]);
}

#[test]
fn an_empty_device_has_no_record_and_the_first_write_is_sequence_one() {
    let pair = Pair::at(4);
    let mut dev = Cutting::new(vec![0; 16 * SECTOR]);
    assert_eq!(pair.read(&mut dev, Kind::TRIAL).unwrap(), None);
    assert_eq!(pair.write(&mut dev, Kind::TRIAL, b"a").unwrap(), 1);
    let now = pair.read(&mut dev, Kind::TRIAL).unwrap().unwrap();
    assert_eq!(
        (now.seq, now.copy, now.payload.as_slice()),
        (1, 0, &b"a"[..])
    );
}

#[test]
fn writes_alternate_copies_and_each_is_one_sector() {
    let pair = Pair::at(2);
    let mut dev = Cutting::new(vec![0; 8 * SECTOR]);
    for n in 1..=6u64 {
        let before = dev.writes;
        let seq = pair.write(&mut dev, Kind::TRIAL, &n.to_le_bytes()).unwrap();
        assert_eq!(seq, n);
        assert_eq!(dev.writes - before, 1, "a record write is one sector write");
        let cur = pair.read(&mut dev, Kind::TRIAL).unwrap().unwrap();
        assert_eq!((cur.seq, cur.payload.as_slice()), (n, &n.to_le_bytes()[..]));
        assert_eq!(cur.copy, (n as u8 - 1) % 2);
    }
}

#[test]
fn the_reader_takes_the_valid_copy_with_the_higher_sequence() {
    // Build the four combinations by hand: each copy is valid, corrupt, older or newer.
    let pair = Pair::at(0);
    let mut a = [0u8; SECTOR];
    let mut b = [0u8; SECTOR];
    encode(Kind::TRIAL, 5, b"old", &mut a).unwrap();
    encode(Kind::TRIAL, 6, b"new", &mut b).unwrap();

    let with = |x: &Sector, y: &Sector| {
        let mut img = Vec::new();
        img.extend_from_slice(x);
        img.extend_from_slice(y);
        Cutting::new(img)
    };

    // Both valid: the higher sequence wins whichever copy holds it.
    let mut dev = with(&a, &b);
    assert_eq!(
        pair.read(&mut dev, Kind::TRIAL).unwrap().unwrap().payload,
        b"new"
    );
    let mut dev = with(&b, &a);
    let cur = pair.read(&mut dev, Kind::TRIAL).unwrap().unwrap();
    assert_eq!(
        (cur.seq, cur.copy, cur.payload.as_slice()),
        (6, 0, &b"new"[..])
    );

    // One valid, one corrupt: the valid one, whichever it is.
    let mut torn = b;
    torn[100] ^= 0xFF;
    let mut dev = with(&a, &torn);
    assert_eq!(
        pair.read(&mut dev, Kind::TRIAL).unwrap().unwrap().payload,
        b"old"
    );
    let mut dev = with(&torn, &a);
    assert_eq!(
        pair.read(&mut dev, Kind::TRIAL).unwrap().unwrap().payload,
        b"old"
    );

    // Neither valid: absent, not an error.
    let mut both = a;
    both[100] ^= 0xFF;
    let mut dev = with(&both, &torn);
    assert_eq!(pair.read(&mut dev, Kind::TRIAL).unwrap(), None);

    // A copy of another kind is not this record.
    let mut other = [0u8; SECTOR];
    encode(Kind::REASON, 9, b"x", &mut other).unwrap();
    let mut dev = with(&other, &a);
    assert_eq!(
        pair.read(&mut dev, Kind::TRIAL).unwrap().unwrap().payload,
        b"old"
    );
}

#[test]
fn two_valid_copies_at_one_sequence_with_different_payloads_are_refused() {
    let pair = Pair::at(0);
    let mut a = [0u8; SECTOR];
    let mut b = [0u8; SECTOR];
    encode(Kind::TRIAL, 3, b"one", &mut a).unwrap();
    encode(Kind::TRIAL, 3, b"two", &mut b).unwrap();
    let mut img = Vec::new();
    img.extend_from_slice(&a);
    img.extend_from_slice(&b);
    let mut dev = Cutting::new(img);
    assert!(matches!(
        pair.read(&mut dev, Kind::TRIAL),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        pair.write(&mut dev, Kind::TRIAL, b"x"),
        Err(Error::Conflict)
    ));
    assert_eq!(dev.writes, 0, "a refused write writes nothing");
}

/// A cut at each write point of one update, at every tear length that matters: nothing written,
/// half a sector, the CRC's last byte missing, a whole sector (the cut lands after the write).
/// After each cut the reader gets the old value or the new one, and no sector outside the record
/// changed.
#[test]
fn a_cut_at_any_point_of_an_update_leaves_the_old_or_the_new_value() {
    let pair = Pair::at(3);
    let sectors = 16;
    let old = b"os-1 2 armed".to_vec();
    let new = b"os-2 1 armed".to_vec();
    let start = image_with(pair, Kind::TRIAL, &old, sectors);

    // How many writes an uncut update makes: one.
    let mut dev = Cutting::new(start.clone());
    pair.write(&mut dev, Kind::TRIAL, &new).unwrap();
    let total = dev.writes;
    assert_eq!(total, 1);

    for n in 0..=total {
        for tear in [0, 1, 17, 255, 507, 508, 511, SECTOR] {
            let mut dev = Cutting::new(start.clone());
            dev.cut = Some((n, tear));
            let _ = pair.write(&mut dev, Kind::TRIAL, &new);

            // A reader that boots after the cut: a fresh device over the medium.
            let mut after = Cutting::new(dev.image.clone());
            let got = pair.read(&mut after, Kind::TRIAL).unwrap().unwrap();
            assert!(
                got.payload == old || got.payload == new,
                "cut at write {n} tear {tear}: read {:?}",
                String::from_utf8_lossy(&got.payload)
            );
            // A cut before the sector completes keeps the old value. At n == total the update was
            // not cut at all, so the new value is correct.
            if n < total && tear < SECTOR {
                assert_eq!(
                    got.payload, old,
                    "a torn sector must not be taken (tear {tear})"
                );
            }

            // Everything outside the record's two sectors is byte-identical to before.
            let lo = pair.lba() as usize * SECTOR;
            let hi = lo + 2 * SECTOR;
            assert_eq!(
                dev.image[..lo],
                start[..lo],
                "bytes before the record changed"
            );
            assert_eq!(
                dev.image[hi..],
                start[hi..],
                "bytes after the record changed"
            );
        }
    }
}

/// A run of updates with a cut at each: the device keeps working after a restart, and the value
/// is always the last one that completed or the one being written.
#[test]
fn updates_survive_a_cut_at_each_step_and_recover() {
    let pair = Pair::at(1);
    let mut image = vec![0u8; 8 * SECTOR];
    let mut committed = None::<Vec<u8>>;
    for step in 0..12u8 {
        let value = vec![step; 1 + usize::from(step)];
        let tear = (usize::from(step) * 97) % (SECTOR + 1);
        let mut dev = Cutting::new(image.clone());
        dev.cut = Some((0, tear));
        let result = pair.write(&mut dev, Kind::TRIAL, &value);
        image = dev.image;
        let mut after = Cutting::new(image.clone());
        let got = pair
            .read(&mut after, Kind::TRIAL)
            .unwrap()
            .map(|c| c.payload);
        match result {
            Ok(_) => {
                assert_eq!(got.as_deref(), Some(&value[..]));
                committed = Some(value);
            }
            Err(_) => {
                let expected = committed.clone();
                assert!(
                    got == expected || got.as_deref() == Some(&value[..]),
                    "step {step}: cut left {got:?}, neither the last commit {expected:?} nor the new value"
                );
                // Recovery: the next update, on the medium as it is, succeeds.
                let mut dev = Cutting::new(image.clone());
                pair.write(&mut dev, Kind::TRIAL, &value).unwrap();
                image = dev.image;
                committed = Some(value);
            }
        }
    }
}

#[test]
fn precreated_files_are_valid_at_sequence_one_and_refuse_unknown_kinds() {
    let files = precreate(&[(Kind::TRIAL, b"x"), (Kind::SECURITY, b"gen=0")]).unwrap();
    assert_eq!(files[0].0, b"TRIAL   REC");
    assert_eq!(files[1].0, b"SECURE  REC");
    for (name, bytes) in &files {
        assert_eq!(
            bytes.len(),
            2 * SECTOR,
            "{} is two sectors",
            String::from_utf8_lossy(name.as_slice())
        );
    }
    let mut dev = Cutting::new(files[0].1.clone());
    let cur = Pair::at(0).read(&mut dev, Kind::TRIAL).unwrap().unwrap();
    assert_eq!((cur.seq, cur.copy), (1, 0));
    assert!(precreate(&[(Kind(200), b"")]).is_err());
}

#[test]
fn a_file_device_writes_and_reads_its_sectors() {
    let dir = std::env::temp_dir().join(format!("lemnos-twocopy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rec.img");
    std::fs::write(&path, vec![0u8; 8 * SECTOR]).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let mut dev = FileSectors::new(file, 0);
    let pair = Pair::at(2);
    pair.write(&mut dev, Kind::SECURITY, b"gen=1").unwrap();
    let cur = pair.read(&mut dev, Kind::SECURITY).unwrap().unwrap();
    assert_eq!(cur.payload, b"gen=1");
    let raw = std::fs::read(&path).unwrap();
    assert_eq!(sector_of(&raw, 2)[0..4], *b"LMRC");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The FAT directory name (eleven bytes, space padded, upper case) of an 8.3 name with a dot.
fn fat_name_of(visible: &str) -> [u8; 11] {
    let (base, ext) = visible.split_once('.').expect("an 8.3 name with a dot");
    assert!(base.len() <= 8 && ext.len() <= 3, "{visible} is 8.3");
    let mut out = [b' '; 11];
    for (i, b) in base.bytes().enumerate() {
        out[i] = b.to_ascii_uppercase();
    }
    for (i, b) in ext.bytes().enumerate() {
        out[8 + i] = b.to_ascii_uppercase();
    }
    out
}

#[test]
fn every_record_has_an_ordinary_visible_name_that_its_directory_entry_is_named_by() {
    for kind in [
        Kind::TRIAL,
        Kind::REASON,
        Kind::BOOT_HISTORY,
        Kind::SECURITY,
        Kind::EPISODES,
    ] {
        let visible = kind
            .visible_name()
            .expect("every known kind has a visible name");
        assert!(
            !visible.contains(' ') && visible.is_ascii(),
            "{visible} has a space or is not ASCII"
        );
        let fat = kind.file_name().expect("every known kind has a FAT name");
        assert_eq!(
            &fat_name_of(visible),
            fat,
            "{visible} must name the directory entry {}",
            String::from_utf8_lossy(fat)
        );
    }
    assert_eq!(Kind(200).visible_name(), None);
    // The FAT names precreate writes are the same ones: the trial record is `TRIAL   REC`.
    assert_eq!(&fat_name_of("trial.rec"), b"TRIAL   REC");
}
