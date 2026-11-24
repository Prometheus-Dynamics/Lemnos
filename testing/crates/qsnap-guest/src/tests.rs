use super::*;

/// A `newc` entry, as `cpio -H newc -o` writes it.
fn entry(out: &mut Vec<u8>, name: &str, mode: u32, body: &[u8]) {
    let name_len = name.len() + 1;
    let fields = [
        0,
        mode,
        0,
        0,
        1,
        0,
        u32::try_from(body.len()).unwrap(),
        0,
        0,
        0,
        0,
        u32::try_from(name_len).unwrap(),
        0,
    ];
    out.extend_from_slice(b"070701");
    for f in fields {
        out.extend_from_slice(format!("{f:08x}").as_bytes());
    }
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    out.extend_from_slice(body);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("lemnos-qsnap-guest-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn an_archive_unpacks_with_its_directories_files_links_and_modes() {
    let mut archive = Vec::new();
    entry(&mut archive, ".", 0o040_755, b"");
    entry(&mut archive, "bin", 0o040_755, b"");
    entry(
        &mut archive,
        "bin/check",
        0o100_755,
        b"#!/bin/sh\necho hi\n",
    );
    entry(&mut archive, "./data.txt", 0o100_644, b"abc");
    entry(&mut archive, "bin/link", 0o120_777, b"check");
    entry(&mut archive, "TRAILER!!!", 0, b"");
    archive.extend_from_slice(&[0u8; 512]); // the rest of the disk
    let to = scratch("unpack");
    assert_eq!(unpack(&archive[..], &to).unwrap(), 2);
    assert_eq!(
        fs::read(to.join("bin/check")).unwrap(),
        b"#!/bin/sh\necho hi\n"
    );
    assert_eq!(
        fs::metadata(to.join("bin/check"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(fs::read(to.join("data.txt")).unwrap(), b"abc");
    assert_eq!(
        fs::read_link(to.join("bin/link")).unwrap(),
        Path::new("check")
    );
    fs::remove_dir_all(&to).unwrap();
}

#[test]
fn a_name_that_leaves_the_directory_is_refused_and_nothing_outside_is_written() {
    let mut archive = Vec::new();
    entry(&mut archive, "../escaped", 0o100_644, b"x");
    entry(&mut archive, "TRAILER!!!", 0, b"");
    let to = scratch("escape");
    let err = unpack(&archive[..], &to).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(!to.parent().unwrap().join("escaped").exists());
    fs::remove_dir_all(&to).unwrap();
}

#[test]
fn a_disk_that_is_not_an_archive_is_refused() {
    let to = scratch("zeros");
    assert_eq!(
        unpack(&[0u8; 512][..], &to).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let short = b"070701";
    assert_eq!(
        unpack(&short[..], &to).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    fs::remove_dir_all(&to).unwrap();
}
