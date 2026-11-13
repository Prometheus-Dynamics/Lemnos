//! `lemnos-ctl tree BOARD --udev FILE` writes the udev rules of the board, and only that file.

use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lemnos-ctl-udev-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

#[test]
fn tree_writes_the_reference_boards_udev_rules_to_the_named_file() {
    let dir = scratch("reference");
    let socs = repo().join("socs");
    let board = repo().join("testing/boards/reference.toml");
    let udev = dir.join("71-lemnos-board.rules");
    let overlay = dir.join("reference.dts");
    let args: Vec<String> = [
        "tree",
        "--socs",
        socs.to_str().unwrap(),
        board.to_str().unwrap(),
        "-o",
        overlay.to_str().unwrap(),
        "--udev",
        udev.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut out = Vec::new();
    lemnos_ctl::run(&args, &mut out).expect("tree --udev");
    let written = std::fs::read_to_string(&udev).expect("the udev file");
    let golden =
        std::fs::read_to_string(repo().join("service/devtree/tests/golden/reference-udev.rules"))
            .expect("the golden");
    assert_eq!(written, golden);
    // The overlay was written as before: the flag adds a file, it changes no other output.
    assert!(overlay.exists());
}

#[test]
fn every_board_with_a_soc_gives_the_module_nodes_a_rule() {
    let dir = scratch("every");
    let socs = repo().join("socs");
    let mut boards = 0;
    for entry in std::fs::read_dir(repo().join("testing/boards")).expect("boards") {
        let board = entry.expect("entry").path();
        if board.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let text = std::fs::read_to_string(&board).expect("board file");
        if !text
            .lines()
            .any(|line| line.trim_start().starts_with("soc"))
        {
            continue;
        }
        boards += 1;
        let name = board.file_stem().unwrap().to_string_lossy().into_owned();
        let udev = dir.join(format!("{name}.rules"));
        let args: Vec<String> = [
            "tree",
            "--socs",
            socs.to_str().unwrap(),
            board.to_str().unwrap(),
            "-o",
            dir.join(format!("{name}.dts")).to_str().unwrap(),
            "--udev",
            udev.to_str().unwrap(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut out = Vec::new();
        lemnos_ctl::run(&args, &mut out).unwrap_or_else(|e| panic!("{name}: {e}"));
        let rules = std::fs::read_to_string(&udev).unwrap();
        assert!(
            rules.contains(
                "SUBSYSTEM==\"misc\", KERNEL==\"lemnos\", GROUP=\"lemnos\", MODE=\"0660\"\n"
            ),
            "{name}: no rule for /dev/lemnos"
        );
    }
    assert!(boards > 0);
}
