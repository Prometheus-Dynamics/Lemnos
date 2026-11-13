//! The udev fragment: the reference board's golden, and what the board's `[host]` section and its
//! dma-buf streams do to the dma-heap rules.

use lemnos_devtree::udev::{DAEMON_GROUP, fragment};
use lemnos_host::board::{self, BoardFile};

const REFERENCE_BOARD: &str = include_str!("../../../testing/boards/reference.toml");

fn golden(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn parse(text: &str) -> BoardFile {
    board::parse(text).unwrap_or_else(|e| panic!("{e}"))
}

/// A board with a `[host]` section of `keys` and no devices.
fn host(keys: &str) -> BoardFile {
    parse(&format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n[host]\n{keys}"
    ))
}

#[test]
fn the_reference_board_gives_its_module_nodes_and_its_cma_heap_to_the_daemon_group() {
    let text = fragment(&parse(REFERENCE_BOARD));
    assert_eq!(text, golden("reference-udev.rules"));
    assert_eq!(DAEMON_GROUP, "lemnos");
}

#[test]
fn the_heap_rules_follow_the_board_and_an_empty_list_names_none() {
    let text = fragment(&host("dma_heaps = [\"system\", \"linux,cma\"]\n"));
    assert!(text.contains(
        "SUBSYSTEM==\"dma_heap\", KERNEL==\"linux,cma\", GROUP=\"lemnos\", MODE=\"0660\"\n"
    ));
    assert!(text.contains(
        "SUBSYSTEM==\"dma_heap\", KERNEL==\"system\", GROUP=\"lemnos\", MODE=\"0660\"\n"
    ));
    let text = fragment(&host("dma_heaps = []\n"));
    assert!(!text.contains("dma_heap"), "{text}");
    // The module's own nodes are there on every board.
    assert!(
        text.contains("SUBSYSTEM==\"misc\", KERNEL==\"lemnos\", GROUP=\"lemnos\", MODE=\"0660\"\n")
    );
    assert!(
        text.contains(
            "SUBSYSTEM==\"misc\", KERNEL==\"lemnos-*\", GROUP=\"lemnos\", MODE=\"0660\"\n"
        )
    );
}

#[test]
fn a_board_with_no_host_section_gets_the_cma_heap_the_daemon_opens() {
    let text = fragment(&parse(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n",
    ));
    assert!(text.contains("KERNEL==\"linux,cma\""), "{text}");
}
