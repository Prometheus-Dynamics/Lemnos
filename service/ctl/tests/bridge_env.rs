//! `lemnos-ctl tree BOARD --bridge-env FILE --bridge-dropin FILE` writes the bridge's settings and the
//! unit drop-in that grants its directories, from one board file, and only those files. The shipped
//! drop-in is the reference board's, so it can't drift from what the generator writes.

#[path = "support/scratch.rs"]
mod scratch;
use scratch::Scratch;
use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn tree_writes_the_reference_boards_bridge_env_to_the_named_file() {
    let dir =
        Scratch(std::env::temp_dir().join(format!("lemnos-ctl-bridge-env-{}", std::process::id())));
    std::fs::create_dir_all(&*dir).expect("scratch directory");
    let socs = repo().join("socs");
    let board = repo().join("testing/boards/reference.toml");
    let env = dir.join("lemnos-orion.env.generated");
    let overlay = dir.join("reference.dts");
    let dropin = dir.join("files.conf");
    let args: Vec<String> = [
        "tree",
        "--socs",
        socs.to_str().unwrap(),
        board.to_str().unwrap(),
        "-o",
        overlay.to_str().unwrap(),
        "--bridge-env",
        env.to_str().unwrap(),
        "--bridge-dropin",
        dropin.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut out = Vec::new();
    lemnos_ctl::run(&args, &mut out).expect("tree --bridge-env");
    let written = std::fs::read_to_string(&env).expect("the env file");
    let golden =
        std::fs::read_to_string(repo().join("service/ctl/tests/golden/reference-bridge.env"))
            .expect("the golden");
    assert_eq!(written, golden);
    let written = std::fs::read_to_string(&dropin).expect("the drop-in");
    let golden = std::fs::read_to_string(
        repo().join("service/ctl/tests/golden/reference-bridge-dropin.conf"),
    )
    .expect("the golden");
    assert_eq!(written, golden);
    let shipped =
        std::fs::read_to_string(repo().join("packaging/systemd/lemnos-orion.service.d/files.conf"))
            .expect("the shipped drop-in");
    assert_eq!(
        shipped, written,
        "the shipped drop-in is the reference board's, as the generator writes it"
    );
    assert!(overlay.exists());
    let _ = std::fs::remove_dir_all(&*dir);
}
