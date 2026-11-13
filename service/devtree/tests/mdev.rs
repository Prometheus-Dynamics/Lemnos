//! The mdev lines of a board: the reference board's golden, and how a fact's name becomes a regex.

use lemnos_devtree::mdev::{fragment, regex};
use lemnos_devtree::permissions::{DAEMON_GROUP, Fact, Name};
use lemnos_host::board::{self, BoardFile};

const REFERENCE_BOARD: &str = include_str!("../../../testing/boards/reference.toml");

fn golden(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn fact(subsystem: &'static str, dir: &'static str, name: Name) -> Fact {
    Fact {
        subsystem,
        dir,
        name,
        owner: "root",
        group: DAEMON_GROUP,
        mode: "0660",
    }
}

fn parse(text: &str) -> BoardFile {
    board::parse(text).unwrap_or_else(|e| panic!("{e}"))
}

fn host(keys: &str) -> BoardFile {
    parse(&format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n[host]\n{keys}"
    ))
}

#[test]
fn the_reference_board_gives_its_mdev_lines() {
    let text = fragment(&parse(REFERENCE_BOARD));
    assert_eq!(text, golden("reference-mdev.conf"));
    assert_eq!(DAEMON_GROUP, "lemnos");
}

#[test]
fn a_heap_name_is_escaped_so_that_its_dot_is_a_dot() {
    let text = fragment(&host("dma_heaps = [\"a.b\"]\n"));
    assert!(
        text.contains("\n^dma_heap/a\\.b$ root:lemnos 0660\n"),
        "{text}"
    );
    assert!(!text.contains("a.b$"), "{text}");
}

#[test]
fn a_glob_becomes_a_star_of_any_characters_and_nothing_else_is_special() {
    let glob = fact("misc", "", Name::Glob("lemnos-*".to_owned()));
    assert_eq!(regex(&glob), "^lemnos-.*$");
    let exact = fact("misc", "", Name::Exact("a+b(c)[d]".to_owned()));
    assert_eq!(regex(&exact), "^a\\+b\\(c\\)\\[d\\]$");
}

#[test]
fn a_board_with_no_heaps_has_the_module_lines_and_no_heap_line() {
    let text = fragment(&host("dma_heaps = []\n"));
    assert!(text.contains("\n^lemnos$ root:lemnos 0660\n"), "{text}");
    assert!(text.contains("\n^lemnos-.*$ root:lemnos 0660\n"), "{text}");
    assert!(!text.contains("dma_heap"), "{text}");
}
