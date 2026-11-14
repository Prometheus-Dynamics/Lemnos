//! End to end, on image files, for both forms of the boot partition: the real code of all three
//! parts in one run. The image is `bootimage::build`'s (the code `lemnos-ctl boot-partition` runs);
//! slots are installed, tried, confirmed and removed through the daemon's own record types
//! (`lemnos_driver_system_slots::boot::Tryboot` and `uboot::UbootEnv`, the `BootRecord` calls the
//! driver makes); slot-init's `boot` runs as it would at each next boot (its every-boot check against
//! the slot manifest, its record reading); and `boot-partition --verify`'s code (`bootimage::verify`)
//! accepts the image after every step.

use std::path::PathBuf;

use lemnos_driver_system_slots::boot::{BootRecord, Restarter, Tryboot};
use lemnos_driver_system_slots::uboot::UbootEnv;
use lemnos_slot_store::bootimage::Mem;

use super::*;

fn restarter() -> Restarter {
    Restarter {
        systemctl: "/nonexistent/systemctl".into(),
        check: None,
    }
}

/// An installed slot's data-partition side: manifest, image, meta. The boot files are the driver's.
fn register(board: &Board, id: &str, seq: u64) {
    board.manifest(id, id, "");
    board.write(
        &format!("data/slots/{id}.erofs"),
        &vec![0u8; (BLOCK + tree_bytes(1)) as usize],
    );
    board.write(
        &format!("data/board/slots/{id}.meta"),
        format!("GOOD=yes\nSEQ={seq}\n").as_bytes(),
    );
}

/// The state file the driver keeps on the data partition.
fn state(board: &Board, default: &str, previous: &str) {
    board.write(
        "data/board/slots/state.env",
        format!("DEFAULT={default}\nPREVIOUS={previous}\nSTATE=confirmed\n").as_bytes(),
    );
}

/// `boot-partition --verify`'s code on the board's image: every finding holds.
fn verify(board: &Board, parts: &Parts, stage: &str) {
    let image = fs::read(board.node()).unwrap();
    let bad: Vec<String> = bootimage::verify(&mut Mem(image), parts)
        .into_iter()
        .filter(|f| !f.ok)
        .map(|f| f.text)
        .collect();
    assert!(bad.is_empty(), "{stage}: boot-partition --verify: {bad:?}");
}

/// A boot of `slot` as the firmware or U-Boot would start it: the command line is what the bay's
/// own command line says. The slot-init result must be a switch into its root.
fn boots(board: &Board, slot: &str, stage: &str) {
    let (fake, why) = board.boot(&[]);
    assert_eq!(why, "", "{stage}: {:?}", fake.logs.borrow());
    assert_eq!(fake.last(), "switch_root newroot", "{stage}");
    assert_eq!(
        board.read("newroot/data/board/slots/booted"),
        format!("{slot}\n"),
        "{stage}"
    );
}

/// The sectors that differ between two images of the same size.
fn changed(before: &[u8], after: &[u8]) -> Vec<u64> {
    assert_eq!(before.len(), after.len());
    before
        .chunks(512)
        .zip(after.chunks(512))
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, _)| i as u64)
        .collect()
}

/// A set's sectors: its five files and its record pair.
fn set_lbas(image: &[u8], set: usize) -> (Vec<u64>, [u64; 2]) {
    let layout = test_bays();
    let sets = bay::place_sets(&mut Mem(image.to_vec()), &layout).unwrap();
    let (files, info) = sets[set];
    let mut out = Vec::new();
    for (i, lba) in files.iter().enumerate() {
        for k in 0..layout.bytes[i] / 512 {
            out.push(lba + k);
        }
    }
    (out, [info, info + 1])
}

/// The bay name of a slot, read through the record's own bay lookup.
fn bay_of(board: &Board, slot: &str) -> Option<String> {
    let mut volume = BayVolume::open(&board.node()).ok()?;
    let (dir, _) = volume.slot_bay(slot).ok()?;
    dir.rsplit('/').next().map(str::to_string)
}

/// The command line the bay named by `bay` holds, as the firmware reads it (`cmdline.txt` of the set).
fn bay_cmdline(board: &Board, bay: &str) -> String {
    let mut file = fs::File::open(board.node()).unwrap();
    let bytes = bootcheck::fat_read(&mut file, &format!("slots/{bay}/cmdline.txt"))
        .unwrap()
        .expect("the bay's command line");
    String::from_utf8_lossy(&bytes)
        .trim_end_matches(['\0', ' ', '\n'])
        .to_string()
}

fn start(board: &Board, cmdline: &str) {
    board.write("proc/cmdline", format!("{cmdline}\n").as_bytes());
}

#[test]
fn end_to_end_pi_firmware_form_install_trial_confirm_keep_previous_free() {
    let parts = tryboot_parts(Vec::new());
    let board = Board::fresh(tryboot_image(Vec::new()), false);
    let node = board.node();
    let record = Tryboot {
        boot: board.layout.boot.clone(),
        node: Ok(node.clone()),
        restarter: restarter(),
    };
    let mut record_rw = record.clone();

    // Slot A installed by the image builder: slot-init's every-boot check passes on it.
    register(&board, "os-1", 1);
    state(&board, "os-1", "");
    board.cmdline("os-1", &hash("os-1"));
    boots(&board, "os-1", "A, from the image");
    verify(&board, &parts, "A, from the image");
    assert_eq!(bay_of(&board, "os-1").as_deref(), Some("bay0"));

    // Slot B through the driver's install: the next free bay, the default untouched.
    let before = fs::read(&node).unwrap();
    let p = payloads("os-2");
    record.install("os-2", &refs(&p)).unwrap();
    register(&board, "os-2", 2);
    let after = fs::read(&node).unwrap();
    let (files, info) = set_lbas(&before, 1);
    for lba in changed(&before, &after) {
        assert!(
            files.contains(&lba) || info.contains(&lba),
            "the install of B wrote sector {lba}, outside bay1's files and record"
        );
    }
    assert_eq!(bay_of(&board, "os-2").as_deref(), Some("bay1"));
    boots(&board, "os-1", "B installed, A still boots");
    verify(&board, &parts, "B installed");

    // Arm a trial of B: the record and the trial pointer; the default's slot-init takes an attempt and
    // restarts into the trial (what the firmware does with the tryboot flag next).
    record_rw.set_trial("os-2", 3).unwrap();
    assert_eq!(board.trial(), Some(("os-2".to_string(), 3)));
    assert_eq!(board.pointer(Pointer::Trial).as_deref(), Some("os-2"));
    verify(&board, &parts, "trial armed");
    let (fake, why) = board.boot(&[]);
    assert!(fake.called("restart"), "{why}: {:?}", fake.calls());
    assert_eq!(board.trial(), Some(("os-2".to_string(), 2)));
    // The trial boot: the firmware reads the trial pointer's bay and starts its command line.
    let trial_bay = bay_of(&board, "os-2").unwrap();
    board.write(
        lemnos_slot_store::records::tryboot::TRIAL_FLAG,
        &[0, 0, 0, 1],
    );
    start(&board, &bay_cmdline(&board, &trial_bay));
    boots(
        &board,
        "os-2",
        "B on trial: slot-init selects B's bay and passes",
    );
    assert_eq!(
        board.trial().map(|t| t.1),
        Some(2),
        "the trial boot takes nothing more"
    );
    verify(&board, &parts, "B on trial");

    // Confirm B: the default moves to B's bay, A's is kept as the previous.
    record_rw.set_default("os-2").unwrap();
    record_rw.clear_trial().unwrap();
    state(&board, "os-2", "os-1");
    let _ = fs::remove_file(
        board
            .env
            .at(lemnos_slot_store::records::tryboot::TRIAL_FLAG),
    );
    assert_eq!(board.pointer(Pointer::Default).as_deref(), Some("os-2"));
    assert_eq!(board.pointer(Pointer::Previous).as_deref(), Some("os-1"));
    assert_eq!(board.trial(), None);
    verify(&board, &parts, "B confirmed");
    board.cmdline("os-2", &hash("os-2"));
    boots(&board, "os-2", "B confirmed");

    // Slot C: the remaining free bay, never A's (previous) or B's (default).
    let before = fs::read(&node).unwrap();
    let p = payloads("os-3");
    record.install("os-3", &refs(&p)).unwrap();
    register(&board, "os-3", 3);
    let after = fs::read(&node).unwrap();
    assert_eq!(bay_of(&board, "os-3").as_deref(), Some("bay2"));
    let (files, info) = set_lbas(&before, 2);
    for lba in changed(&before, &after) {
        assert!(
            files.contains(&lba) || info.contains(&lba),
            "the install of C wrote sector {lba}, outside bay2's files and record"
        );
    }
    assert_eq!(bay_of(&board, "os-1").as_deref(), Some("bay0"));
    assert_eq!(bay_of(&board, "os-2").as_deref(), Some("bay1"));
    boots(&board, "os-2", "C installed");
    verify(&board, &parts, "C installed");

    // Remove C: its bay is freed by a record write alone.
    let before = fs::read(&node).unwrap();
    record.free("os-3").unwrap();
    let after = fs::read(&node).unwrap();
    assert_eq!(bay_of(&board, "os-3"), None);
    let (_, info) = set_lbas(&before, 2);
    let diff = changed(&before, &after);
    assert!(
        !diff.is_empty() && diff.iter().all(|l| info.contains(l)),
        "the removal wrote {diff:?}, not only bay2's record {info:?}"
    );
    // A, the previous, and B, the default, are refused and untouched.
    assert!(record.free("os-1").is_err());
    assert!(record.free("os-2").is_err());
    boots(&board, "os-2", "C removed");
    verify(&board, &parts, "C removed");
}

#[test]
fn end_to_end_uboot_form_install_trial_confirm_keep_previous_free() {
    let locator = uboot_locator();
    let parts = uboot_parts(&locator);
    let board = Board::fresh(uboot_image(&locator), true);
    // The boot disk: a GPT disk with the environment partition at its top, both copies holding the
    // default (A) and its bay.
    board.write(
        "dev/mmcblk0",
        &Board::gpt_disk(&[(ENV_TYPE_BYTES, 64, 127)]),
    );
    let disk = board.env.dev.join("mmcblk0");
    let place = Place::gpt(1024).unwrap();
    {
        let (mut dev, bytes) = FileEnv::open_sized(&disk).unwrap();
        let g = place.geometry(&mut dev, bytes).unwrap();
        uboot_env::set(
            &mut dev,
            &g,
            &[("default", Some("os-1")), ("default_bay", Some("bay0"))],
        )
        .unwrap();
    }
    let node = board.node();
    let mut record = UbootEnv {
        disk: Ok(disk.clone()),
        place: place.clone(),
        restarter: restarter(),
        boot_node: Ok(node.clone()),
    };
    let var = |name: &str| {
        board
            .uboot_vars()
            .into_iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
    };

    // Slot A from the image.
    register(&board, "os-1", 1);
    state(&board, "os-1", "");
    board.cmdline("os-1", &hash("os-1"));
    boots(&board, "os-1", "A, from the image");
    verify(&board, &parts, "A, from the image");

    // B through the driver: the free bay, A's (the default's) untouched.
    let before = fs::read(&node).unwrap();
    let p = payloads("os-2");
    record.install("os-2", &refs(&p)).unwrap();
    register(&board, "os-2", 2);
    let after = fs::read(&node).unwrap();
    assert_eq!(bay_of(&board, "os-2").as_deref(), Some("bay1"));
    let (files, info) = set_lbas(&before, 1);
    let extlinux = {
        let mut dev = Mem(before.clone());
        let found = lemnos_slot_store::bootfat::locate_path(
            &mut dev,
            "slots/bay1/extlinux/extlinux.conf",
            bootimage::EXTLINUX_BYTES,
        )
        .unwrap();
        (0..bootimage::EXTLINUX_BYTES / 512)
            .map(|k| found.first_lba + k)
            .collect::<Vec<_>>()
    };
    for lba in changed(&before, &after) {
        assert!(
            files.contains(&lba) || info.contains(&lba) || extlinux.contains(&lba),
            "the install of B wrote sector {lba}, outside bay1"
        );
    }
    boots(&board, "os-1", "B installed, A still boots");
    verify(&board, &parts, "B installed");

    // Arm a trial of B: the environment names it and its bay; U-Boot's script takes the attempt
    // (here: the same variable write it makes), reads that bay's extlinux and starts its command line.
    record.set_trial("os-2", 3).unwrap();
    assert_eq!(var("trial").as_deref(), Some("os-2"));
    assert_eq!(var("trial_bay").as_deref(), Some("bay1"));
    {
        let (mut dev, bytes) = FileEnv::open_sized(&disk).unwrap();
        let g = place.geometry(&mut dev, bytes).unwrap();
        uboot_env::set(&mut dev, &g, &[("trial_attempts", Some("2"))]).unwrap();
    }
    verify(&board, &parts, "trial armed");
    let trial_bay = var("trial_bay").unwrap();
    let mut file = fs::File::open(&node).unwrap();
    let conf = bootcheck::fat_read(
        &mut file,
        &format!("slots/{trial_bay}/extlinux/extlinux.conf"),
    )
    .unwrap()
    .expect("the trial bay's extlinux");
    let conf = String::from_utf8(conf).unwrap();
    let append = conf
        .lines()
        .find_map(|l| l.trim().strip_prefix("APPEND "))
        .expect("an APPEND line");
    assert!(append.contains("os.slot=os-2"), "{append}");
    start(&board, append);
    boots(
        &board,
        "os-2",
        "B on trial: slot-init selects B's bay and passes",
    );
    verify(&board, &parts, "B on trial");

    // Confirm B: default and bay move, A's bay is the previous.
    record.set_default("os-2").unwrap();
    record.clear_trial().unwrap();
    state(&board, "os-2", "os-1");
    assert_eq!(var("default").as_deref(), Some("os-2"));
    assert_eq!(var("default_bay").as_deref(), Some("bay1"));
    assert_eq!(var("previous_bay").as_deref(), Some("bay0"));
    assert_eq!(var("trial"), None);
    verify(&board, &parts, "B confirmed");
    boots(&board, "os-2", "B confirmed");

    // C: the remaining free bay, never A's or B's.
    let before = fs::read(&node).unwrap();
    let p = payloads("os-3");
    record.install("os-3", &refs(&p)).unwrap();
    register(&board, "os-3", 3);
    assert_eq!(bay_of(&board, "os-3").as_deref(), Some("bay2"));
    assert_eq!(bay_of(&board, "os-1").as_deref(), Some("bay0"));
    assert_eq!(bay_of(&board, "os-2").as_deref(), Some("bay1"));
    let (files, info) = set_lbas(&before, 0);
    let after = fs::read(&node).unwrap();
    for lba in changed(&before, &after) {
        assert!(
            !files.contains(&lba) && !info.contains(&lba),
            "the install of C wrote sector {lba} in A's bay"
        );
    }
    boots(&board, "os-2", "C installed");
    verify(&board, &parts, "C installed");

    // Remove C: a record write only.
    let before = fs::read(&node).unwrap();
    record.free("os-3").unwrap();
    let after = fs::read(&node).unwrap();
    assert_eq!(bay_of(&board, "os-3"), None);
    let (_, info) = set_lbas(&before, 2);
    let diff = changed(&before, &after);
    assert!(
        !diff.is_empty() && diff.iter().all(|l| info.contains(l)),
        "the removal wrote {diff:?}, not only bay2's record {info:?}"
    );
    assert!(record.free("os-1").is_err(), "the previous's bay is kept");
    assert!(record.free("os-2").is_err(), "the default's bay is kept");
    boots(&board, "os-2", "C removed");
    verify(&board, &parts, "C removed");
    let _: PathBuf = node;
}
