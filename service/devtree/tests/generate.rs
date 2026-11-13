//! The generator's output for fixture boards, every resource kind, and the refusals; and the start
//! check against fixture live trees.

use std::fs;
use std::path::{Path, PathBuf};

use lemnos_devtree::{SocMap, check, generate};
use lemnos_host::board;

const REFERENCE_MAP: &str = include_str!("../../../socs/bcm2712-rp1.toml");
const REFERENCE_BOARD: &str = include_str!("../../../testing/boards/reference.toml");
const ALL_KINDS_MAP: &str = include_str!("fixtures/all-kinds.toml");
const ALL_KINDS_BOARD: &str = include_str!("fixtures/all-kinds-board.toml");

fn generated(board_text: &str, map_text: &str) -> Result<lemnos_devtree::Output, String> {
    let board = board::parse(board_text).map_err(|e| e.to_string())?;
    let map = SocMap::parse(map_text)?;
    generate(&board, &map)
}

fn golden(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn the_reference_board_makes_its_overlay_and_boot_fragment() {
    let out = generated(REFERENCE_BOARD, REFERENCE_MAP).unwrap();
    assert_eq!(out.overlay, golden("reference.dts"));
    assert_eq!(out.boot, golden("reference-boot.txt"));
}

#[test]
fn every_resource_kind_is_generated() {
    let out = generated(ALL_KINDS_BOARD, ALL_KINDS_MAP).unwrap();
    assert_eq!(out.overlay, golden("all-kinds.dts"));
    assert_eq!(out.boot, golden("all-kinds-boot.txt"));
    // The kinds the board names are each an enabled node with a reason beside it.
    for target in [
        "/bus/i2c@1000",
        "/bus/spi@2000",
        "/bus/uart@3000",
        "/bus/pwm@4000",
        "/bus/pio@5000",
        "/bus/csi@6000",
        "/bus/usb@7000",
        "/bus/wdt@8000",
    ] {
        assert!(
            out.overlay.contains(&format!("target-path = \"{target}\"")),
            "{target}"
        );
    }
    assert!(out.overlay.contains("dr_mode = \"peripheral\""));
    assert!(
        out.overlay.contains("status = \"disabled\";"),
        "the holder is disabled"
    );
}

#[test]
fn no_fan_node_and_no_cooling_binding_is_generated() {
    let out = generated(REFERENCE_BOARD, REFERENCE_MAP).unwrap();
    assert!(!out.overlay.contains("pwm-fan"));
    assert!(!out.overlay.contains("cooling"));
}

#[test]
fn a_gadget_is_absent_without_a_usb_hardware_entry() {
    let board = REFERENCE_BOARD.replace("[[hardware]]\ncontroller = \"watchdog\"\n", "");
    let out = generated(&board, REFERENCE_MAP).unwrap();
    assert!(!out.overlay.contains("usb@480000"));
    assert!(!out.overlay.contains("watchdog"));
}

fn refused(board_text: &str) -> String {
    generated(board_text, ALL_KINDS_MAP).unwrap_err()
}

#[test]
fn a_controller_the_map_lacks_is_refused_with_what_it_has() {
    let board = ALL_KINDS_BOARD.replace("controller = \"wdt\"", "controller = \"nope\"");
    let message = refused(&board);
    assert!(
        message.contains("\"nope\" is not in the test-soc map"),
        "{message}"
    );
    assert!(message.contains("i2c0"), "{message}");
}

#[test]
fn a_pin_set_the_map_lacks_is_refused() {
    let board = ALL_KINDS_BOARD.replace(
        "controller = \"uart0\"\npins = \"a\"",
        "controller = \"uart0\"\npins = \"z\"",
    );
    assert!(refused(&board).contains("no pin set \"z\""));
}

#[test]
fn a_kind_that_needs_pins_is_refused_without_them() {
    let board = ALL_KINDS_BOARD.replace(
        "controller = \"pwm0\"\npins = \"a\"",
        "controller = \"pwm0\"",
    );
    assert!(refused(&board).contains("needs a `pins` set"));
}

#[test]
fn a_gpio_in_two_pin_sets_is_refused() {
    let board = ALL_KINDS_BOARD.replace(
        "controller = \"uart0\"\npins = \"a\"",
        "controller = \"uart0\"\npins = \"a\"\n\n[[hardware]]\ncontroller = \"i2c0\"\npins = \"a\"",
    );
    assert!(refused(&board).contains("listed twice") || refused(&board).contains("GPIO"));
}

#[test]
fn a_chip_name_is_refused_with_the_reason() {
    let board = ALL_KINDS_BOARD.replace("bus = \"i2c-0\"", "bus = \"gpiochip0:9\"");
    let message = refused(&board);
    assert!(message.contains("gpiochip0:9"), "{message}");
    assert!(
        message.contains("no device-tree alias sets the number"),
        "{message}"
    );
}

#[test]
fn an_i2c_name_must_match_its_alias() {
    let board = ALL_KINDS_BOARD.replace("name = \"i2c-0\"", "name = \"i2c-3\"");
    assert!(refused(&board).contains("alias i2c3"));
}

#[test]
fn a_device_naming_an_unlisted_bus_is_refused() {
    let board = ALL_KINDS_BOARD.replace("bus = \"i2c-0\"", "bus = \"i2c-9\"");
    assert!(refused(&board).contains("\"i2c-9\", which no [[hardware]] entry names"));
}

#[test]
fn a_pio_name_needs_the_block() {
    let board = ALL_KINDS_BOARD.replace("[[hardware]]\ncontroller = \"pio\"\n", "");
    assert!(refused(&board).contains("needs the PIO block"));
}

#[test]
fn a_boot_state_without_a_reason_is_refused() {
    let board = ALL_KINDS_BOARD.replace(
        "reason = \"held high from power-on until the fan driver takes it\"\n",
        "",
    );
    assert!(refused(&board).contains("needs a `reason`"));
}

#[test]
fn a_board_for_another_soc_is_refused() {
    let board = ALL_KINDS_BOARD.replace("soc = \"test-soc\"", "soc = \"other\"");
    assert!(refused(&board).contains("the map is for \"test-soc\""));
}

// The start check against fixture live trees.

fn fixture_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("devtree-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("tree")).unwrap();
    fs::create_dir_all(dir.join("sys")).unwrap();
    dir
}

fn node(tree: &Path, path: &str, phandle: Option<u32>, status: Option<&str>, pinctrl: Option<u32>) {
    let dir = tree.join(path.trim_start_matches('/'));
    fs::create_dir_all(&dir).unwrap();
    if let Some(p) = phandle {
        fs::write(dir.join("phandle"), p.to_be_bytes()).unwrap();
    }
    if let Some(s) = status {
        fs::write(dir.join("status"), format!("{s}\0")).unwrap();
    }
    if let Some(p) = pinctrl {
        fs::write(dir.join("pinctrl-0"), p.to_be_bytes()).unwrap();
    }
}

fn run_check(dir: &Path, board_text: &str, map_text: &str) -> Vec<String> {
    let board = board::parse(board_text).unwrap();
    let map = SocMap::parse(map_text).unwrap();
    check(
        &board,
        &map,
        &check::Live {
            tree: &dir.join("tree"),
            sys: &dir.join("sys"),
        },
    )
}

#[test]
fn a_matching_live_tree_has_no_problems() {
    let dir = fixture_dir("ok");
    let tree = dir.join("tree");
    node(&tree, "/bus/i2c@1000", None, Some("okay"), None);
    node(&tree, "/pins/i2c0a", Some(0x10), None, None);
    let problems = run_check(&dir, ALL_KINDS_BOARD, ALL_KINDS_MAP);
    assert!(
        !problems.iter().any(|p| p.starts_with("i2c-0")),
        "{problems:?}"
    );
}

#[test]
fn a_missing_node_is_named() {
    let dir = fixture_dir("absent");
    let problems = run_check(&dir, ALL_KINDS_BOARD, ALL_KINDS_MAP);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("node absent: /bus/i2c@1000")),
        "{problems:?}"
    );
}

#[test]
fn a_disabled_node_is_named() {
    let dir = fixture_dir("disabled");
    node(
        &dir.join("tree"),
        "/bus/i2c@1000",
        None,
        Some("disabled"),
        None,
    );
    let problems = run_check(&dir, ALL_KINDS_BOARD, ALL_KINDS_MAP);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("node disabled: /bus/i2c@1000")),
        "{problems:?}"
    );
}

#[test]
fn a_pin_held_by_another_enabled_node_is_named() {
    let dir = fixture_dir("held");
    let tree = dir.join("tree");
    node(&tree, "/bus/i2c@1000", None, Some("okay"), None);
    node(&tree, "/pins/i2c0a", Some(0x10), None, None);
    // Another node, enabled, configures the same pin set.
    node(&tree, "/bus/pwm@4000", None, Some("okay"), Some(0x10));
    let problems = run_check(&dir, ALL_KINDS_BOARD, ALL_KINDS_MAP);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("pin 0, 1 is held by /bus/pwm@4000")),
        "{problems:?}"
    );
}

#[test]
fn a_stock_driver_bound_where_the_module_must_own_the_node_is_named() {
    let dir = fixture_dir("stock");
    let tree = dir.join("tree");
    node(&tree, "/bus/csi@6000", None, Some("okay"), None);
    let device = dir.join("sys/6000.csi");
    fs::create_dir_all(dir.join("drivers/stock-csi")).unwrap();
    fs::create_dir_all(&device).unwrap();
    std::os::unix::fs::symlink(dir.join("drivers/stock-csi"), device.join("driver")).unwrap();
    let problems = run_check(&dir, ALL_KINDS_BOARD, ALL_KINDS_MAP);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("stock driver stock-csi is bound to 6000.csi")),
        "{problems:?}"
    );
}
