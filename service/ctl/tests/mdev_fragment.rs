//! `lemnos-ctl tree BOARD --mdev FILE` writes the mdev lines of the board, and only that file.

use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lemnos-ctl-mdev-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

#[test]
fn tree_writes_the_reference_boards_mdev_lines_to_the_named_file() {
    let dir = scratch("reference");
    let socs = repo().join("socs");
    let board = repo().join("testing/boards/reference.toml");
    let mdev = dir.join("mdev.conf.lemnos");
    let overlay = dir.join("reference.dts");
    let args: Vec<String> = [
        "tree",
        "--socs",
        socs.to_str().unwrap(),
        board.to_str().unwrap(),
        "-o",
        overlay.to_str().unwrap(),
        "--mdev",
        mdev.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut out = Vec::new();
    lemnos_ctl::run(&args, &mut out).expect("tree --mdev");
    let written = std::fs::read_to_string(&mdev).expect("the mdev file");
    let golden =
        std::fs::read_to_string(repo().join("service/devtree/tests/golden/reference-mdev.conf"))
            .expect("the golden");
    assert_eq!(written, golden);
    assert!(overlay.exists());
    assert!(!dir.join("71-lemnos-board.rules").exists());
}
