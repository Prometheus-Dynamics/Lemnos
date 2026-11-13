//! The VBUS rules for a device port: the generated overlay replaces the port's PHY supply with a
//! stand-in that supplies nothing; the base tree's always-on or switched supply must be stated by
//! the board (a line that holds it off, or a named assertion that the port is not driven); and the
//! start check reads the live PHY, the line and the regulators. The boards are the Rockchip fixture
//! boards under `testing/boards/`, read with the maps under `socs/`.

use std::fs;
use std::path::{Path, PathBuf};

use lemnos_devtree::check::{self, Live, gpio_level};
use lemnos_devtree::{SocMap, generate};
use lemnos_host::board;

const RK3588_MAP: &str = include_str!("../../../socs/rk3588.toml");
const RK3576_MAP: &str = include_str!("../../../socs/rk3576.toml");
const RK3566_MAP: &str = include_str!("../../../socs/rk3566.toml");
const RK3588_BOARD: &str = include_str!("../../../testing/boards/rk3588.toml");
const RK3576_BOARD: &str = include_str!("../../../testing/boards/rk3576.toml");
const RK3566_BOARD: &str = include_str!("../../../testing/boards/rk3566.toml");

/// The switched statement the RK3588 fixture board makes (its base's switch is `gpio:gpio4:11`).
const RK3588_STATEMENT: &str = "vbus = \"gpio:gpio4:11\"\nvbus_off = \"low\"\nvbus_reason = \"the OTG port's 5 V regulator is enabled by GPIO4 line 11; low is off\"\n";
/// The undriven statement the RK3576 fixture board makes for its always-on drd0 port.
const RK3576_UNDRIVEN: &str = "vbus_undriven = true\nvbus_reason = \"fixture: this port's VBUS is not driven on this board (from the schematic; a meter confirms it)\"\n";

/// The manifests of the drivers the tool knows, as the generator checks a device's declared needs.
fn manifests() -> Vec<lemnos_core::Manifest> {
    lemnosd::builtin::drivers()
        .iter()
        .map(|entry| *entry.manifest())
        .collect()
}

fn generated(board_text: &str, map_text: &str) -> Result<generate::Output, String> {
    let board = board::parse(board_text).map_err(|e| e.to_string())?;
    let map = SocMap::parse(map_text)?;
    generate(&board, &map, &manifests())
}

fn refused(board_text: &str, map_text: &str) -> String {
    generated(board_text, map_text).unwrap_err()
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

// The overlay: the stand-in on every device port's PHY.

#[test]
fn every_device_port_phy_gets_the_stand_in_supply() {
    for (board_text, map_text) in [
        (RK3588_BOARD, RK3588_MAP),
        (RK3576_BOARD, RK3576_MAP),
        (RK3566_BOARD, RK3566_MAP),
    ] {
        let out = match generated(board_text, map_text) {
            Ok(out) => out,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(
            count(&out.overlay, "regulator-lemnos-no-vbus {"),
            1,
            "{}",
            out.overlay
        );
        assert_eq!(
            count(&out.overlay, "phy-supply = <&lemnos_no_vbus>;"),
            1,
            "{}",
            out.overlay
        );
        assert!(
            out.overlay.contains("target-path = \"/\";"),
            "the stand-in is a root node"
        );
    }
}

#[test]
fn the_stand_in_is_one_node_for_two_device_ports() {
    let two = format!(
        "{RK3576_BOARD}\n[[hardware]]\nname = \"udc:23400000.usb\"\ncontroller = \"usb-drd1\"\nrole = \"peripheral\"\noptions = [\"usb3\"]\nvbus_undriven = true\nvbus_reason = \"fixture: the second port is not driven on this board\"\n\n[[devices]]\nid = \"gadget1\"\ndriver = \"usb-gadget\"\nuses = {{ function = \"udc:23400000.usb\" }}\n"
    );
    let out = generated(&two, RK3576_MAP).unwrap();
    assert_eq!(
        count(&out.overlay, "regulator-lemnos-no-vbus {"),
        1,
        "{}",
        out.overlay
    );
    assert_eq!(
        count(&out.overlay, "phy-supply = <&lemnos_no_vbus>;"),
        2,
        "{}",
        out.overlay
    );
}

// The three board shapes.

#[test]
fn a_gpio_switched_port_holds_its_line_off_in_the_boot_fragment() {
    let out = generated(RK3588_BOARD, RK3588_MAP).unwrap();
    // GPIO4 is SoC GPIO 128 onwards, so line 11 is GPIO 139. Low is off.
    assert!(out.boot.contains("gpio=139=op,dl\n"), "{}", out.boot);
    assert!(
        out.boot.contains("VBUS of usb-otg0 held low (off)"),
        "{}",
        out.boot
    );
}

#[test]
fn a_hold_at_high_writes_dh() {
    let board_text = RK3588_BOARD.replace("vbus_off = \"low\"", "vbus_off = \"high\"");
    let out = generated(&board_text, RK3588_MAP).unwrap();
    assert!(out.boot.contains("gpio=139=op,dh\n"), "{}", out.boot);
}

#[test]
fn a_no_supply_port_needs_no_statement_and_holds_nothing() {
    let out = generated(RK3566_BOARD, RK3566_MAP).unwrap();
    // The fixture's own `[[lines]]` hold is a different line; the port adds no hold of its own.
    assert!(!out.boot.contains("VBUS"), "{}", out.boot);
    assert_eq!(count(&out.boot, "gpio="), 1, "{}", out.boot);
}

#[test]
fn an_always_on_port_with_nothing_stated_is_refused_naming_its_regulator() {
    let board_text = RK3576_BOARD.replace(RK3576_UNDRIVEN, "");
    let message = refused(&board_text, RK3576_MAP);
    assert!(message.contains("/vcc5v0-otg0-vbus"), "{message}");
    assert!(message.contains("is always on"), "{message}");
    assert!(message.contains("vbus_undriven = true"), "{message}");
}

#[test]
fn each_always_on_port_needs_its_own_statement() {
    // The second port's regulator is the host 5 V: stated for drd0, nothing for drd1.
    let board_text = format!(
        "{RK3576_BOARD}\n[[hardware]]\nname = \"udc:23400000.usb\"\ncontroller = \"usb-drd1\"\nrole = \"peripheral\"\noptions = [\"usb3\"]\n\n[[devices]]\nid = \"gadget1\"\ndriver = \"usb-gadget\"\nuses = {{ function = \"udc:23400000.usb\" }}\n"
    );
    let message = refused(&board_text, RK3576_MAP);
    assert!(message.contains("/vcc5v0-host-vbus"), "{message}");
    assert!(message.contains("name \"udc:23400000.usb\""), "{message}");
}

#[test]
fn two_always_on_ports_stated_on_their_own_generate_both_stand_ins() {
    let board_text = format!(
        "{RK3576_BOARD}\n[[hardware]]\nname = \"udc:23400000.usb\"\ncontroller = \"usb-drd1\"\nrole = \"peripheral\"\noptions = [\"usb3\"]\nvbus_undriven = true\nvbus_reason = \"fixture: the second port is not driven on this board\"\n\n[[devices]]\nid = \"gadget1\"\ndriver = \"usb-gadget\"\nuses = {{ function = \"udc:23400000.usb\" }}\n"
    );
    let out = generated(&board_text, RK3576_MAP).unwrap();
    assert_eq!(
        count(&out.overlay, "phy-supply = <&lemnos_no_vbus>;"),
        2,
        "{}",
        out.overlay
    );
    assert!(!out.boot.contains("VBUS"), "{}", out.boot);
}

#[test]
fn an_always_on_port_may_hold_its_vbus_by_a_line_switched_elsewhere() {
    let board_text = RK3576_BOARD.replace(
        RK3576_UNDRIVEN,
        "vbus = \"gpio:gpio4:11\"\nvbus_off = \"low\"\nvbus_reason = \"fixture: VBUS is switched by GPIO4 line 11 after the regulator\"\n",
    );
    let out = generated(&board_text, RK3576_MAP).unwrap();
    assert!(out.boot.contains("gpio=139=op,dl\n"), "{}", out.boot);
}

// The statement: what the board must say, and what it must not.

#[test]
fn a_switched_port_with_nothing_stated_is_refused_naming_the_line() {
    let board_text = RK3588_BOARD.replace(RK3588_STATEMENT, "");
    let message = refused(&board_text, RK3588_MAP);
    assert!(message.contains("switched by gpio:gpio4:11"), "{message}");
    assert!(message.contains("`vbus_undriven = true`"), "{message}");
}

#[test]
fn a_statement_naming_another_line_than_the_base_switch_is_refused() {
    let board_text = RK3588_BOARD.replace("gpio:gpio4:11", "gpio:gpio4:12");
    let message = refused(&board_text, RK3588_MAP);
    assert!(message.contains("must be the same line"), "{message}");
}

#[test]
fn a_vbus_line_needs_its_off_level() {
    let board_text = RK3588_BOARD.replace("vbus_off = \"low\"\n", "");
    assert!(refused(&board_text, RK3588_MAP).contains("must say its level when VBUS is off"));
}

#[test]
fn a_vbus_off_must_be_low_or_high() {
    let board_text = RK3588_BOARD.replace("vbus_off = \"low\"", "vbus_off = \"middle\"");
    assert!(refused(&board_text, RK3588_MAP).contains("\"middle\" is not \"low\" or \"high\""));
}

#[test]
fn a_vbus_statement_needs_its_reason() {
    let board_text = RK3588_BOARD.replace(
        "vbus_reason = \"the OTG port's 5 V regulator is enabled by GPIO4 line 11; low is off\"\n",
        "",
    );
    assert!(refused(&board_text, RK3588_MAP).contains("needs `vbus_reason`"));
}

#[test]
fn vbus_and_vbus_undriven_are_one_or_the_other() {
    let board_text = RK3588_BOARD.replace(
        "vbus_off = \"low\"",
        "vbus_off = \"low\"\nvbus_undriven = true",
    );
    assert!(refused(&board_text, RK3588_MAP).contains("one or the other"));
}

#[test]
fn vbus_off_or_reason_without_a_statement_is_refused() {
    let board_text = RK3566_BOARD.replace(
        "[[hardware]]\nname = \"udc:fcc00000.usb\"\ncontroller = \"usb-otg0\"\nrole = \"peripheral\"\n",
        "[[hardware]]\nname = \"udc:fcc00000.usb\"\ncontroller = \"usb-otg0\"\nrole = \"peripheral\"\nvbus_off = \"low\"\n",
    );
    assert!(refused(&board_text, RK3566_MAP).contains("go with `vbus` or `vbus_undriven`"));
}

#[test]
fn a_vbus_line_that_is_also_a_boot_line_is_refused() {
    let board_text = format!(
        "{RK3588_BOARD}\n[[lines]]\nline = \"gpio:gpio4:11\"\nboot = \"high\"\nreason = \"fixture: a second hold of the same line\"\n"
    );
    assert!(refused(&board_text, RK3588_MAP).contains("hold it in one place"));
}

// A base the generator cannot see a port in.

#[test]
fn a_port_with_no_base_is_refused_unless_stated() {
    let board_text = RK3566_BOARD.replace("base = \"usb-no-vbus\"\n", "");
    let message = refused(&board_text, RK3566_MAP);
    assert!(message.contains("does not describe its VBUS"), "{message}");
    assert!(
        message.contains("(none is named by [board] base)"),
        "{message}"
    );
}

#[test]
fn a_base_that_does_not_describe_the_port_is_refused_unless_stated() {
    let map = format!("{RK3566_MAP}\n[bases.empty]\n");
    let board_text = RK3566_BOARD.replace("base = \"usb-no-vbus\"", "base = \"empty\"");
    assert!(refused(&board_text, &map).contains("does not describe its VBUS"));
}

// The map's VBUS facts are checked when the map is read.

#[test]
fn a_vbus_fact_on_a_controller_that_is_not_a_device_port_is_refused() {
    let map = format!("{RK3588_MAP}\n[bases.bad.vbus.i2c0]\nreason = \"x\"\n");
    let err = SocMap::parse(&map).unwrap_err();
    assert!(
        err.contains("only a usb-peripheral port has a VBUS fact"),
        "{err}"
    );
}

#[test]
fn an_always_on_regulator_that_is_also_switched_is_refused() {
    let map = RK3588_MAP.replace(
        "switch = \"gpio:gpio4:11\"",
        "switch = \"gpio:gpio4:11\"\nalways_on = true",
    );
    let err = SocMap::parse(&map).unwrap_err();
    assert!(
        err.contains("always_on or switched by a line, not both"),
        "{err}"
    );
}

#[test]
fn a_regulator_needs_its_name_and_a_fact_needs_its_reason() {
    let map = RK3588_MAP.replace("regulator_name = \"vcc5v0_otg\"\n", "");
    assert!(
        SocMap::parse(&map)
            .unwrap_err()
            .contains("needs its regulator_name")
    );
    let map = RK3588_MAP.replace(
        "reason = \"a fixed regulator on the OTG port's phy-supply, switched by its enable line (the base tree's)\"",
        "reason = \"\"",
    );
    assert!(
        SocMap::parse(&map)
            .unwrap_err()
            .contains("needs its reason")
    );
}

#[test]
fn a_switch_on_a_chip_the_map_lacks_is_refused() {
    let map = RK3588_MAP.replace("switch = \"gpio:gpio4:11\"", "switch = \"gpio:gpio9:11\"");
    assert!(
        SocMap::parse(&map)
            .unwrap_err()
            .contains("names no GPIO chip of the map")
    );
}

// The start check: the live PHY, the line and the regulators.

fn fixture(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("vbus-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("tree")).unwrap();
    fs::create_dir_all(dir.join("sys")).unwrap();
    fs::create_dir_all(dir.join("udc")).unwrap();
    fs::create_dir_all(dir.join("regulators")).unwrap();
    dir
}

/// A node of the live tree: its `status`, and its `phandle` when it has one.
fn put(tree: &Path, path: &str, phandle: Option<u32>, status: &str) -> PathBuf {
    let dir = tree.join(path.trim_start_matches('/'));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("status"), format!("{status}\0")).unwrap();
    if let Some(p) = phandle {
        fs::write(dir.join("phandle"), p.to_be_bytes()).unwrap();
    }
    dir
}

/// A live RK3576 port in device mode with its PHY, the base regulators (phandles 40 and 41) and,
/// when given, the stand-in (phandle 42) as the PHY's supply.
fn rk3576_live(dir: &Path, phy_supply: Option<u32>) {
    let tree = dir.join("tree");
    let usb = put(&tree, "/soc/usb@23000000", None, "okay");
    fs::write(usb.join("dr_mode"), "peripheral\0").unwrap();
    put(&tree, "/soc/syscon@2602e000/usb2-phy@0", None, "okay");
    let phy = put(
        &tree,
        "/soc/syscon@2602e000/usb2-phy@0/otg-port",
        Some(50),
        "okay",
    );
    if let Some(p) = phy_supply {
        fs::write(phy.join("phy-supply"), p.to_be_bytes()).unwrap();
    }
    put(&tree, "/vcc5v0-otg0-vbus", Some(40), "okay");
    put(&tree, "/regulator-lemnos-no-vbus", Some(42), "okay");
    fs::create_dir_all(dir.join("udc").join("23000000.usb")).unwrap();
}

fn live_check(
    dir: &Path,
    board_text: &str,
    map_text: &str,
    debug: Option<&str>,
) -> check::Findings {
    let board = board::parse(board_text).unwrap();
    let map = SocMap::parse(map_text).unwrap();
    if let Some(text) = debug {
        fs::write(dir.join("gpio"), text).unwrap();
    }
    check::check(
        &board,
        &map,
        &Live {
            tree: &dir.join("tree"),
            sys: &dir.join("sys"),
            gpio_chips: &[("gpio4".to_string(), 32)],
            udc: &dir.join("udc"),
            gpio_debug: &dir.join("gpio"),
            regulators: &dir.join("regulators"),
        },
    )
}

fn vbus_problems(findings: &check::Findings) -> Vec<String> {
    findings
        .problems
        .iter()
        .filter(|p| p.contains("VBUS") || p.contains("phy-supply") || p.contains("regulator"))
        .cloned()
        .collect()
}

#[test]
fn a_phy_with_the_stand_in_has_no_vbus_problem() {
    let dir = fixture("standin");
    rk3576_live(&dir, Some(42));
    let findings = live_check(&dir, RK3576_BOARD, RK3576_MAP, None);
    assert!(vbus_problems(&findings).is_empty(), "{findings:?}");
}

#[test]
fn a_phy_that_still_has_its_base_supply_is_named() {
    let dir = fixture("base-supply");
    rk3576_live(&dir, Some(40));
    let findings = live_check(&dir, RK3576_BOARD, RK3576_MAP, None);
    let problems = vbus_problems(&findings);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("still has phy-supply /vcc5v0-otg0-vbus")),
        "{problems:?}"
    );
}

#[test]
fn a_switched_line_at_its_off_level_passes() {
    let dir = fixture("line-off");
    rk3576_live(&dir, Some(40));
    let board_text = RK3576_BOARD.replace(
        RK3576_UNDRIVEN,
        "vbus = \"gpio:gpio4:11\"\nvbus_off = \"low\"\nvbus_reason = \"fixture: switched by GPIO4 line 11\"\n",
    );
    // gpio4 starts at GPIO 128 in the listing; line 11 is global 139.
    let listing = "gpiochip4: GPIOs 128-159, parent: platform/fec50000.gpio, gpio4:\n gpio-139 (vcc-otg-en          ) out lo\n";
    let findings = live_check(&dir, &board_text, RK3576_MAP, Some(listing));
    assert!(vbus_problems(&findings).is_empty(), "{findings:?}");
}

#[test]
fn a_switched_line_reading_the_wrong_level_is_named() {
    let dir = fixture("line-high");
    rk3576_live(&dir, Some(40));
    let board_text = RK3576_BOARD.replace(
        RK3576_UNDRIVEN,
        "vbus = \"gpio:gpio4:11\"\nvbus_off = \"low\"\nvbus_reason = \"fixture: switched by GPIO4 line 11\"\n",
    );
    let listing = "gpiochip4: GPIOs 128-159, parent: platform/fec50000.gpio, gpio4:\n gpio-139 (vcc-otg-en          ) out hi\n";
    let findings = live_check(&dir, &board_text, RK3576_MAP, Some(listing));
    assert!(
        vbus_problems(&findings)
            .iter()
            .any(|p| p.contains("reads hi, but the board says its off level is low")),
        "{findings:?}"
    );
}

#[test]
fn an_unreadable_gpio_listing_is_named_for_a_switched_line() {
    let dir = fixture("no-debugfs");
    rk3576_live(&dir, Some(42));
    let board_text = RK3576_BOARD.replace(
        RK3576_UNDRIVEN,
        "vbus = \"gpio:gpio4:11\"\nvbus_off = \"low\"\nvbus_reason = \"fixture: switched by GPIO4 line 11\"\n",
    );
    let findings = live_check(&dir, &board_text, RK3576_MAP, None);
    assert!(
        vbus_problems(&findings)
            .iter()
            .any(|p| p.contains("cannot read the GPIO levels")),
        "{findings:?}"
    );
}

#[test]
fn an_unlisted_hogged_switched_line_is_named_because_the_hog_did_not_apply() {
    // The overlay hogs a switched line the board holds, so the kernel lists it as requested. A line
    // that is not listed means the overlay is not applied on this boot.
    let dir = fixture("unrequested");
    rk3576_live(&dir, Some(42));
    let board_text = RK3576_BOARD.replace(
        RK3576_UNDRIVEN,
        "vbus = \"gpio:gpio4:11\"\nvbus_off = \"low\"\nvbus_reason = \"fixture: switched by GPIO4 line 11\"\n",
    );
    let listing = "gpiochip4: GPIOs 128-159, parent: platform/fec50000.gpio, gpio4:\n";
    let findings = live_check(&dir, &board_text, RK3576_MAP, Some(listing));
    assert!(
        vbus_problems(&findings).iter().any(|p| p
            .contains("held low by a gpio-hog of the overlay")
            && p.contains("not listed")),
        "{findings:?}"
    );
}

#[test]
fn a_line_the_boot_path_alone_holds_is_a_note_until_a_driver_requests_it() {
    // A device uses the line, so no hog is made: the boot path holds it, and the line is read once
    // the driver requests it. Nothing is listed yet, which is no problem.
    let dir = fixture("boot-only");
    rk3576_live(&dir, Some(42));
    let board_text = format!(
        "{RK3576_BOARD}\n[[lines]]\nline = \"gpio:gpio4:11\"\nboot = \"low\"\nreason = \"fixture: off until the switch takes it\"\n\n[[devices]]\nid = \"switch\"\ndriver = \"gpio-power-switch\"\nuses = {{ line = \"gpio:gpio4:11\" }}\n"
    );
    let listing = "gpiochip4: GPIOs 128-159, parent: platform/fec50000.gpio, gpio4:\n";
    let findings = live_check(&dir, &board_text, RK3576_MAP, Some(listing));
    assert!(
        !findings
            .problems
            .iter()
            .any(|p| p.contains("gpio:gpio4:11")),
        "{findings:?}"
    );
    assert!(
        findings
            .notes
            .iter()
            .any(|n| n.contains("gpio:gpio4:11: boot low by the boot path only")),
        "{findings:?}"
    );
}

#[test]
fn an_enabled_always_on_supply_is_a_note_with_what_the_board_says() {
    let dir = fixture("enabled-regulator");
    rk3576_live(&dir, Some(42));
    let reg = dir.join("regulators").join("regulator.0");
    fs::create_dir_all(&reg).unwrap();
    fs::write(reg.join("name"), "vcc5v0_otg0\n").unwrap();
    fs::write(reg.join("state"), "enabled\n").unwrap();
    fs::write(reg.join("num_users"), "1\n").unwrap();
    let findings = live_check(&dir, RK3576_BOARD, RK3576_MAP, None);
    assert!(
        findings.notes.iter().any(|n| n.contains("always-on regulator \"vcc5v0_otg0\" is enabled (num_users 1); the board says this port is not driven by it")),
        "{findings:?}"
    );
    assert!(vbus_problems(&findings).is_empty(), "{findings:?}");
}

#[test]
fn a_port_whose_board_states_nothing_is_named_by_the_check_too() {
    let dir = fixture("unstated");
    rk3576_live(&dir, Some(42));
    let board_text = RK3576_BOARD.replace(RK3576_UNDRIVEN, "");
    let findings = live_check(&dir, &board_text, RK3576_MAP, None);
    assert!(
        vbus_problems(&findings)
            .iter()
            .any(|p| p.contains("is always on")),
        "{findings:?}"
    );
}

#[test]
fn gpio_listing_is_read_by_its_chip_base_and_line() {
    let dir = fixture("listing");
    let file = dir.join("gpio");
    fs::write(
        &file,
        "gpiochip0: GPIOs 0-31, parent: platform/fd8a0000.gpio, gpio0:\n gpio-11  (a) in hi\ngpiochip4: GPIOs 128-159, parent: platform/fec50000.gpio, can sleep, gpio4:\n gpio-139 (b) out lo\n",
    )
    .unwrap();
    assert_eq!(
        gpio_level(&file, "gpio4", 11).unwrap().as_deref(),
        Some("lo")
    );
    assert_eq!(
        gpio_level(&file, "gpio0", 11).unwrap().as_deref(),
        Some("hi")
    );
    assert_eq!(gpio_level(&file, "gpio4", 12).unwrap(), None);
    assert_eq!(gpio_level(&file, "gpio9", 11).unwrap(), None);
}
