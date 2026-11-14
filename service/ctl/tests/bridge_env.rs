//! `lemnos-ctl tree BOARD --bridge-env FILE` writes the bridge's link line, and only that file.

use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn tree_writes_the_reference_boards_bridge_env_to_the_named_file() {
    let dir = std::env::temp_dir().join(format!("lemnos-ctl-bridge-env-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    let socs = repo().join("socs");
    let board = repo().join("testing/boards/reference.toml");
    let env = dir.join("lemnos-orion.env.generated");
    let overlay = dir.join("reference.dts");
    let args: Vec<String> = [
        "tree",
        "--socs",
        socs.to_str().unwrap(),
        board.to_str().unwrap(),
        "-o",
        overlay.to_str().unwrap(),
        "--bridge-env",
        env.to_str().unwrap(),
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
    assert!(overlay.exists());
    let _ = std::fs::remove_dir_all(&dir);
}
