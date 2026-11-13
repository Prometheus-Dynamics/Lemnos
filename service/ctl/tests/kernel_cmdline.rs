//! The module's parameters as modprobe options and as kernel command-line words carry the same
//! values, for every board file the repository ships. A module built into the kernel gets the
//! second form; a module loaded by modprobe gets the first.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// The kernel's command line is at most this long, in bytes (COMMAND_LINE_SIZE on the targets).
const COMMAND_LINE_MAX: usize = 2048;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The `name=value` pairs of the `options lemnos` line of a modprobe fragment.
fn modprobe_pairs(text: &str) -> BTreeSet<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("options lemnos "))
        .map(|rest| rest.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

/// The command-line words, with their `lemnos.` prefix taken off.
fn cmdline_pairs(text: &str) -> BTreeSet<String> {
    text.split_whitespace()
        .map(|word| {
            word.strip_prefix("lemnos.")
                .unwrap_or_else(|| panic!("command-line word without lemnos. prefix: {word}"))
                .to_string()
        })
        .collect()
}

#[test]
fn the_command_line_carries_the_modprobe_values_for_every_board() {
    let dir = std::env::temp_dir().join(format!("lemnos-ctl-cmdline-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    let socs = repo().join("socs");
    let mut boards = 0;
    let mut with_options = 0;
    for entry in std::fs::read_dir(repo().join("testing/boards")).expect("boards") {
        let board = entry.expect("entry").path();
        if board.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        // The QEMU guest's board names no SoC, and the generator has nothing to give it.
        let text = std::fs::read_to_string(&board).expect("board file");
        if !text
            .lines()
            .any(|line| line.trim_start().starts_with("soc"))
        {
            continue;
        }
        boards += 1;
        let name = board.file_stem().unwrap().to_string_lossy().into_owned();
        let modprobe = dir.join(format!("{name}.conf"));
        let cmdline = dir.join(format!("{name}.cmdline"));
        let overlay = dir.join(format!("{name}.dts"));
        let args: Vec<String> = [
            "tree",
            "--socs",
            socs.to_str().unwrap(),
            board.to_str().unwrap(),
            "-o",
            overlay.to_str().unwrap(),
            "--modprobe",
            modprobe.to_str().unwrap(),
            "--cmdline",
            cmdline.to_str().unwrap(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut out = Vec::new();
        lemnos_ctl::run(&args, &mut out).unwrap_or_else(|e| panic!("{name}: {e}"));

        let from_modprobe = modprobe_pairs(&std::fs::read_to_string(&modprobe).unwrap());
        let line = std::fs::read_to_string(&cmdline).unwrap();
        assert!(
            line.len() <= COMMAND_LINE_MAX,
            "{name}: command line is {} bytes",
            line.len()
        );
        let from_cmdline = cmdline_pairs(&line);
        assert_eq!(
            from_cmdline, from_modprobe,
            "{name}: command line and modprobe differ"
        );
        if !from_modprobe.is_empty() {
            with_options += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(boards > 0, "no board files found");
    assert!(
        with_options > 0,
        "no board gives the module a node: the test checks nothing"
    );
}
