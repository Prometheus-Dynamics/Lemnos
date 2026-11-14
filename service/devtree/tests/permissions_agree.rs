//! The udev rules and the mdev lines are two renderings of one list of facts. Each board file in the
//! repository is rendered both ways, both outputs are parsed back into (node path, owner, group,
//! mode), and the two sets must be equal. A renderer that changes one permission fails here.

use std::collections::BTreeSet;
use std::path::PathBuf;

use lemnos_devtree::{mdev, udev};
use lemnos_host::board::{self, BoardFile};

/// A rule as the test compares it: the SUBSYSTEM it matches, its node path under `/dev` (a glob with
/// `*`), owner, group and mode.
type Node = (String, String, String, String, String);

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The directory under `/dev` a udev SUBSYSTEM's nodes live in. This is the test's own statement
/// of the kernel's naming, not the facts' `dir`, so a fact with the wrong directory fails.
fn dir_of(subsystem: &str) -> &'static str {
    match subsystem {
        "misc" => "",
        "dma_heap" => "dma_heap/",
        other => panic!("no node directory known for SUBSYSTEM {other:?}"),
    }
}

/// `SUBSYSTEM=="s", KERNEL=="k", [OWNER="o", ]GROUP="g", MODE="m"`: owner root when OWNER is absent.
fn parse_udev(line: &str) -> Node {
    let mut subsystem = None;
    let mut kernel = None;
    let mut owner = "root".to_owned();
    let mut group = None;
    let mut mode = None;
    for field in line.split(", ") {
        let (key, value) = field
            .split_once("==")
            .or_else(|| field.split_once('='))
            .unwrap();
        let value = value.trim_matches('"').to_owned();
        match key {
            "SUBSYSTEM" => subsystem = Some(value),
            "KERNEL" => kernel = Some(value),
            "OWNER" => owner = value,
            "GROUP" => group = Some(value),
            "MODE" => mode = Some(value),
            other => panic!("unexpected udev key {other:?} in {line:?}"),
        }
    }
    let subsystem = subsystem.expect(line);
    let node = format!("{}{}", dir_of(&subsystem), kernel.expect(line));
    (
        subsystem,
        node,
        owner,
        group.expect(line),
        mode.expect(line),
    )
}

/// `SUBSYSTEM=^s$;^regex$ owner:group mode`: the subsystem is read from the env match, and the regex
/// back to a node path (`\X` is X and `.*` is `*`).
fn parse_mdev(line: &str) -> Node {
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(fields.len(), 3, "not a mdev line: {line:?}");
    let (env, rule) = fields[0]
        .split_once(';')
        .unwrap_or_else(|| panic!("no SUBSYSTEM env match: {line:?}"));
    let subsystem = env
        .strip_prefix("SUBSYSTEM=^")
        .and_then(|s| s.strip_suffix('$'))
        .unwrap_or_else(|| panic!("not a SUBSYSTEM match: {line:?}"));
    let pattern = rule
        .strip_prefix('^')
        .and_then(|p| p.strip_suffix('$'))
        .unwrap_or_else(|| panic!("not anchored: {line:?}"));
    let mut node = String::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => node.push(chars.next().expect(line)),
            '.' if chars.peek() == Some(&'*') => {
                chars.next();
                node.push('*');
            }
            other => node.push(other),
        }
    }
    let (owner, group) = fields[1].split_once(':').expect(line);
    (
        subsystem.to_owned(),
        node,
        owner.to_owned(),
        group.to_owned(),
        fields[2].to_owned(),
    )
}

fn nodes(lines: impl Iterator<Item = String>, parse: fn(&str) -> Node) -> BTreeSet<Node> {
    lines
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| parse(&line))
        .collect()
}

fn udev_nodes(board: &BoardFile) -> BTreeSet<Node> {
    nodes(udev::fragment(board).lines().map(str::to_owned), parse_udev)
}

fn mdev_nodes(board: &BoardFile) -> BTreeSet<Node> {
    nodes(mdev::fragment(board).lines().map(str::to_owned), parse_mdev)
}

/// Every board file in the repository: the examples and the testing boards.
fn board_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in ["docs/examples", "testing/boards"] {
        for entry in std::fs::read_dir(repo().join(dir)).expect(dir) {
            let path = entry.expect("entry").path();
            if path.extension().is_some_and(|e| e == "toml") {
                files.push(path);
            }
        }
    }
    files.sort();
    assert!(files.len() >= 2, "the board files were not found");
    files
}

#[test]
fn every_board_file_gives_the_same_nodes_and_modes_in_both_renderings() {
    let mut compared = Vec::new();
    let mut skipped = Vec::new();
    for path in board_files() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).expect("board file");
        let board = match board::parse(&text) {
            Ok(board) => board,
            Err(error) => {
                // A board file must parse. An example that is not one (a fixture for another test
                // with its own format) is named, so the set compared is visible.
                assert!(
                    path.starts_with(repo().join("docs/examples")),
                    "{name}: a testing board does not parse: {error}"
                );
                skipped.push(name);
                continue;
            }
        };
        let from_udev = udev_nodes(&board);
        let from_mdev = mdev_nodes(&board);
        assert!(
            from_udev.iter().any(|(_, node, ..)| node == "lemnos"),
            "{name}: the module's node is missing from the udev rules"
        );
        assert_eq!(from_udev, from_mdev, "{name}: the two renderings differ");
        assert!(
            from_mdev
                .iter()
                .all(|(subsystem, ..)| !subsystem.is_empty()),
            "{name}: a rule without its SUBSYSTEM"
        );
        compared.push(name);
    }
    assert!(compared.len() >= 8, "only {compared:?} were compared");
}

/// The board's record decides the mailbox node: a `tryboot` board (the default too) gives the daemon's
/// group `/dev/vcio` in both renderings; a board with another record does not.
#[test]
fn the_mailbox_node_is_in_both_renderings_only_for_a_tryboot_record() {
    let text = std::fs::read_to_string(repo().join("testing/boards/reference.toml")).unwrap();
    assert!(text.contains("record = \"tryboot\""));
    let vcio: Node = (
        "misc".into(),
        "vcio".into(),
        "root".into(),
        "lemnos".into(),
        "0660".into(),
    );
    let tryboot = board::parse(&text).unwrap();
    assert!(udev_nodes(&tryboot).contains(&vcio), "udev lacks vcio");
    assert!(mdev_nodes(&tryboot).contains(&vcio), "mdev lacks vcio");
    let other = board::parse(&text.replace("record = \"tryboot\"", "record = \"file\"")).unwrap();
    assert!(!udev_nodes(&other).contains(&vcio));
    assert!(!mdev_nodes(&other).contains(&vcio));
    let defaulted = board::parse(&text.replace("record = \"tryboot\", ", "")).unwrap();
    assert!(
        udev_nodes(&defaulted).contains(&vcio),
        "the default record is tryboot"
    );
}
