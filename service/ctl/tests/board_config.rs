//! `[boot] config_owner = "board"`: the board's own `config.txt`. The boot partition Lemnos builds for
//! such a board has no `config.txt` at all, a root list that says so, and a `tryboot.txt` for the trial
//! route. Lemnos-owned boards are unchanged (see `firmware_overlays.rs`). Run through the binary's
//! `boot-partition` command, as the image is made in production.

use lemnos_slot_store::bootfat;
use lemnos_slot_store::bootimage::Mem;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn socs() -> String {
    repo().join("socs").display().to_string()
}

fn run(args: &[&str]) -> (Result<(), String>, String) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let mut out = Vec::new();
    let r = lemnos_ctl::run(&args, &mut out);
    (r, String::from_utf8(out).unwrap())
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("board-config-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The reference board with `extra` written in its `[boot]` table, before the others.
fn board_with(dir: &Path, extra: &str) -> String {
    let text = std::fs::read_to_string(repo().join("testing/boards/reference.toml")).unwrap();
    assert!(text.matches("[boot]\n").count() == 1);
    let text = text.replacen("[boot]\n", &format!("[boot]\n{extra}\n"), 1);
    let path = dir.join("board.toml");
    std::fs::write(&path, text).unwrap();
    path.display().to_string()
}

/// Five small payloads, one per role, named as the roles are.
fn payloads(dir: &Path) -> String {
    let p = dir.join("payload");
    std::fs::create_dir_all(&p).unwrap();
    for (name, body) in [
        ("kernel", "kernel\n"),
        ("initramfs", "initramfs\n"),
        ("device-tree", "device tree\n"),
        ("cmdline", "console=tty1 os.slot=os-1 panic=5\n"),
        ("board-overlay", "overlay\n"),
    ] {
        std::fs::write(p.join(name), body).unwrap();
    }
    p.display().to_string()
}

fn build(dir: &Path, board: &str, payload: &str) -> Result<(), String> {
    let (r, min) = run(&[
        "boot-partition",
        "--layout-of",
        board,
        "--socs",
        &socs(),
        "--payload",
        payload,
        "--min-size",
    ]);
    r?;
    let size = min.lines().next().unwrap().trim().to_string();
    let image = dir.join("boot.img");
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        board,
        "--socs",
        &socs(),
        "--slot",
        "os-1",
        "--payload",
        payload,
        "--size",
        &size,
        "--out",
        &image.display().to_string(),
    ]);
    r
}

#[test]
fn a_board_that_owns_config_txt_gets_no_config_txt_and_a_tryboot_txt() {
    let dir = scratch("owned");
    let board = board_with(&dir, "config_owner = \"board\"");
    build(&dir, &board, &payloads(&dir)).unwrap();
    let image = std::fs::read(dir.join("boot.img")).unwrap();
    let mut mem = Mem(image.clone());
    // Lemnos writes no config.txt: the file is not on the partition at all.
    assert!(bootfat::read_file(&mut mem, "config.txt", 65536).is_err());
    // tryboot.txt is there: it is the file the tryboot restart reads.
    assert!(bootfat::read_file(&mut mem, "tryboot.txt", 65536).is_ok());
    // The root list says the board owns config.txt.
    let list = bootfat::read_file(&mut mem, "firmware-overlays.txt", 4096).unwrap();
    assert_eq!(list, b"board-config\n");
    // The image checks clean against its own board file, and a `--verify` names nothing failed.
    let (r, out) = run(&[
        "boot-partition",
        "--verify",
        &dir.join("boot.img").display().to_string(),
        "--layout-of",
        &board,
        "--socs",
        &socs(),
    ]);
    r.unwrap();
    assert!(!out.contains("BAD"), "{out}");
}

#[test]
fn a_board_that_owns_config_txt_refuses_config_lines_by_name() {
    let dir = scratch("lines");
    let board = board_with(
        &dir,
        "config_owner = \"board\"\nconfig_lines = [\"gpio=5=op,dh\"]",
    );
    let err = build(&dir, &board, &payloads(&dir)).unwrap_err();
    assert!(
        err.contains("config_owner = \"board\" and config_lines"),
        "{err}"
    );
}

#[test]
fn an_unknown_config_owner_is_refused_by_name() {
    let dir = scratch("unknown");
    let board = board_with(&dir, "config_owner = \"someone\"");
    let err = build(&dir, &board, &payloads(&dir)).unwrap_err();
    assert!(err.contains("config_owner is \"someone\""), "{err}");
}
