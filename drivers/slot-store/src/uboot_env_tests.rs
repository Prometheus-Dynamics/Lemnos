//! U-Boot's redundant environment: the format's facts, the selection rule as `env_check_redund`
//! states it, the writer's one copy, and a cut at every byte of a write, over an in-memory device.

use super::{
    Copy, EnvDev, Geometry, Place, Which, encode_copy, encode_vars, load, select, set, vars,
};
use std::io;

/// A device in memory. `budget` bytes of writes are allowed; the write that passes it writes the
/// bytes it has left and fails, as a power cut does.
struct Mem {
    bytes: Vec<u8>,
    budget: usize,
}

impl Mem {
    fn new(len: usize) -> Self {
        Self {
            bytes: vec![0xEE; len],
            budget: usize::MAX,
        }
    }
}

impl EnvDev for Mem {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let at = offset as usize;
        buf.copy_from_slice(&self.bytes[at..at + buf.len()]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let at = offset as usize;
        let take = bytes.len().min(self.budget);
        self.bytes[at..at + take].copy_from_slice(&bytes[..take]);
        self.budget -= take;
        if take < bytes.len() {
            return Err(io::Error::other("cut"));
        }
        Ok(())
    }
}

fn geometry() -> Geometry {
    Geometry::new(1024, 0, 4096).unwrap()
}

/// Writes a valid copy of `pairs` at `offset` with serial `flags`.
fn put(dev: &mut Mem, offset: u64, pairs: &[(&str, &str)], flags: u8) {
    let list: Vec<(String, String)> = pairs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    let data = encode_vars(&list, geometry().data_len()).unwrap();
    let bytes = encode_copy(&data, flags);
    let at = offset as usize;
    dev.bytes[at..at + bytes.len()].copy_from_slice(&bytes);
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn copy(flags: u8, valid: bool) -> Copy {
    let data = encode_vars(&[], geometry().data_len()).unwrap();
    let mut c = Copy {
        crc: super::super::twocopy::crc32(&data),
        flags,
        data,
    };
    if !valid {
        c.crc ^= 1;
    }
    c
}

/// The CRC is over the whole data area, padding included, as `env_export` computes it.
#[test]
fn the_crc_covers_the_padding_as_u_boots_does() {
    let g = geometry();
    let data = encode_vars(&pairs(&[("default", "a")]), g.data_len()).unwrap();
    let mut bytes = encode_copy(&data, 1);
    assert!(Copy::parse(&bytes, &g).unwrap().valid());
    let last = bytes.len() - 1;
    bytes[last] = 0x55;
    assert!(
        !Copy::parse(&bytes, &g).unwrap().valid(),
        "a padding byte changes the CRC"
    );
}

/// The copy's bytes: the CRC word least significant byte first, then the serial, then the data.
#[test]
fn a_copy_is_crc_serial_and_data() {
    let data = vec![7u8; 4];
    let bytes = encode_copy(&data, 9);
    assert_eq!(
        &bytes[..4],
        &super::super::twocopy::crc32(&data).to_le_bytes()
    );
    assert_eq!(bytes[4], 9);
    assert_eq!(&bytes[5..], &data[..]);
}

/// U-Boot's rule, case by case, from `env_check_redund`.
#[test]
fn selection_is_u_boots_rule() {
    let v = |flags| copy(flags, true);
    let bad = copy(5, false);
    assert_eq!(select(Some(&v(1)), Some(&v(0))), Some(Which::First));
    assert_eq!(select(Some(&v(0)), Some(&v(1))), Some(Which::Second));
    assert_eq!(
        select(Some(&v(255)), Some(&v(0))),
        Some(Which::Second),
        "the serial wrapped"
    );
    assert_eq!(select(Some(&v(0)), Some(&v(255))), Some(Which::First));
    assert_eq!(
        select(Some(&v(3)), Some(&v(3))),
        Some(Which::First),
        "equal serials take copy one"
    );
    assert_eq!(
        select(Some(&bad), Some(&v(0))),
        Some(Which::Second),
        "a bad CRC is not taken"
    );
    assert_eq!(select(Some(&v(9)), Some(&bad)), Some(Which::First));
    assert_eq!(
        select(Some(&bad), Some(&bad)),
        None,
        "no valid copy: no environment"
    );
    assert_eq!(select(None, None), None);
}

#[test]
fn variables_round_trip_and_the_list_ends_at_the_empty_entry() {
    let list = pairs(&[("default", "a1"), ("trial", ""), ("trial_attempts", "3")]);
    let mut data = encode_vars(&list, 128).unwrap();
    assert_eq!(vars(&data).unwrap(), list);
    // Bytes after the list's end are padding, not variables.
    // The entries end with the empty entry: "default=a1\0trial=\0trial_attempts=3\0" then "\0".
    let end = "default=a1\0trial=\0trial_attempts=3\0".len();
    assert_eq!(data[end], 0, "the list's terminator");
    data[end + 1] = 0x41;
    assert_eq!(vars(&data).unwrap(), list);
}

#[test]
fn a_variable_that_cannot_be_written_or_a_full_list_is_refused() {
    assert!(encode_vars(&pairs(&[("a=b", "c")]), 64).is_err());
    assert!(encode_vars(&pairs(&[("a", "b\0c")]), 64).is_err());
    assert!(encode_vars(&pairs(&[("", "x")]), 64).is_err());
    assert!(
        encode_vars(&pairs(&[("long", &"x".repeat(60))]), 16).is_err(),
        "too full"
    );
}

/// The writer changes the inactive copy and its serial is the active one plus one; the next load
/// selects the new copy, which now holds the newer serial.
#[test]
fn set_writes_the_inactive_copy_with_the_next_serial() {
    let g = geometry();
    let mut dev = Mem::new(8192);
    put(&mut dev, 0, &[("default", "a")], 4);
    put(&mut dev, 4096, &[("default", "old")], 3);
    set(&mut dev, &g, &[("default", Some("b"))]).unwrap();
    assert_eq!(
        dev.bytes[4096 + 4],
        5,
        "the inactive copy gets the active serial plus one"
    );
    let (which, got) = load(&mut dev, &g).unwrap().unwrap();
    assert_eq!((which, got), (Which::Second, pairs(&[("default", "b")])));
}

/// The serial wraps as U-Boot's does: from 255 to 0, and then copy two (serial 0) is the newer.
#[test]
fn the_serial_wraps_from_255_to_0() {
    let g = geometry();
    let mut dev = Mem::new(8192);
    put(&mut dev, 0, &[("default", "a")], 255);
    dev.bytes[4096..4096 + 1024].fill(0xAA);
    set(&mut dev, &g, &[("default", Some("b"))]).unwrap();
    assert_eq!(dev.bytes[4096 + 4], 0, "the serial wrapped to 0");
    let (which, got) = load(&mut dev, &g).unwrap().unwrap();
    assert_eq!(which, Which::Second);
    assert_eq!(got, pairs(&[("default", "b")]));
}

/// No valid environment: nothing is written, and the writer says so.
#[test]
fn set_refuses_without_a_valid_environment_and_writes_nothing() {
    let g = geometry();
    let mut dev = Mem::new(8192);
    let before = dev.bytes.clone();
    let err = set(&mut dev, &g, &[("trial", Some("b"))]).unwrap_err();
    assert!(
        err.contains("no valid U-Boot environment of 1024 bytes"),
        "{err}"
    );
    assert_eq!(dev.bytes, before);
}

/// Removing a variable and setting another, in one copy.
#[test]
fn set_removes_and_replaces() {
    let g = geometry();
    let mut dev = Mem::new(8192);
    put(
        &mut dev,
        0,
        &[("default", "a"), ("trial", "b"), ("trial_attempts", "3")],
        1,
    );
    set(
        &mut dev,
        &g,
        &[("trial", None), ("trial_attempts", Some("0"))],
    )
    .unwrap();
    let (_, got) = load(&mut dev, &g).unwrap().unwrap();
    assert_eq!(got, pairs(&[("default", "a"), ("trial_attempts", "0")]));
}

/// A cut at every byte of a write, from each starting copy: whatever the cut, the environment
/// U-Boot's rule selects is a whole valid environment, the old one or the new one.
#[test]
fn a_cut_at_every_byte_of_a_write_leaves_the_old_or_the_new_environment() {
    let g = geometry();
    let padded = g.padded();
    for active_first in [true, false] {
        for cut in 0..=padded {
            let mut dev = Mem::new(8192);
            if active_first {
                put(&mut dev, 0, &[("default", "a"), ("trial", "x")], 6);
                put(&mut dev, 4096, &[("default", "zz")], 5);
            } else {
                put(&mut dev, 0, &[("default", "zz")], 5);
                put(&mut dev, 4096, &[("default", "a"), ("trial", "x")], 6);
            }
            let old = pairs(&[("default", "a"), ("trial", "x")]);
            let new = pairs(&[("default", "a"), ("trial", "y")]);
            dev.budget = cut;
            let result = set(&mut dev, &g, &[("trial", Some("y"))]);
            assert_eq!(result.is_err(), cut < padded, "cut {cut}");
            dev.budget = usize::MAX;
            let (_, got) = load(&mut dev, &g)
                .unwrap()
                .unwrap_or_else(|| panic!("no environment after a cut at {cut}"));
            assert!(
                got == old || got == new,
                "cut {cut} (active first: {active_first}) left {got:?}"
            );
        }
    }
}

fn locator_text(size: &str, offset: &str, redund: &str) -> String {
    format!(
        "version = 1\nform = \"device\"\ndevice = \"/dev/mmcblk0\"\nsize = {size}\nsector = 512\noffset = {offset}\noffset_redund = {redund}\n"
    )
}

/// Both locator forms round-trip through their text, to the same place.
#[test]
fn a_locator_round_trips_through_its_text() {
    let gpt = Place::gpt(8192).unwrap();
    let text = gpt.encode();
    assert!(
        text.contains("version = 1\n")
            && text.contains("form = \"gpt\"\n")
            && text.contains("size = 8192\n")
            && !text.contains("offset"),
        "{text}"
    );
    assert_eq!(Place::parse(&text).unwrap(), gpt);
    let device = Place::device("/dev/mmcblk0", 1024, 0, 4096).unwrap();
    let text = device.encode();
    assert!(text.contains("offset_redund = 4096\n"), "{text}");
    assert_eq!(Place::parse(&text).unwrap(), device);
    // The same text, keys in another order, with blank lines and comments, reads the same.
    let shuffled = "# note\n\noffset_redund = 4096\nsector = 512\noffset = 0\nsize = 1024\ndevice = \"/dev/mmcblk0\"\nform = \"device\"\nversion = 1\n";
    assert_eq!(Place::parse(shuffled).unwrap(), device);
}

/// Every malformed locator is refused by name, and nothing is read from it.
#[test]
fn a_malformed_locator_is_refused_by_name() {
    let good = locator_text("1024", "0", "4096");
    let gpt = Place::gpt(8192).unwrap().encode();
    let cases = [
        (good.replace("sector = 512", "sector = 4096"), "sector"),
        (good.replace("version = 1", "version = 2"), "version"),
        (
            good.replace("offset = 0\n", "offset = 0\nextra = 1\n"),
            "unknown key",
        ),
        (
            good.replace("offset = 0\n", "offset = 0\noffset = 0\n"),
            "given twice",
        ),
        (good.replace("size = 1024\n", ""), "\"size\" is missing"),
        (good.replace("\"/dev/mmcblk0\"", "/dev/mmcblk0"), "quoted"),
        (
            good.replace("\"/dev/mmcblk0\"", "\"mmcblk0\""),
            "absolute device path",
        ),
        (
            good.replace("/dev/mmcblk0", "/dev/mtd0"),
            "flash or a UBI volume",
        ),
        (
            good.replace("form = \"device\"", "form = \"uuid\""),
            "form \"uuid\"",
        ),
        (good.replace("size = 1024", "size = +1024"), "decimal"),
        (
            good.replace("offset = 0", "offset = 100"),
            "multiple of the 512",
        ),
        (
            good.replace("offset_redund = 4096", "offset_redund = 512"),
            "overlap",
        ),
        (good.replace("size = 1024", "size = 4"), "too small"),
        (
            good.replace("device = \"/dev/mmcblk0\"\n", ""),
            "\"device\" is missing",
        ),
        (gpt.clone() + "offset = 0\n", "not a key of the gpt form"),
        (
            gpt.replace("form = \"gpt\"", "form = \"gpt\"\npartuuid = \"x\""),
            "unknown key",
        ),
        ("device = \"x\"\n".to_string(), "\"version\" is missing"),
    ];
    for (text, why) in cases {
        let err = Place::parse(&text).expect_err(&text);
        assert!(err.contains(why), "{why}: {err}");
    }
}

/// A place whose copies run past the device is refused by name, and one that fits is not.
#[test]
fn a_place_past_the_end_of_its_device_is_refused() {
    let mut dev = Mem::new(0);
    let made = Place::parse(&locator_text("1024", "0", "4096")).unwrap();
    assert!(made.geometry(&mut dev, 5120).is_ok());
    let err = made.geometry(&mut dev, 4096 + 1023).unwrap_err();
    assert!(
        err.contains("second copy") && err.contains("runs past the end"),
        "{err}"
    );
    let first = Place::parse(&locator_text("1024", "8192", "0")).unwrap();
    let err = first.geometry(&mut dev, 4096).unwrap_err();
    assert!(err.contains("first copy"), "{err}");
}

/// The two copies never overlap, and a copy must fit its sector-padded size.
#[test]
fn copies_that_overlap_are_refused_by_geometry() {
    assert!(Geometry::new(1024, 0, 512).is_err());
    assert!(Geometry::new(1024, 0, 1024).is_ok());
    assert!(
        Geometry::new(1000, 0, 512).is_err(),
        "1000 bytes pad to 1024"
    );
}

/// U-Boot's placement in the GUID partition (`env/mmc.c:144-147`): the last `len` blocks hold
/// copy 0 and the `len` blocks below them copy 1, `len` being the size rounded up to blocks.
#[test]
fn the_copies_sit_at_the_top_of_the_partition_as_u_boot_places_them() {
    // 128 blocks from block 100: end = 228. An 8 KiB environment is 16 blocks.
    let g = Geometry::in_partition(8192, 100, 128).unwrap();
    assert_eq!(g.offset, (228 - 16) * 512);
    assert_eq!(g.offset_redund, (228 - 32) * 512);
    // A size that is not a whole number of blocks rounds up.
    let g = Geometry::in_partition(1000, 100, 128).unwrap();
    assert_eq!(
        (g.offset, g.offset_redund),
        ((228 - 2) * 512, (228 - 4) * 512)
    );
    // Two copies must fit (`env/mmc.c:135-141`).
    assert!(Geometry::in_partition(8192, 0, 31).is_err());
    assert!(Geometry::in_partition(8192, 0, 32).is_ok());
}

/// Nothing is written without a valid copy: an erased area (0xFF, or zero) is its own case.
#[test]
fn an_erased_area_and_a_wrong_size_are_told_apart_and_nothing_is_written() {
    let g = geometry();
    for fill in [0xFFu8, 0x00] {
        let mut dev = Mem::new(8192);
        dev.bytes.fill(fill);
        let before = dev.bytes.clone();
        let err = set(&mut dev, &g, &[("trial", Some("b"))]).unwrap_err();
        assert!(err.contains("is erased: U-Boot has not saved"), "{err}");
        assert_eq!(dev.bytes, before);
    }
    // Written at 2048 bytes, read as 1024: both CRCs fail.
    let mut dev = Mem::new(8192);
    let big = Geometry::new(2048, 0, 4096).unwrap();
    let data = encode_vars(&pairs(&[("default", "a")]), big.data_len()).unwrap();
    for at in [0usize, 4096] {
        let bytes = encode_copy(&data, 1);
        dev.bytes[at..at + bytes.len()].copy_from_slice(&bytes);
    }
    let before = dev.bytes.clone();
    let err = set(&mut dev, &geometry(), &[("trial", Some("b"))]).unwrap_err();
    assert_eq!(
        err,
        "no valid U-Boot environment of 1024 bytes here: U-Boot may be built with another CONFIG_ENV_SIZE"
    );
    assert_eq!(dev.bytes, before);
    // The same device read at its real size works.
    set(&mut dev, &big, &[("trial", Some("b"))]).unwrap();
}

/// One valid copy (the other torn by an earlier cut) is the active one, and the torn copy is the
/// one written: two sets in a row from that state both succeed.
#[test]
fn a_torn_inactive_copy_is_rewritten_and_the_pair_is_whole_again() {
    let g = geometry();
    let mut dev = Mem::new(8192);
    put(&mut dev, 0, &[("default", "a")], 9);
    dev.bytes[4096..4096 + 1024].fill(0x5A);
    set(&mut dev, &g, &[("trial", Some("x"))]).unwrap();
    set(&mut dev, &g, &[("trial", Some("y"))]).unwrap();
    let (_, got) = load(&mut dev, &g).unwrap().unwrap();
    assert_eq!(got, pairs(&[("default", "a"), ("trial", "y")]));
    let (a, b) = (dev.bytes[4], dev.bytes[4096 + 4]);
    assert_eq!((a, b), (11, 10));
}

/// A device that loses power in a way a prefix cut does not describe: each of the copy's sectors
/// either lands, stays as it was, or is torn (a new head over an old or garbage tail); sectors in
/// any combination. For every one the selected environment is whole, old or new.
#[test]
fn torn_and_reordered_sector_writes_never_leave_a_bad_environment() {
    // Three sectors per copy, so combinations are many.
    let g = Geometry::new(1536, 0, 4096).unwrap();
    let old = pairs(&[("default", "a"), ("trial", "x")]);
    let new = pairs(&[("default", "a"), ("trial", "y")]);
    let sectors = g.padded() / super::SECTOR;
    assert_eq!(sectors, 3);
    for active_first in [true, false] {
        for garbage in [0xEEu8, 0x00, 0xFF] {
            // What the finished write would put in the inactive copy.
            let mut done = Mem::new(8192);
            let put_at = |d: &mut Mem, at: u64, flags: u8, v: &[(&str, &str)]| {
                let list = pairs(v);
                let data = encode_vars(&list, g.data_len()).unwrap();
                let b = encode_copy(&data, flags);
                d.bytes[at as usize..at as usize + b.len()].copy_from_slice(&b);
            };
            let (active, inactive) = if active_first {
                (0u64, 4096u64)
            } else {
                (4096, 0)
            };
            put_at(&mut done, active, 6, &[("default", "a"), ("trial", "x")]);
            put_at(&mut done, inactive, 5, &[("default", "zz")]);
            let start = done.bytes.clone();
            set(&mut done, &g, &[("trial", Some("y"))]).unwrap();
            let finished = done.bytes.clone();
            // Per sector: 0 stays old, 1 lands, 2 is torn at the middle of the sector over garbage,
            // 3 is torn over the old contents.
            for combo in 0..4usize.pow(sectors as u32) {
                let mut dev = Mem::new(8192);
                dev.bytes.copy_from_slice(&start);
                let mut c = combo;
                for k in 0..sectors {
                    let lo = inactive as usize + k * 512;
                    match c % 4 {
                        0 => {}
                        1 => dev.bytes[lo..lo + 512].copy_from_slice(&finished[lo..lo + 512]),
                        2 => {
                            dev.bytes[lo..lo + 256].copy_from_slice(&finished[lo..lo + 256]);
                            dev.bytes[lo + 256..lo + 512].fill(garbage);
                        }
                        _ => dev.bytes[lo..lo + 256].copy_from_slice(&finished[lo..lo + 256]),
                    }
                    c /= 4;
                }
                let (_, got) = load(&mut dev, &g)
                    .unwrap()
                    .unwrap_or_else(|| panic!("no environment after combo {combo}"));
                assert!(
                    got == old || got == new,
                    "combo {combo} (active first: {active_first}) left {got:?}"
                );
                // And the next write from that state works and ends whole.
                set(&mut dev, &g, &[("trial", Some("z"))]).unwrap();
                let (_, after) = load(&mut dev, &g).unwrap().unwrap();
                assert_eq!(after, pairs(&[("default", "a"), ("trial", "z")]));
            }
        }
    }
}

/// From every serial value, a cut at every byte keeps a whole environment, including the wrap.
#[test]
fn a_cut_leaves_a_whole_environment_from_every_serial_pair() {
    let g = geometry();
    for serial in 0..=255u8 {
        for cut in [0usize, 1, 4, 5, 6, 511, 512, 513, 1023] {
            let mut dev = Mem::new(8192);
            put(&mut dev, 0, &[("default", "a"), ("trial", "x")], serial);
            put(&mut dev, 4096, &[("default", "zz")], serial.wrapping_sub(1));
            dev.budget = cut;
            let _ = set(&mut dev, &g, &[("trial", Some("y"))]);
            dev.budget = usize::MAX;
            let (_, got) = load(&mut dev, &g)
                .unwrap()
                .unwrap_or_else(|| panic!("none at serial {serial} cut {cut}"));
            let old = pairs(&[("default", "a"), ("trial", "x")]);
            let new = pairs(&[("default", "a"), ("trial", "y")]);
            assert!(
                got == old || got == new,
                "serial {serial} cut {cut}: {got:?}"
            );
        }
    }
}

/// The whole GPT path on a disk image: the partition found by its type GUID, the copies at the top
/// of it, a write that U-Boot's rule reads back, and the refusal when the size differs.
#[test]
fn the_gpt_form_finds_the_partition_and_writes_the_copies_at_its_top() {
    use crate::gpt::{ENV_TYPE_BYTES, image};
    let blocks = 600u64;
    let mut disk = Mem::new(0);
    disk.bytes = image(
        &[
            ([1; 16], 34, 99),
            (ENV_TYPE_BYTES, 100, 227),
            ([2; 16], 228, 500),
        ],
        blocks,
    );
    let place = Place::gpt(8192).unwrap();
    let len = disk.bytes.len() as u64;
    let g = place.geometry(&mut disk, len).unwrap();
    assert_eq!(
        (g.offset, g.offset_redund),
        ((228 - 16) * 512, (228 - 32) * 512)
    );
    // An environment U-Boot saved: copy 0 first, serial 1.
    let data = encode_vars(
        &pairs(&[("default", "a"), ("bootcmd", "run distro")]),
        g.data_len(),
    )
    .unwrap();
    let at = g.offset as usize;
    let bytes = encode_copy(&data, 1);
    disk.bytes[at..at + bytes.len()].copy_from_slice(&bytes);
    set(
        &mut disk,
        &g,
        &[("trial", Some("b")), ("trial_attempts", Some("3"))],
    )
    .unwrap();
    let (which, got) = load(&mut disk, &g).unwrap().unwrap();
    assert_eq!(which, Which::Second);
    // U-Boot's own variables are kept, ours added.
    assert_eq!(
        got,
        pairs(&[
            ("default", "a"),
            ("bootcmd", "run distro"),
            ("trial", "b"),
            ("trial_attempts", "3")
        ])
    );
    // Nothing outside the two copies changed.
    let mut changed: Vec<usize> = Vec::new();
    let mut before = image(
        &[
            ([1; 16], 34, 99),
            (ENV_TYPE_BYTES, 100, 227),
            ([2; 16], 228, 500),
        ],
        blocks,
    );
    before[at..at + bytes.len()].copy_from_slice(&bytes);
    for (i, (a, b)) in before.iter().zip(&disk.bytes).enumerate() {
        if a != b {
            changed.push(i);
        }
    }
    let lo = ((228 - 32) * 512) as usize;
    assert!(
        changed.iter().all(|&i| (lo..lo + 8192).contains(&i)),
        "{changed:?}"
    );
    // The board file says another size: both CRCs fail and the refusal names it.
    let other = Place::gpt(4096).unwrap();
    let g2 = other.geometry(&mut disk, len).unwrap();
    let err = set(&mut disk, &g2, &[("trial", Some("c"))]).unwrap_err();
    assert!(
        err.contains("of 4096 bytes here: U-Boot may be built with another CONFIG_ENV_SIZE"),
        "{err}"
    );
}
