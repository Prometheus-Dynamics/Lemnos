//! `lemnos-ctl boot-partition --add-to`: the reference board's files added to a dump of another
//! system's boot partition, through the command. The old system is the stand-in
//! `lemnos_slot_store::fatimage::foreign_system` (the reference board's partition geometry, 44 files
//! of real sizes, seeded contents). Files are written under the test target directory only; nothing
//! here touches a block device.
//!
//! `the_same_assertions_hold_on_a_real_dump_when_one_is_given` runs the same checks on a copy of a
//! real partition dump when `LEMNOS_REAL_BOOT_DUMP` names one, and prints that it was skipped when
//! it does not: a skip is not a pass.

// The skip lines and the real-dump figures are this test's report: a skip must be seen.
#![allow(clippy::print_stdout)]

use lemnos_slot_store::bootfat;
use lemnos_slot_store::bootimage::Mem;
use lemnos_slot_store::fatadd::{self, Existing};
use lemnos_slot_store::fatimage::{foreign_system, seeded};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[path = "support/pi.rs"]
mod pi;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn board() -> String {
    repo()
        .join("testing/boards/reference.toml")
        .display()
        .to_string()
}

fn socs() -> String {
    repo().join("socs").display().to_string()
}

fn run(args: &[&str]) -> (Result<(), String>, String) {
    let args = pi::with_firmware(args);
    let mut out = Vec::new();
    let r = lemnos_ctl::run(&args, &mut out);
    (r, String::from_utf8(out).unwrap())
}

/// A directory for one test, removed when the test ends (the volumes are 79 MiB each).
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).display().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The five payloads of a slot, shaped as the firmware reads them and told apart by `seed` (the
/// kernel's length and the command line), each inside the reference board's bay; the board's
/// firmware files go beside them.
fn payloads(dir: &Scratch, name: &str, seed: u64) -> String {
    let payload = dir.0.join(name);
    let line = format!("console=serial0,115200 root=lemnos seed={seed}\n");
    pi::write_payload(&payload, line.as_bytes(), 8_000_000 + seed as usize * 4096);
    pi::write_firmware(&pi::firmware_beside(&payload), &[]);
    payload.display().to_string()
}

fn sha(bytes: &[u8]) -> String {
    lemnos_slot_store::bundle::hex(&Sha256::digest(bytes))
}

/// Every file of a volume with its bytes.
fn contents(image: &[u8]) -> Vec<(Existing, Vec<u8>)> {
    let sv = fatadd::survey(image).unwrap();
    let mut mem = Mem(image.to_vec());
    sv.entries
        .iter()
        .filter(|e| !e.is_dir)
        .map(|e| {
            (
                e.clone(),
                bootfat::read_file(&mut mem, &e.path, u64::MAX).unwrap(),
            )
        })
        .collect()
}

/// What one `--add-to` run gave.
struct Added {
    text: String,
    image: Vec<u8>,
    out: String,
    undo: String,
}

/// Runs `--add-to` on the dump at `dump` for slot `slot` into `set`, and asserts everything the
/// command promises about its result:
///
/// - the dump file itself is not written;
/// - every file the dump held is byte-identical, with the same directory entry and the same
///   cluster chain (the two files `--replace-trial` replaces excepted, when it is given; and for a
///   second slot the bytes, not the place, of the files of the set it is installed into);
/// - the entries of `config.txt` and `default.txt` are the same 32 bytes;
/// - the FAT copies are equal, and every added file is one run of exactly its size where the
///   command's `added` line says;
/// - `boot-partition --verify` on the result prints exactly the BAD lines the command announced;
/// - the changed-sector list is complete and exact, and the undo file gives the dump back.
fn add_and_check(
    dir: &Scratch,
    tag: &str,
    dump: &str,
    slot: &str,
    set: u8,
    replace: bool,
    payload: &str,
) -> Added {
    let old = std::fs::read(dump).unwrap();
    let (out, list, undo) = (
        dir.path(&format!("{tag}.img")),
        dir.path(&format!("{tag}.changed")),
        dir.path(&format!("{tag}.undo")),
    );
    let set_text = set.to_string();
    let (board, socs) = (board(), socs());
    let mut args = vec![
        "boot-partition",
        "--add-to",
        dump,
        "--layout-of",
        &board,
        "--socs",
        &socs,
        "--slot",
        slot,
        "--payload",
        payload,
        "--set",
        &set_text,
        "--out",
        &out,
        "--changed-out",
        &list,
        "--undo-out",
        &undo,
    ];
    if replace {
        args.push("--replace-trial");
    }
    let (r, text) = run(&args);
    r.unwrap_or_else(|e| panic!("--add-to failed: {e}\n{text}"));
    assert!(
        std::fs::read(dump).unwrap() == old,
        "the dump is not written"
    );
    let image = std::fs::read(&out).unwrap();
    assert_eq!(image.len(), old.len());

    // Every old file: the same entry, chain and bytes.
    let before = fatadd::survey(&old).unwrap();
    let replaced: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("replaced "))
        .map(|l| l.split(' ').next().unwrap())
        .collect();
    let after_files = contents(&image);
    let installed_only = text.starts_with("mode: files already there");
    for (entry, bytes) in contents(&old) {
        if replaced.iter().any(|r| r.eq_ignore_ascii_case(&entry.path)) {
            continue;
        }
        let (now, now_bytes) = after_files
            .iter()
            .find(|(e, _)| e.path == entry.path)
            .unwrap_or_else(|| panic!("{} is still there", entry.path));
        // A slot installed into a set the first run laid out reshapes that set's `reserve`: it
        // keeps what the files leave of the region, so its clusters stay inside the old ones.
        let own_set = installed_only
            && entry
                .path
                .to_ascii_lowercase()
                .starts_with(&format!("slots/bay{set}/"));
        if own_set && entry.path.to_ascii_lowercase().ends_with("/reserve") {
            let region = entry.chain.first().unwrap()..=entry.chain.last().unwrap();
            assert!(
                now.chain.iter().all(|c| region.contains(&c)),
                "{}: inside its region",
                entry.path
            );
            continue;
        }
        assert_eq!(
            *now, entry,
            "{}: the same entry, first cluster, size and chain",
            entry.path
        );
        // A second slot is written into the files of its own set, which the first run placed:
        // those keep their entries and clusters, and their bytes are the new slot's.
        let own = installed_only
            && entry
                .path
                .to_ascii_lowercase()
                .starts_with(&format!("slots/bay{set}/"));
        assert!(own || *now_bytes == bytes, "{}: byte-identical", entry.path);
    }
    for dirent in before.entries.iter().filter(|e| e.is_dir) {
        let after = fatadd::survey(&image).unwrap();
        assert_eq!(
            after.find(&dirent.path),
            Some(dirent),
            "{}: the same directory",
            dirent.path
        );
    }
    for name in ["config.txt", "default.txt"] {
        if let Some(e) = before.find(name) {
            assert!(
                old[e.at..e.at + 32] == image[e.at..e.at + 32],
                "{name}: the same directory entry"
            );
        }
    }
    let geo = before.geo;
    let (fat, first) = (geo.fat_sectors as usize * 512, geo.reserved as usize * 512);
    assert!(
        image[first..first + fat] == image[first + fat..first + 2 * fat],
        "both FAT copies are equal"
    );

    // Every added file: one run of exactly its size, where its line says.
    let mut mem = Mem(image.clone());
    for line in text.lines().filter_map(|l| l.strip_prefix("added ")) {
        let f: Vec<&str> = line.split(' ').collect();
        let (size, first, clusters): (u64, u64, u64) = (
            f[1].parse().unwrap(),
            f[2].parse().unwrap(),
            f[3].parse().unwrap(),
        );
        let found =
            bootfat::locate_path(&mut mem, f[0], size).unwrap_or_else(|e| panic!("{}: {e}", f[0]));
        assert_eq!(found.first_lba, geo.cluster_lba(first), "{}", f[0]);
        assert_eq!(
            clusters,
            size.div_ceil(geo.cluster_bytes() as u64).max(1),
            "{}",
            f[0]
        );
        assert!(
            before.free[first as usize] && before.free[(first + clusters - 1) as usize],
            "{}",
            f[0]
        );
    }

    // Every added region: whole FAT sectors of clusters, starting on one, free before the add.
    let mut regions = 0;
    for line in text.lines().filter_map(|l| l.strip_prefix("added-region ")) {
        let f: Vec<&str> = line.split(' ').collect();
        let (size, first, clusters): (u64, u64, u64) = (
            f[1].parse().unwrap(),
            f[2].parse().unwrap(),
            f[3].parse().unwrap(),
        );
        assert!(f[0].ends_with("/reserve"), "{line}");
        assert_eq!(size, clusters * geo.cluster_bytes() as u64, "{line}");
        assert!(first % 256 == 0 && clusters % 256 == 0, "{line}");
        assert!(
            (first..first + clusters).all(|c| before.free[c as usize]),
            "{line}"
        );
        regions += 1;
    }
    assert!(installed_only || regions > 0, "the add made the regions");

    // The verify of the result, through the command: exactly the announced BAD lines.
    let expected: Vec<&str> = text
        .lines()
        .find_map(|l| l.strip_prefix("expected BAD: "))
        .expect("the expected set is printed")
        .split(", ")
        .collect();
    let (r, verify) = run(&[
        "boot-partition",
        "--verify",
        &out,
        "--layout-of",
        &board,
        "--socs",
        &socs,
    ]);
    let bad: Vec<&str> = verify.lines().filter(|l| l.starts_with("BAD")).collect();
    assert_eq!(bad.len(), expected.len(), "{verify}");
    for name in &expected {
        assert_eq!(
            bad.iter()
                .filter(|l| l.starts_with(&format!("BAD {name}: ")))
                .count(),
            1,
            "one BAD line for {name}: {verify}"
        );
    }
    assert_eq!(
        r.unwrap_err(),
        format!("{out}: {} finding(s) failed", expected.len())
    );
    for line in verify.lines() {
        assert!(
            text.contains(line),
            "the command printed the same verify line: {line}"
        );
    }

    // The changed-sector list: complete (applying it gives the result) and exact (a sector is
    // listed if and only if it differs), each range with the hashes of its bytes.
    let mut applied = old.clone();
    let mut listed = vec![false; old.len() / 512];
    let ranges = std::fs::read_to_string(&list).unwrap();
    for line in ranges.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split(' ').collect();
        assert_eq!(f.len(), 5, "{line}");
        let (lba, count): (usize, usize) = (f[0].parse().unwrap(), f[1].parse().unwrap());
        assert!(
            [
                "fat1", "fat2", "root-dir", "dir", "new-dir", "new-file", "own-file"
            ]
            .contains(&f[2]),
            "{line}"
        );
        let (from, to) = (lba * 512, (lba + count) * 512);
        assert_eq!(f[3], sha(&old[from..to]), "{line}");
        assert_eq!(f[4], sha(&image[from..to]), "{line}");
        applied[from..to].copy_from_slice(&image[from..to]);
        for (s, seen) in listed.iter_mut().enumerate().skip(lba).take(count) {
            assert!(!*seen, "sector {s} is listed once");
            *seen = true;
        }
    }
    assert!(applied == image, "the listed sectors alone give the result");
    for (s, &is_listed) in listed.iter().enumerate() {
        assert_eq!(
            is_listed,
            old[s * 512..(s + 1) * 512] != image[s * 512..(s + 1) * 512],
            "sector {s}"
        );
    }
    assert!(!listed[0], "the boot sector is not changed");

    // The undo file, in the library and through the command.
    let undo_bytes = std::fs::read(&undo).unwrap();
    assert!(
        fatadd::apply_undo(&image, &undo_bytes).unwrap() == old,
        "the undo file gives the dump back"
    );
    let restored = dir.path(&format!("{tag}.restored"));
    let (r, undo_text) = run(&[
        "boot-partition",
        "--undo",
        &undo,
        "--of",
        &out,
        "--out",
        &restored,
    ]);
    r.unwrap_or_else(|e| panic!("--undo failed: {e}"));
    assert!(
        std::fs::read(&restored).unwrap() == old,
        "--undo gives the dump back, byte for byte"
    );
    assert!(
        undo_text.contains(&format!("sha256 {}", sha(&old))),
        "{undo_text}"
    );
    std::fs::remove_file(&restored).unwrap();
    Added {
        text,
        image,
        out,
        undo,
    }
}

#[test]
fn the_reference_board_is_added_to_an_old_system_and_verify_prints_exactly_the_expected_bad_lines()
{
    let dir = Scratch::new("add-to-old-system");
    let dump = dir.path("old.img");
    std::fs::write(&dump, foreign_system()).unwrap();
    let payload = payloads(&dir, "payload-a", 1);

    // Without --replace-trial: the old system's trial.txt and tryboot.txt stay, and are BAD too.
    let plain = add_and_check(&dir, "plain", &dump, "ours-a", 0, false, &payload);
    assert!(
        plain
            .text
            .contains("mode: files added, slot ours-a installed into bay0\n"),
        "{}",
        plain.text
    );
    assert!(
        plain
            .text
            .contains("expected BAD: default.txt, config.txt, trial.txt, tryboot.txt\n")
    );
    assert!(plain.text.contains("left trial.txt: the existing system's"));
    assert!(plain.text.contains(
        "BAD default.txt: record file is 55 bytes, expected 1024: the image is not ours\n"
    ));
    assert!(
        plain
            .text
            .contains("BAD config.txt: does not hold its generated bytes\n"),
        "{}",
        plain.text
    );
    assert!(plain.text.contains(
        "BAD trial.txt: record file is 55 bytes, expected 1024: the image is not ours\n"
    ));
    assert!(
        plain
            .text
            .contains("BAD tryboot.txt: does not hold its generated bytes\n")
    );
    assert!(
        plain
            .text
            .contains("ok  bay0 (ours-a): its 8 files match their record\n")
    );
    assert!(plain.text.contains("allocation: first fit"));
    // The files added for the reference board: every file its fresh image has, but the two kept.
    let added: Vec<&str> = plain
        .text
        .lines()
        .filter_map(|l| l.strip_prefix("added "))
        .map(|l| l.split(' ').next().unwrap())
        .collect();
    // The root files and each set's record are added as files; each set's region is added whole
    // (`added-region`), and the install writes the set's files inside it.
    let mut want = vec![
        "slot-layout.toml",
        "firmware-overlays.txt",
        "previous.txt",
        "trial.rec",
        "reason.rec",
        "episodes.rec",
        "count.rec",
        "secure.rec",
    ];
    let records: Vec<String> = (0..3)
        .map(|n| format!("slots/bay{n}/bayinfo.rec"))
        .collect();
    want.extend(records.iter().map(String::as_str));
    assert_eq!(added, want);
    let regions: Vec<&str> = plain
        .text
        .lines()
        .filter_map(|l| l.strip_prefix("added-region "))
        .map(|l| l.split(' ').next().unwrap())
        .collect();
    assert_eq!(
        regions,
        [
            "slots/bay0/reserve",
            "slots/bay1/reserve",
            "slots/bay2/reserve"
        ]
    );
    // The same inputs give the same three files.
    let again = add_and_check(&dir, "again", &dump, "ours-a", 0, false, &payload);
    assert!(again.image == plain.image);
    assert!(std::fs::read(&again.undo).unwrap() == std::fs::read(&plain.undo).unwrap());
    assert_eq!(
        std::fs::read(dir.path("again.changed")).unwrap(),
        std::fs::read(dir.path("plain.changed")).unwrap()
    );
    std::fs::remove_file(&again.out).unwrap();

    // With --replace-trial: exactly default.txt and config.txt are BAD.
    let replaced = add_and_check(&dir, "replaced", &dump, "ours-a", 0, true, &payload);
    assert!(
        replaced
            .text
            .contains("expected BAD: default.txt, config.txt\n"),
        "{}",
        replaced.text
    );
    assert!(replaced.text.contains("ok  trial.txt: names bay0\n"));
    assert!(
        replaced
            .text
            .contains("ok  tryboot.txt: holds its generated bytes\n")
    );
    let old = std::fs::read(&dump).unwrap();
    let mut mem = Mem(old.clone());
    for name in ["trial.txt", "tryboot.txt"] {
        let was = bootfat::read_file(&mut mem, name, u64::MAX).unwrap();
        let line = replaced
            .text
            .lines()
            .find(|l| l.starts_with(&format!("replaced {name} ")))
            .unwrap_or_else(|| panic!("{name} is reported replaced"));
        assert!(
            line.contains(&format!("was {} bytes, sha256 {}", was.len(), sha(&was))),
            "the old content's hash is printed: {line}"
        );
    }

    // A second slot into set 1 of the first result: set 0 is untouched, both are read.
    let payload_b = payloads(&dir, "payload-b", 2);
    let second = add_and_check(&dir, "second", &plain.out, "ours-b", 1, true, &payload_b);
    assert!(
        second
            .text
            .contains("mode: files already there, slot ours-b installed into bay1\n"),
        "{}",
        second.text
    );
    assert!(
        second
            .text
            .contains("ok  bay0 (ours-a): its 8 files match their record\n")
    );
    assert!(
        second
            .text
            .contains("ok  bay1 (ours-b): its 8 files match their record\n")
    );
    assert!(second.text.contains("ok  trial.txt: names bay1\n"));
    assert!(!second.text.lines().any(|l| l.starts_with("added ")));
}

#[test]
fn a_block_device_a_directory_and_a_file_that_is_not_fat16_are_refused_and_nothing_is_written() {
    let dir = Scratch::new("add-to-refusals");
    let payload = payloads(&dir, "payload", 3);
    let (board, socs) = (board(), socs());
    let (out, list, undo) = (
        dir.path("out.img"),
        dir.path("out.changed"),
        dir.path("out.undo"),
    );
    let attempt = |dump: &str| {
        let (r, text) = run(&[
            "boot-partition",
            "--add-to",
            dump,
            "--layout-of",
            &board,
            "--socs",
            &socs,
            "--slot",
            "ours",
            "--payload",
            &payload,
            "--out",
            &out,
            "--changed-out",
            &list,
            "--undo-out",
            &undo,
        ]);
        assert_eq!(text, "", "a refusal prints nothing to the output");
        for file in [&out, &list, &undo] {
            assert!(!Path::new(file).exists(), "{file} was not written");
        }
        r.unwrap_err()
    };
    // A block device: refused by its file type, before it is opened. Any block node will do; the
    // refusal reads its metadata only.
    let node = std::fs::read_dir("/sys/class/block")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| Path::new("/dev").join(e.file_name()))
        .find(|p| {
            use std::os::unix::fs::FileTypeExt;
            std::fs::metadata(p).is_ok_and(|m| m.file_type().is_block_device())
        });
    match node {
        Some(node) => {
            let node = node.display().to_string();
            assert_eq!(
                attempt(&node),
                format!(
                    "{node} is a block device: --add-to reads a dump of the partition (a regular file) and never a device; dump the partition to a file first: nothing was written"
                )
            );
            println!("block device refusal: ran against {node}");
        }
        None => println!(
            "SKIPPED: this host has no block device node: the block device refusal did not run"
        ),
    }
    assert_eq!(
        attempt(&dir.path("payload")),
        format!(
            "{} is not a regular file: --add-to reads a dump of the partition: nothing was written",
            dir.path("payload")
        )
    );
    // Not FAT: seeded bytes; a FAT12 and a FAT32 header on the stand-in's first sector.
    let noise = dir.path("noise.img");
    std::fs::write(&noise, seeded(5, 1024 * 1024)).unwrap();
    assert_eq!(
        attempt(&noise),
        format!(
            "{noise}: not a FAT16 volume: the first sector has no boot signature (55 AA): nothing was written"
        )
    );
    let small = lemnos_slot_store::fatimage::tree(&[]);
    let mut fat12 = small.clone();
    fat12[13] = 8;
    let file = dir.path("fat12.img");
    std::fs::write(&file, fat12).unwrap();
    assert_eq!(
        attempt(&file),
        format!(
            "{file}: a FAT12 volume (2027 clusters): --add-to writes FAT16 only: nothing was written"
        )
    );
    let mut fat32 = small.clone();
    fat32[17..19].copy_from_slice(&[0, 0]);
    fat32[22..24].copy_from_slice(&[0, 0]);
    let file = dir.path("fat32.img");
    std::fs::write(&file, fat32).unwrap();
    assert_eq!(
        attempt(&file),
        format!(
            "{file}: a FAT32 volume (no 16-bit FAT size or no fixed root directory): --add-to writes FAT16 only: nothing was written"
        )
    );
    // A FAT16 volume too small for the board's bays: the first set's region, which has no run, is named.
    let file = dir.path("small.img");
    std::fs::write(&file, &small).unwrap();
    let err = attempt(&file);
    assert!(
        err.starts_with(&format!(
            "{file}: slots/bay0/reserve: the bay's region needs one run of "
        )) && err.ends_with("nothing is ever moved to make room: nothing was written"),
        "{err}"
    );
}

#[test]
fn every_output_is_a_new_file_and_the_options_are_checked() {
    let dir = Scratch::new("add-to-options");
    let dump = dir.path("old.img");
    std::fs::write(&dump, lemnos_slot_store::fatimage::tree(&[])).unwrap();
    let payload = payloads(&dir, "payload", 4);
    let (board, socs) = (board(), socs());
    let (out, list, undo) = (
        dir.path("out.img"),
        dir.path("out.changed"),
        dir.path("out.undo"),
    );
    let base = [
        "boot-partition",
        "--add-to",
        &dump,
        "--layout-of",
        &board,
        "--socs",
        &socs,
        "--slot",
        "ours",
        "--payload",
        &payload,
    ];
    let with = |more: &[&str]| run(&[&base[..], more].concat()).0.unwrap_err();
    assert_eq!(
        with(&["--changed-out", &list, "--undo-out", &undo]),
        "--add-to needs --out FILE"
    );
    assert_eq!(
        with(&["--out", &out, "--undo-out", &undo]),
        "--add-to needs --changed-out LIST"
    );
    assert_eq!(
        with(&["--out", &out, "--changed-out", &list]),
        "--add-to needs --undo-out UNDO"
    );
    assert_eq!(
        with(&[
            "--out",
            &out,
            "--changed-out",
            &list,
            "--undo-out",
            &undo,
            "--set",
            "x"
        ]),
        "--set is a bay set's number (0, 1 or 2)"
    );
    assert_eq!(
        with(&[
            "--out",
            &out,
            "--changed-out",
            &list,
            "--undo-out",
            &undo,
            "--set",
            "3"
        ]),
        format!("{dump}: bay set 3 does not exist: there are 3")
    );
    assert_eq!(
        with(&[
            "--out",
            &out,
            "--changed-out",
            &list,
            "--undo-out",
            &undo,
            "--size",
            "1"
        ]),
        "--size goes with a new image: --add-to keeps the dump's size"
    );
    // An output that exists, the dump itself included, is refused.
    for (i, existing) in [&dump, &payload].iter().enumerate() {
        let mut names = [out.as_str(), list.as_str(), undo.as_str()];
        names[i] = existing;
        assert_eq!(
            with(&[
                "--out",
                names[0],
                "--changed-out",
                names[1],
                "--undo-out",
                names[2]
            ]),
            format!(
                "{existing} exists: each output is written to a new file only: nothing was written"
            )
        );
    }
    // The options of --add-to without it.
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        &board,
        "--socs",
        &socs,
        "--min-size",
        "--replace-trial",
    ]);
    assert_eq!(
        r.unwrap_err(),
        "--set, --replace-trial, --changed-out and --undo-out go with --add-to, and --of with --undo"
    );
    // --undo: its two other options, a new output, and a file that is no undo file.
    assert_eq!(
        run(&["boot-partition", "--undo", &undo, "--out", &out])
            .0
            .unwrap_err(),
        "--undo needs --of FILE, the dump the files were added to"
    );
    assert_eq!(
        run(&["boot-partition", "--undo", &undo, "--of", &dump])
            .0
            .unwrap_err(),
        "--undo needs --out FILE"
    );
    let not_undo = dir.path("not.undo");
    std::fs::write(&not_undo, b"something else").unwrap();
    assert_eq!(
        run(&[
            "boot-partition",
            "--undo",
            &not_undo,
            "--of",
            &dump,
            "--out",
            &out
        ])
        .0
        .unwrap_err(),
        format!(
            "{dump}: not an undo file of lemnos-ctl boot-partition --add-to: nothing was written"
        )
    );
    assert!(!Path::new(&out).exists());
}

#[test]
fn the_same_assertions_hold_on_a_real_dump_when_one_is_given() {
    let Some(real) = std::env::var_os("LEMNOS_REAL_BOOT_DUMP") else {
        println!("SKIPPED: LEMNOS_REAL_BOOT_DUMP is not set: the real-dump assertions did not run");
        return;
    };
    let dir = Scratch::new("add-to-real-dump");
    // A copy: the real dump is read once and never written.
    let bytes =
        std::fs::read(&real).unwrap_or_else(|e| panic!("{}: {e}", Path::new(&real).display()));
    let dump = dir.path("real.img");
    std::fs::write(&dump, &bytes).unwrap();
    let sv = fatadd::survey(&bytes).unwrap();
    let map = sv.free_map();
    println!(
        "RAN on a real dump: {} bytes, sha256 {}; {} sectors per cluster, {} clusters, {} files; free map: {} clusters free in {} run(s), longest {}",
        bytes.len(),
        sha(&bytes),
        sv.geo.spc,
        sv.geo.clusters,
        sv.entries.iter().filter(|e| !e.is_dir).count(),
        map.clusters,
        map.runs,
        map.longest
    );
    let payload = payloads(&dir, "payload-a", 1);
    let plain = add_and_check(&dir, "plain", &dump, "ours-a", 0, false, &payload);
    for line in plain.text.lines().filter(|l| {
        l.starts_with("mode")
            || l.starts_with("free")
            || l.starts_with("expected")
            || l.starts_with("BAD")
            || l.starts_with("changed")
    }) {
        println!("  without --replace-trial: {line}");
    }
    let replaced = add_and_check(&dir, "replaced", &dump, "ours-a", 0, true, &payload);
    for line in replaced.text.lines().filter(|l| {
        l.starts_with("expected")
            || l.starts_with("BAD")
            || l.starts_with("replaced")
            || l.starts_with("changed")
    }) {
        println!("  with --replace-trial: {line}");
    }
    std::fs::remove_file(&replaced.out).unwrap();
    let payload_b = payloads(&dir, "payload-b", 2);
    let second = add_and_check(&dir, "second", &plain.out, "ours-b", 1, true, &payload_b);
    for line in second.text.lines().filter(|l| {
        l.starts_with("mode")
            || l.starts_with("expected")
            || l.starts_with("BAD")
            || l.starts_with("changed")
    }) {
        println!("  second slot into bay1: {line}");
    }
    println!("RAN on a real dump: every assertion held");
}
