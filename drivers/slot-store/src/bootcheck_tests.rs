use super::*;
use crate::bootfat::tests::{Item, Kind, tree};
use crate::tests::TempDir;
use std::fs;

const ID: &str = "os-2";

/// The slot's boot files as a bundle has them: `(name, bytes)`.
fn bundle() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("kernel8.img", b"kernel of os-2".to_vec()),
        ("bcm2712-rpi-5-b.dtb", b"dtb of os-2".to_vec()),
        ("overlays/vc4-kms.dtbo", b"overlay of os-2".to_vec()),
        ("initramfs", b"initramfs of os-2".to_vec()),
        (
            "cmdline.txt",
            b"console=tty1 rootwait os.slot=os-2 os.roothash=ab panic=5\n".to_vec(),
        ),
    ]
}

/// The manifest's `boot.files`: each path with its hash.
fn listed(files: &[(&str, Vec<u8>)]) -> Vec<(String, String)> {
    files
        .iter()
        .map(|(n, b)| ((*n).to_string(), sha256_hex(b)))
        .collect()
}

/// Writes the files under `dir/slots/<id>/`.
fn install(dir: &std::path::Path, files: &[(&str, Vec<u8>)]) {
    for (name, bytes) in files {
        let path = dir.join(slot_dir(ID)).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
}

/// Reads a relative path under `dir` as real files, `None` when absent.
fn from_dir(dir: &std::path::Path) -> impl FnMut(&str) -> Result<Option<Vec<u8>>, String> + '_ {
    move |path| match fs::read(dir.join(path)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[test]
fn sha256_is_the_standard_digest() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn a_slot_whose_files_are_as_the_manifest_says_matches() {
    let dir = TempDir::new("bootcheck-match");
    let files = bundle();
    install(&dir.0, &files);
    assert!(check(ID, &listed(&files), from_dir(&dir.0)).is_empty());
}

#[test]
fn a_command_line_changed_by_one_argument_does_not_match() {
    // The in-place edit case: the default's cmdline.txt gets one argument appended in place.
    let dir = TempDir::new("bootcheck-cmdline");
    let files = bundle();
    install(&dir.0, &files);
    let path = dir.0.join(slot_dir(ID)).join("cmdline.txt");
    let mut text = fs::read_to_string(&path).unwrap();
    text = text.replace("panic=5", "panic=5 mem=128M");
    fs::write(&path, text).unwrap();
    assert_eq!(
        check(ID, &listed(&files), from_dir(&dir.0)),
        vec![Mismatch::Changed("cmdline.txt".into())]
    );
}

#[test]
fn every_kind_of_boot_file_is_checked_and_a_change_in_any_is_found() {
    let files = bundle();
    for (name, _) in &files {
        let dir = TempDir::new("bootcheck-kinds");
        install(&dir.0, &files);
        let path = dir.0.join(slot_dir(ID)).join(name);
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] ^= 0x01;
        fs::write(&path, bytes).unwrap();
        assert_eq!(
            check(ID, &listed(&files), from_dir(&dir.0)),
            vec![Mismatch::Changed((*name).to_string())],
            "{name}"
        );
    }
}

#[test]
fn a_missing_boot_file_is_missing_and_a_failing_read_is_unreadable() {
    let dir = TempDir::new("bootcheck-missing");
    let files = bundle();
    install(&dir.0, &files);
    fs::remove_file(dir.0.join(slot_dir(ID)).join("initramfs")).unwrap();
    assert_eq!(
        check(ID, &listed(&files), from_dir(&dir.0)),
        vec![Mismatch::Missing("initramfs".into())]
    );
    let refuse = |path: &str| -> Result<Option<Vec<u8>>, String> {
        if path.ends_with("kernel8.img") {
            Err("the chain loops".into())
        } else {
            Ok(None)
        }
    };
    let got = check(ID, &listed(&files), refuse);
    assert_eq!(
        got[0],
        Mismatch::Unreadable("kernel8.img".into(), "the chain loops".into())
    );
}

#[test]
fn the_fat_reader_gives_the_same_answer_as_the_files_on_a_fat_partition() {
    // The check through the raw FAT reader (no mount): the same files, in a FAT image.
    let files = bundle();
    let dtb = files[1].1.clone();
    let image_tree = vec![Item {
        long: "slots",
        short: b"SLOTS      ",
        lfn: false,
        kind: Kind::Dir(vec![Item {
            long: ID,
            short: b"OS-2       ",
            lfn: false,
            kind: Kind::Dir(vec![
                Item {
                    long: "kernel8.img",
                    short: b"KERNEL8 IMG",
                    lfn: true,
                    kind: Kind::File(files[0].1.clone()),
                },
                Item {
                    long: "bcm2712-rpi-5-b.dtb",
                    short: b"BCM2712~DTB",
                    lfn: true,
                    kind: Kind::File(dtb),
                },
                Item {
                    long: "cmdline.txt",
                    short: b"CMDLINE TXT",
                    lfn: true,
                    kind: Kind::File(files[4].1.clone()),
                },
            ]),
        }]),
    }];
    let mut vol = tree(&image_tree, false);
    let mut want = listed(&files);
    // The image has only three of the files; the others are reported missing, by name.
    let got = check(ID, &want, |p| fat_read(&mut vol, p));
    assert_eq!(
        got,
        vec![
            Mismatch::Missing("overlays/vc4-kms.dtbo".into()),
            Mismatch::Missing("initramfs".into()),
        ]
    );
    want.retain(|(n, _)| n == "kernel8.img" || n == "cmdline.txt");
    assert!(check(ID, &want, |p| fat_read(&mut vol, p)).is_empty());
}

/// The text `system-slots`' writer makes for a slot, for `id`: no fdt line (the script loads board.dtb).
fn generated(id: &str, append: &str) -> String {
    format!(
        "# Written by the board's slot driver for slot {id}. Rewritten at each install.\n\
         default {id}\n\
         timeout 0\n\
         label {id}\n\
         \x20 kernel /slots/{id}/Image\n\
         \x20 initrd /slots/{id}/initramfs.cpio.gz\n\
         \x20 append {append}\n"
    )
}

fn slot_files() -> Vec<(String, String)> {
    ["Image", "board.dtb", "initramfs.cpio.gz", "cmdline.txt"]
        .iter()
        .map(|n| (n.to_string(), "00".to_string()))
        .collect()
}

#[test]
fn the_generated_extlinux_matches_when_its_append_is_the_hashed_command_line() {
    let line = "console=tty1 root=/dev/mapper/slot lemnos.slot=s1 lemnos.roothash=ab12";
    let conf = generated("s1", line);
    let cmdline = format!("{line}\n");
    assert_eq!(
        extlinux_mismatch("s1", &slot_files(), &conf, cmdline.as_bytes()),
        Ok(())
    );
    // The installer trims the line break before it writes the entry; the file may end in CRLF.
    let crlf = format!("{line}\r\n");
    assert_eq!(
        extlinux_mismatch("s1", &slot_files(), &conf, crlf.as_bytes()),
        Ok(())
    );
}

#[test]
fn a_command_line_changed_by_one_argument_does_not_match_the_extlinux_append() {
    // The one-argument edit on the U-Boot form: `mem=128M` appended to the hashed line, in the file U-Boot reads.
    let line = "console=tty1 lemnos.slot=s1 lemnos.roothash=ab12";
    let conf = generated("s1", &format!("{line} mem=128M"));
    let cmdline = format!("{line}\n");
    let why = extlinux_mismatch("s1", &slot_files(), &conf, cmdline.as_bytes()).unwrap_err();
    assert!(why.contains("append"), "{why}");
    // And the other way: the hashed line changed, the entry did not.
    let conf = generated("s1", line);
    let hashed = format!("{line} mem=128M\n");
    assert!(extlinux_mismatch("s1", &slot_files(), &conf, hashed.as_bytes()).is_err());
}

#[test]
fn an_entry_that_names_another_slot_or_an_unlisted_file_does_not_match() {
    let line = "console=tty1";
    let cmdline = format!("{line}\n");
    let other_slot = generated("s1", line).replace("/slots/s1/Image", "/slots/s2/Image");
    assert!(extlinux_mismatch("s1", &slot_files(), &other_slot, cmdline.as_bytes()).is_err());
    let unlisted = generated("s1", line).replace("initramfs.cpio.gz", "evil.cpio.gz");
    let why = extlinux_mismatch("s1", &slot_files(), &unlisted, cmdline.as_bytes()).unwrap_err();
    assert!(why.contains("initrd") || why.contains("evil"), "{why}");
    // A label with an fdt line is refused by name, whichever file it names.
    let fdt = generated("s1", line).replace("  initrd", "  fdt /slots/s1/board.dtb\n  initrd");
    let why = extlinux_mismatch("s1", &slot_files(), &fdt, cmdline.as_bytes()).unwrap_err();
    assert!(why.contains("has an fdt line"), "{why}");
    // A slot with no board.dtb is refused by name too.
    let no_dtb: Vec<(String, String)> = slot_files()
        .into_iter()
        .filter(|(n, _)| n != "board.dtb")
        .collect();
    let why =
        extlinux_mismatch("s1", &no_dtb, &generated("s1", line), cmdline.as_bytes()).unwrap_err();
    assert!(why.contains("no board.dtb"), "{why}");
}

#[test]
fn a_missing_duplicate_or_unknown_entry_does_not_match() {
    let line = "console=tty1";
    let cmdline = format!("{line}\n");
    let base = generated("s1", line);
    let without = |key: &str| {
        base.lines()
            .filter(|l| !l.trim_start().starts_with(key))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };
    for key in ["kernel", "initrd", "append"] {
        assert!(
            extlinux_mismatch("s1", &slot_files(), &without(key), cmdline.as_bytes()).is_err(),
            "without {key} matched"
        );
    }
    let twice = format!("{base}  kernel /slots/s1/Image\n");
    assert!(extlinux_mismatch("s1", &slot_files(), &twice, cmdline.as_bytes()).is_err());
    let unknown = format!("{base}  fdtoverlays /slots/s1/evil.dtbo\n");
    let why = extlinux_mismatch("s1", &slot_files(), &unknown, cmdline.as_bytes()).unwrap_err();
    assert!(why.contains("fdtoverlays"), "{why}");
    assert!(extlinux_mismatch("s1", &slot_files(), "", cmdline.as_bytes()).is_err());
}

#[test]
fn an_fdtdir_line_is_refused_by_name() {
    let line = "console=tty1";
    let cmdline = format!("{line}\n");
    let dir = generated("s1", line).replace("  initrd", "  fdtdir /slots/s1/dtbs/\n  initrd");
    let why = extlinux_mismatch("s1", &slot_files(), &dir, cmdline.as_bytes()).unwrap_err();
    assert!(why.contains("fdtdir"), "{why}");
}

#[test]
fn the_uboot_check_reads_the_slot_its_cmdline_and_the_root_copy_when_default() {
    use std::collections::HashMap;
    let line = "console=tty1 lemnos.slot=s1";
    let conf = generated("s1", line);
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    files.insert(
        "slots/s1/extlinux/extlinux.conf".into(),
        conf.clone().into_bytes(),
    );
    files.insert(
        "slots/s1/cmdline.txt".into(),
        format!("{line}\n").into_bytes(),
    );
    files.insert("extlinux/extlinux.conf".into(), conf.clone().into_bytes());
    let read = |files: &HashMap<String, Vec<u8>>| {
        let files = files.clone();
        move |path: &str| Ok(files.get(path).cloned())
    };
    assert_eq!(uboot_check("s1", &slot_files(), read(&files)), None);
    // A changed append in the slot's copy is refused.
    let mut edited = files.clone();
    edited.insert(
        "slots/s1/extlinux/extlinux.conf".into(),
        generated("s1", &format!("{line} mem=128M")).into_bytes(),
    );
    assert!(uboot_check("s1", &slot_files(), read(&edited)).is_some());
    // The root copy is the install-time fallback: one that is not the default's is not a mismatch.
    let mut root_other = files.clone();
    root_other.insert("extlinux/extlinux.conf".into(), b"other\n".to_vec());
    assert_eq!(uboot_check("s1", &slot_files(), read(&root_other)), None);
    assert_eq!(uboot_check("s1", &slot_files(), read(&root_other)), None);
    // A board with no extlinux anywhere is not the U-Boot form: nothing to check.
    assert_eq!(uboot_check("s1", &slot_files(), |_| Ok(None)), None);
    // A U-Boot board whose slot copy is gone does not match.
    let mut gone = files.clone();
    gone.remove("slots/s1/extlinux/extlinux.conf");
    gone.insert("boot-record.env".into(), b"x".to_vec());
    assert!(uboot_check("s1", &slot_files(), read(&gone)).is_some());
}

mod overlay_marker {
    use super::*;
    use crate::manifest::Overlay;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const OLD: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    fn want() -> Overlay {
        Overlay {
            board: "board-a".into(),
            sha256: SHA.into(),
        }
    }

    #[test]
    fn the_slots_own_marker_matches() {
        assert_eq!(overlay_mismatch(&want(), Some(("board-a", SHA))), None);
    }

    #[test]
    fn an_absent_marker_is_named() {
        let why = overlay_mismatch(&want(), None).unwrap();
        assert!(why.contains("no board overlay marker"), "{why}");
    }

    #[test]
    fn a_stale_digest_is_named() {
        let why = overlay_mismatch(&want(), Some(("board-a", OLD))).unwrap();
        assert!(why.contains("digest") && why.contains(OLD), "{why}");
    }

    #[test]
    fn another_boards_overlay_is_named() {
        let why = overlay_mismatch(&want(), Some(("board-b", SHA))).unwrap();
        assert!(why.contains("board-b"), "{why}");
    }
}
