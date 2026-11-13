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
    fs::create_dir_all(dir.join("udc")).unwrap();
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
            gpio_chips: &[("gpio-test".to_string(), 16)],
            udc: &dir.join("udc"),
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

// The three outputs of the reference board are complete: every device is traceable to something
// the outputs make, and what each one makes is in them.

/// The board's devices, each with what its needs must produce in the outputs.
const REFERENCE_DEVICES: &[(&str, &str)] = &[
    // The ring's PIO block: the node enabled, with its reason.
    ("ring", "pio: the RP1 PIO block"),
    // The IMU and the sensor: their buses' nodes enabled and aliased.
    ("imu", "alias i2c1, the name \"i2c-1\""),
    ("sensor", "alias i2c10, the name \"i2c-10\""),
    // The camera power switch: the regulator's node disabled, its line freed, and the module
    // owns the receiver it powers.
    ("cam0-power", "disabled: base \"module-io\" /cam0_reg"),
    // The fan: the PWM block with the pin set of its channel.
    ("fan", "pwm: a PWM channel on RP1"),
];

#[test]
fn every_device_of_the_reference_board_is_traceable_to_an_output() {
    let out = generated(REFERENCE_BOARD, REFERENCE_MAP).unwrap();
    let board = board::parse(REFERENCE_BOARD).unwrap();
    for device in &board.devices {
        let wanted = REFERENCE_DEVICES
            .iter()
            .find(|(id, _)| *id == device.id)
            .unwrap_or_else(|| panic!("device {:?} has no expected output", device.id));
        let found = out.overlay.contains(wanted.1)
            || out.boot.contains(wanted.1)
            || out.modprobe.contains(wanted.1);
        assert!(
            found,
            "device {:?}: {:?} is in no output",
            device.id, wanted.1
        );
    }
}

#[test]
fn the_reference_boards_module_configuration_is_golden() {
    let out = generated(REFERENCE_BOARD, REFERENCE_MAP).unwrap();
    assert_eq!(out.modprobe, golden("reference-modprobe.conf"));
    assert!(out.modprobe.contains(
        "options lemnos owned=raspberrypi,pispbe,raspberrypi,rp1-cfe pisp_be=raspberrypi,pispbe\n"
    ));
    assert!(out.modprobe.contains("blacklist rp1-cfe\n"));
    assert!(out.modprobe.contains("blacklist pispbe\n"));
}

#[test]
fn the_reference_boards_boot_fragment_covers_each_boot_line() {
    let board = board::parse(REFERENCE_BOARD).unwrap();
    let boots = board.lines.iter().filter(|l| l.boot.is_some()).count();
    let out = generated(REFERENCE_BOARD, REFERENCE_MAP).unwrap();
    let gpio_lines = out.boot.lines().filter(|l| l.starts_with("gpio=")).count();
    assert_eq!(boots, gpio_lines);
    assert!(out.boot.contains("gpio=45=op,dh"));
    assert!(out.boot.contains("# gpio:pinctrl-rp1:45: boot high:"));
}

#[test]
fn a_device_that_makes_nothing_is_refused() {
    // A line with neither a base holder nor a boot state frees nothing.
    let board = format!(
        "{ALL_KINDS_BOARD}\n[[lines]]\nline = \"gpio:gpio-test:4\"\n\n[[devices]]\nid = \"switch\"\ndriver = \"switch\"\nuses = {{ line = \"gpio:gpio-test:4\" }}\n"
    );
    assert!(
        refused(&board).contains("makes nothing"),
        "{}",
        refused(&board)
    );
}

#[test]
fn a_device_with_no_hardware_name_is_refused_unless_its_driver_needs_nothing() {
    let board = format!("{ALL_KINDS_BOARD}\n[[devices]]\nid = \"lonely\"\ndriver = \"lonely\"\n");
    assert!(refused(&board).contains("has no hardware name"));
    let composed = format!(
        "{ALL_KINDS_BOARD}\n[[devices]]\nid = \"group\"\ndriver = \"composed\"\nuses = {{ imu = \"imu\" }}\n"
    );
    assert!(generated(&composed, ALL_KINDS_MAP).is_ok());
}

#[test]
fn a_device_names_only_what_the_generator_makes() {
    let board = ALL_KINDS_BOARD.replace("bus = \"i2c-0\"", "bus = \"ttyAMA0\"");
    assert!(refused(&board).contains("makes nothing of that name"));
}

#[test]
fn a_stable_line_name_needs_a_known_chip() {
    let board = ALL_KINDS_BOARD.replace("gpio:gpio-test:9", "gpio:nope:9");
    assert!(refused(&board).contains("no GPIO chip labelled \"nope\""));
}

#[test]
fn a_numbered_line_name_is_refused_with_the_stable_form() {
    let board = ALL_KINDS_BOARD.replace("gpio:gpio-test:9", "gpiochip0:9");
    let message = refused(&board);
    assert!(message.contains("gpio:LABEL:LINE"), "{message}");
}

#[test]
fn a_device_on_a_stable_gpio_line_needs_the_line_given_to_lemnos() {
    let board = format!(
        "{ALL_KINDS_BOARD}\n[[devices]]\nid = \"switch\"\ndriver = \"gpio-switch\"\nuses = {{ line = \"gpio:gpio-test:4\" }}\n"
    );
    assert!(refused(&board).contains("which no [[lines]] entry gives to Lemnos"));
}

#[test]
fn a_pwm_name_needs_its_block_in_the_map_and_the_board() {
    let board = format!(
        "{ALL_KINDS_BOARD}\n[[devices]]\nid = \"fan\"\ndriver = \"pwm-channel-fan\"\nuses = {{ pwm = \"pwm:/bus/nope@1:0\" }}\n"
    );
    assert!(refused(&board).contains("the map has no PWM block at /bus/nope@1"));
    let board = ALL_KINDS_BOARD.replace("[[hardware]]\ncontroller = \"pwm0\"\npins = \"a\"\n", "")
        .replace("[[devices]]\nid = \"ring\"", "[[devices]]\nid = \"fan\"\ndriver = \"pwm-channel-fan\"\nuses = { pwm = \"pwm:/bus/pwm@4000:0\" }\n\n[[devices]]\nid = \"ring\"");
    assert!(refused(&board).contains("needs the PWM block"));
}

#[test]
fn a_module_node_needs_a_compatible_and_a_role_the_map_gives_it() {
    let board = ALL_KINDS_BOARD.replace("module = \"pisp_be\"", "module = \"gsdma\"");
    assert!(refused(&board).contains("has module role \"gsdma\""));
    let map = ALL_KINDS_MAP.replace("compatible = \"test,csi\"\n", "");
    let message = generated(ALL_KINDS_BOARD, &map).unwrap_err();
    assert!(message.contains("no `compatible`"), "{message}");
}

#[test]
fn the_all_kinds_module_configuration_names_each_option_and_blacklist() {
    let out = generated(ALL_KINDS_BOARD, ALL_KINDS_MAP).unwrap();
    assert_eq!(out.modprobe, golden("all-kinds-modprobe.conf"));
    assert!(out.modprobe.contains("owned=test,csi pisp_be=test,csi"));
    assert!(out.modprobe.contains("blacklist stock-csi"));
}

#[test]
fn a_pwm_channel_the_board_names_must_be_an_enabled_node() {
    let dir = fixture_dir("pwm-name");
    let tree = dir.join("tree");
    let board = ALL_KINDS_BOARD.replace(
        "[[devices]]\nid = \"ring\"",
        "[[devices]]\nid = \"fan\"\ndriver = \"pwm-channel-fan\"\nuses = { pwm = \"pwm:/bus/pwm@4000:0\" }\n\n[[devices]]\nid = \"ring\"",
    );
    node(&tree, "/bus/pwm@4000", None, Some("disabled"), None);
    let problems = run_check(&dir, &board, ALL_KINDS_MAP);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("pwm:/bus/pwm@4000:0: node disabled: /bus/pwm@4000")),
        "{problems:?}"
    );
}

#[test]
fn a_stable_gpio_name_needs_its_chip_on_the_machine_with_the_line_in_range() {
    let dir = fixture_dir("gpio-chips");
    let tree = dir.join("tree");
    node(&tree, "/bus/i2c@1000", None, Some("okay"), None);
    node(&tree, "/pins/i2c0a", Some(0x10), None, None);
    let problems = run_check(&dir, ALL_KINDS_BOARD, ALL_KINDS_MAP);
    assert!(
        !problems.iter().any(|p| p.contains("gpio-test")),
        "{problems:?}"
    );
    // The line 9 of the board is past a 4-line chip.
    let board = board::parse(ALL_KINDS_BOARD).unwrap();
    let map = SocMap::parse(ALL_KINDS_MAP).unwrap();
    let small = [("gpio-test".to_string(), 4u32)];
    let problems = check(
        &board,
        &map,
        &check::Live {
            tree: &tree,
            sys: &dir.join("sys"),
            gpio_chips: &small,
            udc: &dir.join("udc"),
        },
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("line 9 is past the 4 lines of the chip labelled \"gpio-test\"")),
        "{problems:?}"
    );
    // No chip of that label at all: named with the chips there are.
    let other = [("pinctrl-rp1".to_string(), 54u32)];
    let problems = check(
        &board,
        &map,
        &check::Live {
            tree: &tree,
            sys: &dir.join("sys"),
            gpio_chips: &other,
            udc: &dir.join("udc"),
        },
    );
    assert!(
        problems.iter().any(|p| p.contains(
            "no GPIO chip labelled \"gpio-test\" on this machine (the chips are: pinctrl-rp1)"
        )),
        "{problems:?}"
    );
}

// The SoC families: each map under `socs/` with a board file under `testing/boards/`, and goldens for
// the three outputs. The maps cite the device trees they were read from (see each map's header); the
// goldens check the generator, not the trees.

fn family(soc: &str) -> (&'static str, &'static str) {
    match soc {
        "rk3566" => (
            include_str!("../../../socs/rk3566.toml"),
            include_str!("../../../testing/boards/rk3566.toml"),
        ),
        "rk3588" => (
            include_str!("../../../socs/rk3588.toml"),
            include_str!("../../../testing/boards/rk3588.toml"),
        ),
        "rk3576" => (
            include_str!("../../../socs/rk3576.toml"),
            include_str!("../../../testing/boards/rk3576.toml"),
        ),
        "k230" => (
            include_str!("../../../socs/k230.toml"),
            include_str!("../../../testing/boards/k230.toml"),
        ),
        "sun60i-a733" => (
            include_str!("../../../socs/sun60i-a733.toml"),
            include_str!("../../../testing/boards/sun60i-a733.toml"),
        ),
        other => panic!("no family {other}"),
    }
}

#[test]
fn every_family_makes_its_three_outputs_as_its_goldens() {
    for soc in ["rk3566", "rk3588", "rk3576", "k230", "sun60i-a733"] {
        let (map, board_text) = family(soc);
        let out = generated(board_text, map).unwrap_or_else(|e| panic!("{soc}: {e}"));
        assert_eq!(out.overlay, golden(&format!("{soc}.dts")), "{soc} overlay");
        assert_eq!(out.boot, golden(&format!("{soc}-boot.txt")), "{soc} boot");
        assert_eq!(
            out.modprobe,
            golden(&format!("{soc}-modprobe.conf")),
            "{soc} modprobe"
        );
    }
}

#[test]
fn the_overlay_families_are_plugin_overlays_and_the_fragment_families_are_not() {
    for soc in ["rk3566", "rk3588", "rk3576"] {
        let (map, board_text) = family(soc);
        let out = generated(board_text, map).unwrap();
        assert!(out.overlay.contains("/plugin/;"), "{soc}");
        assert!(out.overlay.contains("target-path = \"/"), "{soc}");
    }
    for soc in ["k230", "sun60i-a733"] {
        let (map, board_text) = family(soc);
        let out = generated(board_text, map).unwrap();
        assert!(!out.overlay.contains("/plugin/;"), "{soc}");
        assert!(!out.overlay.contains("/dts-v1/;"), "{soc}");
        assert!(!out.overlay.contains("target-path"), "{soc}");
        assert!(
            out.overlay.contains("&{/soc/i2c@"),
            "{soc}: {}",
            out.overlay
        );
    }
}

#[test]
fn a_fragment_map_with_a_disabled_holder_writes_a_path_block() {
    let map = format!("{}\n", include_str!("../../../socs/k230.toml"));
    let base = "[chips.x]\nlabel = \"x\"\n[bases.b.lines.\"gpio:x:1\"]\nholders = [\"/soc/holder\"]\nreason = \"a test holder\"\n";
    let map = format!("{map}{base}");
    let board = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"f\"\nsoc = \"k230\"\nbase = \"b\"\n[[lines]]\nline = \"gpio:x:1\"\n";
    let out = generated(board, &map).unwrap();
    assert!(
        out.overlay
            .contains("&{/soc/holder} {\n\tstatus = \"disabled\";\n};"),
        "{}",
        out.overlay
    );
}

// The USB device port: a gadget makes its controller a device, with the PHYs it needs; no gadget makes
// none. The board names the port and its options; the map says what each needs.

const RK3566_MAP: &str = include_str!("../../../socs/rk3566.toml");
const RK3566_BOARD: &str = include_str!("../../../testing/boards/rk3566.toml");
const RK3588_MAP: &str = include_str!("../../../socs/rk3588.toml");
const RK3588_BOARD: &str = include_str!("../../../testing/boards/rk3588.toml");
const RK3576_MAP: &str = include_str!("../../../socs/rk3576.toml");
const RK3576_BOARD: &str = include_str!("../../../testing/boards/rk3576.toml");

/// The overlay's fragment for `target`: its text from its `target-path` line to its closing brace.
fn fragment_for<'a>(overlay: &'a str, target: &str) -> &'a str {
    let at = overlay
        .find(&format!("target-path = \"{target}\""))
        .unwrap_or_else(|| panic!("no fragment for {target}:\n{overlay}"));
    let end = overlay[at..].find("\t};").map_or(overlay.len(), |e| at + e);
    &overlay[at..end]
}

#[test]
fn a_gadget_makes_its_usb_port_a_peripheral_with_the_phy_it_needs() {
    let out = generated(RK3566_BOARD, RK3566_MAP).unwrap();
    let port = fragment_for(&out.overlay, "/usb@fcc00000");
    assert!(port.contains("status = \"okay\";"), "{port}");
    assert!(port.contains("dr_mode = \"peripheral\";"), "{port}");
    assert!(fragment_for(&out.overlay, "/usb2phy@fe8a0000").contains("status = \"okay\";"));
    assert!(
        fragment_for(&out.overlay, "/usb2phy@fe8a0000/otg-port").contains("status = \"okay\";")
    );
    // The device port is only ever the one the board names: no host VBUS supply is written for it.
    assert!(!out.overlay.contains("phy-supply"), "{}", out.overlay);
}

#[test]
fn the_rk3588_port_enables_its_usb3_phy_only_when_the_board_names_usb3() {
    let out = generated(RK3588_BOARD, RK3588_MAP).unwrap();
    assert!(fragment_for(&out.overlay, "/usb@fc000000").contains("dr_mode = \"peripheral\";"));
    assert!(fragment_for(&out.overlay, "/syscon@fd5d0000/usb2phy@0/otg-port").contains("okay"));
    assert!(fragment_for(&out.overlay, "/phy@fed80000").contains("status = \"okay\";"));
    // The same board without the option: the USB 2 side only.
    let board = RK3588_BOARD.replace("options = [\"usb3\"]\n", "");
    let out = generated(&board, RK3588_MAP).unwrap();
    assert!(!out.overlay.contains("/phy@fed80000"), "{}", out.overlay);
    assert!(out.overlay.contains("dr_mode = \"peripheral\";"));
}

#[test]
fn the_rk3576_ports_name_their_own_udc_and_phys() {
    let out = generated(RK3576_BOARD, RK3576_MAP).unwrap();
    assert!(fragment_for(&out.overlay, "/soc/usb@23000000").contains("dr_mode = \"peripheral\";"));
    assert!(fragment_for(&out.overlay, "/soc/phy@2b010000").contains("okay"));
    assert!(
        !out.overlay.contains("/soc/usb@23400000"),
        "drd1 is not the board's port"
    );
    // The second port, named by the board in its place, with its own PHYs.
    let board = RK3576_BOARD
        .replace("udc:23000000.usb", "udc:23400000.usb")
        .replace("usb-drd0", "usb-drd1");
    let out = generated(&board, RK3576_MAP).unwrap();
    assert!(fragment_for(&out.overlay, "/soc/usb@23400000").contains("dr_mode = \"peripheral\";"));
    assert!(fragment_for(&out.overlay, "/soc/syscon@2602e000/usb2-phy@2000").contains("okay"));
    assert!(fragment_for(&out.overlay, "/soc/phy@2b060000").contains("okay"));
    assert!(!out.overlay.contains("usb2-phy@0"), "{}", out.overlay);
}

#[test]
fn no_gadget_makes_no_peripheral_port() {
    let board = RK3566_BOARD
        .replace("[[hardware]]\nname = \"udc:fcc00000.usb\"\ncontroller = \"usb-otg0\"\nrole = \"peripheral\"\n", "")
        .replace("[[devices]]\nid = \"gadget\"\ndriver = \"usb-gadget\"\nuses = { function = \"udc:fcc00000.usb\" }\n", "");
    assert!(!board.contains("usb-otg0") && !board.contains("usb-gadget"));
    let out = generated(&board, RK3566_MAP).unwrap();
    assert!(!out.overlay.contains("usb@fcc00000"), "{}", out.overlay);
    assert!(!out.overlay.contains("dr_mode"), "{}", out.overlay);
    assert!(!out.overlay.contains("usb2phy"), "{}", out.overlay);
}

#[test]
fn a_port_no_gadget_names_is_refused() {
    let board = RK3566_BOARD.replace(
        "[[devices]]\nid = \"gadget\"\ndriver = \"usb-gadget\"\nuses = { function = \"udc:fcc00000.usb\" }\n",
        "",
    );
    let err = generated(&board, RK3566_MAP).unwrap_err();
    assert!(
        err.contains("usb-otg0") && err.contains("no device names it"),
        "{err}"
    );
}

#[test]
fn a_usb_port_the_map_lacks_is_refused_with_what_it_has() {
    let board = RK3566_BOARD.replace("controller = \"usb-otg0\"", "controller = \"usb-otg1\"");
    let err = generated(&board, RK3566_MAP).unwrap_err();
    assert!(
        err.contains("\"usb-otg1\" is not in the rk3566 map"),
        "{err}"
    );
    assert!(err.contains("usb-otg0"), "{err}");
}

#[test]
fn a_udc_name_must_be_the_controllers_udc() {
    let board = RK3566_BOARD.replace("udc:fcc00000.usb", "udc:fcc00001.usb");
    let err = generated(&board, RK3566_MAP).unwrap_err();
    assert!(
        err.contains("\"fcc00000.usb\"") && err.contains("udc:fcc00000.usb"),
        "{err}"
    );
}

#[test]
fn a_port_option_the_map_does_not_give_is_refused() {
    let board = RK3566_BOARD.replace(
        "role = \"peripheral\"\n",
        "role = \"peripheral\"\noptions = [\"usb3\"]\n",
    );
    let err = generated(&board, RK3566_MAP).unwrap_err();
    assert!(err.contains("no option \"usb3\""), "{err}");
}

#[test]
fn a_port_role_other_than_peripheral_is_refused() {
    let board = RK3566_BOARD.replace("role = \"peripheral\"", "role = \"host\"");
    let err = generated(&board, RK3566_MAP).unwrap_err();
    assert!(err.contains("role \"host\""), "{err}");
}

#[test]
fn a_usb_peripheral_map_entry_needs_its_udc_name() {
    let map = RK3566_MAP.replace("sysfs = \"fcc00000.usb\"\n", "");
    let err = SocMap::parse(&map).unwrap_err();
    assert!(err.contains("needs `sysfs`"), "{err}");
}

#[test]
fn a_needed_node_must_have_an_absolute_path() {
    let map = RK3566_MAP.replace(
        "path = \"/usb2phy@fe8a0000\"",
        "path = \"usb2phy@fe8a0000\"",
    );
    let err = SocMap::parse(&map).unwrap_err();
    assert!(err.contains("absolute path"), "{err}");
}

/// The OTG port's phandle in the fixture tree, and the port's `phys` entry on the controller.
const OTG_PORT: u32 = 0x13;

fn usb_tree(
    dir: &Path,
    dr_mode: Option<&str>,
    port: Option<&str>,
    phy: Option<&str>,
    linked: bool,
    udc: bool,
) {
    let tree = dir.join("tree");
    node(&tree, "/usb@fcc00000", None, Some("okay"), None);
    if let Some(mode) = dr_mode {
        fs::write(
            tree.join("usb@fcc00000").join("dr_mode"),
            format!("{mode}\0"),
        )
        .unwrap();
    }
    if linked {
        fs::write(
            tree.join("usb@fcc00000").join("phys"),
            OTG_PORT.to_be_bytes(),
        )
        .unwrap();
    }
    node(&tree, "/usb2phy@fe8a0000", None, phy, None);
    node(
        &tree,
        "/usb2phy@fe8a0000/otg-port",
        Some(OTG_PORT),
        port,
        None,
    );
    if udc {
        fs::create_dir_all(dir.join("udc").join("fcc00000.usb")).unwrap();
    }
}

fn usb_problems(
    name: &str,
    dr_mode: Option<&str>,
    port: Option<&str>,
    phy: Option<&str>,
    udc: bool,
) -> Vec<String> {
    let dir = fixture_dir(name);
    usb_tree(&dir, dr_mode, port, phy, true, udc);
    run_check(&dir, RK3566_BOARD, RK3566_MAP)
        .into_iter()
        .filter(|p| p.contains("USB port"))
        .collect()
}

#[test]
fn a_peripheral_port_with_its_phy_on_and_its_udc_has_no_usb_problem() {
    let problems = usb_problems(
        "usb-ok",
        Some("peripheral"),
        Some("okay"),
        Some("okay"),
        true,
    );
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn a_port_in_host_mode_is_named() {
    let problems = usb_problems("usb-host", Some("host"), Some("okay"), Some("okay"), true);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("USB port usb-otg0: host mode") && p.contains("\"host\"")),
        "{problems:?}"
    );
}

#[test]
fn a_port_with_no_dr_mode_is_named_as_not_in_peripheral_mode() {
    let problems = usb_problems("usb-nomode", None, Some("okay"), Some("okay"), true);
    assert!(
        problems.iter().any(|p| p.contains("no dr_mode")),
        "{problems:?}"
    );
}

#[test]
fn a_disabled_port_phy_is_named() {
    let problems = usb_problems(
        "usb-phydis",
        Some("peripheral"),
        Some("disabled"),
        Some("okay"),
        true,
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("disabled") && p.contains("/usb2phy@fe8a0000/otg-port")),
        "{problems:?}"
    );
}

#[test]
fn a_missing_phy_is_named() {
    // The PHY's node is absent: the overlay did not apply, or the base tree is not the board's.
    let dir = fixture_dir("usb-phymissing");
    let tree = dir.join("tree");
    node(&tree, "/usb@fcc00000", None, Some("okay"), None);
    fs::write(tree.join("usb@fcc00000").join("dr_mode"), "peripheral\0").unwrap();
    fs::write(
        tree.join("usb@fcc00000").join("phys"),
        OTG_PORT.to_be_bytes(),
    )
    .unwrap();
    fs::create_dir_all(dir.join("udc").join("fcc00000.usb")).unwrap();
    let problems = run_check(&dir, RK3566_BOARD, RK3566_MAP);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("USB port usb-otg0") && p.contains("/usb2phy@fe8a0000 is absent")),
        "{problems:?}"
    );
}

#[test]
fn a_port_its_phy_is_not_linked_to_is_named() {
    let dir = fixture_dir("usb-unlinked");
    usb_tree(
        &dir,
        Some("peripheral"),
        Some("okay"),
        Some("okay"),
        false,
        true,
    );
    let problems = run_check(&dir, RK3566_BOARD, RK3566_MAP);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("PHY link missing") && p.contains("/usb2phy@fe8a0000/otg-port")),
        "{problems:?}"
    );
}

#[test]
fn a_port_with_no_udc_is_named() {
    let problems = usb_problems(
        "usb-noudc",
        Some("peripheral"),
        Some("okay"),
        Some("okay"),
        false,
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("no UDC \"fcc00000.usb\"")),
        "{problems:?}"
    );
}
