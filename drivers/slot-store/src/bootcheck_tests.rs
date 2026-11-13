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
