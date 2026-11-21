//! `lemnos-ctl tree BOARD --security-unit FILE` writes the unit that renders the security view and
//! makes the places the daemon writes later: /run/board and the record's state directory, both owned
//! by the daemon's user, so lemnosd chowns nothing.

#[path = "support/scratch.rs"]
mod scratch;
use scratch::Scratch;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> Scratch {
    let dir = Scratch(
        std::env::temp_dir().join(format!("lemnos-ctl-secunit-{name}-{}", std::process::id())),
    );
    std::fs::create_dir_all(&*dir).expect("scratch directory");
    dir
}

fn tree(board: &Path, unit: &Path, overlay: &Path) -> Result<(), String> {
    let socs = repo().join("socs");
    let args: Vec<String> = [
        "tree",
        "--socs",
        socs.to_str().unwrap(),
        board.to_str().unwrap(),
        "-o",
        overlay.to_str().unwrap(),
        "--security-unit",
        unit.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut out = Vec::new();
    lemnos_ctl::run(&args, &mut out).map(|_| ())
}

#[test]
fn the_reference_boards_security_unit_is_the_golden() {
    let dir = scratch("reference");
    let unit = dir.join("lemnos-board-security.service");
    tree(
        &repo().join("testing/boards/reference.toml"),
        &unit,
        &dir.join("reference.dts"),
    )
    .expect("tree --security-unit");
    let written = std::fs::read_to_string(&unit).expect("the unit");
    let golden =
        std::fs::read_to_string(repo().join("service/ctl/tests/golden/reference-security.service"))
            .expect("the golden");
    assert_eq!(written, golden);
    assert!(written.contains("User=lemnos\nGroup=lemnos\n"));
    assert!(written.contains("RuntimeDirectoryMode=0775\n"));
    assert!(written.contains("StateDirectory=lemnos\nStateDirectoryMode=0750\n"));
}

/// A record the daemon's drop-in doesn't grant is refused by name, not written into a unit that
/// would leave the daemon unable to write it.
#[test]
fn a_record_outside_var_lib_lemnos_is_refused() {
    let dir = scratch("outside");
    let text = std::fs::read_to_string(repo().join("testing/boards/reference.toml"))
        .expect("reference")
        .replace(
            "record = \"/var/lib/lemnos/security.rec\"",
            "record = \"/etc/lemnos/security.rec\"",
        );
    let board = dir.join("board.toml");
    std::fs::write(&board, text).expect("board");
    let unit = dir.join("unit.service");
    let error = tree(&board, &unit, &dir.join("o.dts")).expect_err("refused");
    assert!(
        error.contains("is not under /var/lib/lemnos"),
        "error: {error}"
    );
    assert!(!unit.exists());
}
