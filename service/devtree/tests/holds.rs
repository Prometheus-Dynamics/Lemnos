//! The boot holds: the boot fragment, the U-Boot fragment and the overlay's `gpio-hog`s say the same
//! level for each line, and the rule that decides which line is hogged is the one the docs state.
//! Each refusal names the entry it refuses.

use std::collections::BTreeSet;

use lemnos_devtree::map::Kind;
use lemnos_devtree::{SocMap, generate};
use lemnos_host::board::{self, BoardFile};

const RK3588_MAP: &str = include_str!("../../../socs/rk3588.toml");
const RK3588_BOARD: &str = include_str!("../../../testing/boards/rk3588.toml");
const RK3566_MAP: &str = include_str!("../../../socs/rk3566.toml");
const RK3566_BOARD: &str = include_str!("../../../testing/boards/rk3566.toml");
const RK3576_MAP: &str = include_str!("../../../socs/rk3576.toml");
const RK3576_BOARD: &str = include_str!("../../../testing/boards/rk3576.toml");
const REFERENCE_MAP: &str = include_str!("../../../socs/bcm2712-rp1.toml");
const REFERENCE_BOARD: &str = include_str!("../../../testing/boards/reference.toml");
const ALL_KINDS_MAP: &str = include_str!("fixtures/all-kinds.toml");
const ALL_KINDS_BOARD: &str = include_str!("fixtures/all-kinds-board.toml");

/// The manifests of the built-in drivers: what each driver declares it needs.
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
    match generated(board_text, map_text) {
        Ok(_) => panic!("the board was not refused"),
        Err(why) => why,
    }
}

/// The global GPIO numbers and levels the boot fragment holds: `gpio=N=op,dh|dl`.
fn boot_holds(boot: &str) -> BTreeSet<(u32, String)> {
    boot.lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let rest = l.strip_prefix("gpio=").expect("a gpio= line");
            let (number, state) = rest.split_once("=op,").expect("op, state");
            let level = if state == "dh" { "high" } else { "low" };
            (number.parse().unwrap(), level.to_string())
        })
        .collect()
}

/// The lines the U-Boot fragment sets: `gpio set NAME` (high) and `gpio clear NAME` (low), where a
/// Rockchip bank name is its letter (`A` for bank 0) and the line within the bank.
fn uboot_holds(uboot: &str, map: &SocMap) -> BTreeSet<(u32, String)> {
    uboot
        .lines()
        .filter(|l| l.starts_with("gpio "))
        .map(|l| {
            let mut words = l.split_whitespace().skip(1);
            let command = words.next().unwrap();
            let name = words.next().unwrap();
            let letter = name.as_bytes()[0];
            let offset: u32 = name[1..].parse().unwrap();
            let base = u32::from(letter - b'A') * 32;
            let chip = map
                .chips
                .values()
                .find(|c| c.number_base == base)
                .expect("a chip for the bank");
            let level = if command == "set" { "high" } else { "low" };
            (chip.number_base + offset, level.to_string())
        })
        .collect()
}

/// The `gpio-hog`s of the overlay, each as the global number and level: a hog is a child of its chip's
/// node (`target-path`), named `lemnos-hog-N`, and its level is `output-high` or `output-low`.
fn hog_holds(overlay: &str, map: &SocMap) -> BTreeSet<(u32, String)> {
    let mut target = String::new();
    let mut out = BTreeSet::new();
    for line in overlay.lines() {
        let line = line.trim();
        if let Some(path) = line
            .strip_prefix("target-path = \"")
            .and_then(|r| r.strip_suffix("\";"))
        {
            target = path.to_string();
        }
        let Some(rest) = line.split("lemnos-hog-").nth(1) else {
            continue;
        };
        let number: u32 = rest
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let level = if line.contains("output-high") {
            "high"
        } else {
            assert!(line.contains("output-low"), "{line}");
            "low"
        };
        let chip = map
            .chips
            .values()
            .find(|c| c.path.as_deref() == Some(target.as_str()))
            .unwrap_or_else(|| panic!("no chip at {target}"));
        out.insert((chip.number_base + number, level.to_string()));
    }
    out
}

/// Each hold the board makes: its global GPIO number and level, and the names the rule looks at
/// (every device's bus and uses, and the GPIOs of the pin sets the board's hardware takes).
fn expected_hogs(board: &BoardFile, map: &SocMap) -> BTreeSet<(u32, String)> {
    let used: BTreeSet<&str> = board
        .devices
        .iter()
        .filter(|d| !d.disabled)
        .flat_map(|d| d.bus.iter().chain(d.uses.values()))
        .map(String::as_str)
        .collect();
    let mut pinned = BTreeSet::new();
    for hardware in &board.hardware {
        if let Some(set) = &hardware.pins {
            let controller = &map.controllers[&hardware.controller];
            pinned.extend(controller.pins[set].gpios.iter().copied());
        }
    }
    let mut holds: Vec<(String, String)> = board
        .lines
        .iter()
        .filter_map(|l| Some((l.line.clone(), l.boot.clone()?)))
        .collect();
    for hardware in &board.hardware {
        if let (Some(line), Some(off)) = (&hardware.vbus, &hardware.vbus_off) {
            holds.push((line.clone(), off.clone()));
        }
    }
    let mut out = BTreeSet::new();
    for (line, level) in holds {
        if used.contains(line.as_str()) {
            continue;
        }
        let (label, number) = line
            .strip_prefix("gpio:")
            .and_then(|r| r.rsplit_once(':'))
            .unwrap();
        let chip = map.chip_labelled(label).unwrap();
        let gpio = chip.number_base + number.parse::<u32>().unwrap();
        if u16::try_from(gpio).is_ok_and(|g| pinned.contains(&g)) {
            continue;
        }
        out.insert((gpio, level));
    }
    out
}

/// Every family and fixture that holds a line: the boot fragment, the U-Boot fragment and the
/// overlay's hogs agree line for line, and the hogs are exactly the holds the rule takes.
#[test]
fn the_boot_fragment_the_u_boot_fragment_and_the_hogs_agree() {
    let cases = [
        ("rk3566", RK3566_MAP, RK3566_BOARD),
        ("rk3588", RK3588_MAP, RK3588_BOARD),
        ("rk3576", RK3576_MAP, RK3576_BOARD),
        ("reference", REFERENCE_MAP, REFERENCE_BOARD),
        ("all-kinds", ALL_KINDS_MAP, ALL_KINDS_BOARD),
    ];
    let mut compared_uboot = 0usize;
    for (name, map_text, board_text) in cases {
        let board = board::parse(board_text).unwrap();
        let map = SocMap::parse(map_text).unwrap();
        let out = generated(board_text, map_text).unwrap_or_else(|e| panic!("{name}: {e}"));
        let boot = boot_holds(&out.boot);
        let hogs = hog_holds(&out.overlay, &map);
        assert_eq!(hogs, expected_hogs(&board, &map), "{name}: the hogs");
        assert!(
            hogs.is_subset(&boot),
            "{name}: every hog is a boot hold at the same level"
        );
        match &out.uboot {
            Ok(text) => {
                assert_eq!(uboot_holds(text, &map), boot, "{name}: the U-Boot fragment");
                compared_uboot += 1;
            }
            Err(why) => assert!(
                !boot.is_empty(),
                "{name}: no U-Boot naming is refused only when a line is held: {why}"
            ),
        }
    }
    assert_eq!(
        compared_uboot, 3,
        "the three Rockchip maps compare (rk3576 holds nothing)"
    );
}

#[test]
fn a_hold_a_device_uses_is_not_hogged_and_the_boot_path_holds_it() {
    // The RK3566's power-switch line is held low from power-on and then driven by the device.
    let out = generated(RK3566_BOARD, RK3566_MAP).unwrap();
    assert!(!out.overlay.contains("gpio-hog"), "{}", out.overlay);
    assert!(out.boot.contains("gpio=36=op,dl"), "{}", out.boot);
    assert_eq!(out.uboot.unwrap().lines().last(), Some("gpio clear B4"));
}

#[test]
fn a_hold_on_a_pin_set_pad_is_not_hogged_so_the_reference_board_is_unchanged() {
    // The reference board's fan line is the PWM output's pad: the boot fragment holds it, and the
    // PWM's pin set takes it at the driver's probe. No hog is made, and nothing else changes.
    let out = generated(REFERENCE_BOARD, REFERENCE_MAP).unwrap();
    assert!(!out.overlay.contains("gpio-hog"), "{}", out.overlay);
    assert!(out.boot.contains("gpio=45=op,dh"), "{}", out.boot);
}

#[test]
fn a_pure_hold_is_hogged_at_its_level_on_its_chip() {
    let out = generated(RK3588_BOARD, RK3588_MAP).unwrap();
    assert!(
        out.overlay
            .contains("target-path = \"/pinctrl/gpio@fec50000\";"),
        "{}",
        out.overlay
    );
    assert!(
        out.overlay
            .contains("lemnos-hog-11 { gpio-hog; gpios = <11 0>; output-low;"),
        "{}",
        out.overlay
    );
    assert_eq!(out.uboot.unwrap().lines().last(), Some("gpio clear E11"));
}

#[test]
fn a_hold_on_a_chip_with_no_node_is_refused_by_name() {
    // A map whose chip has no node cannot hog a line the boot path holds, and says so by name.
    let map = ALL_KINDS_MAP.replace("path = \"/bus/gpio@9000\"", "");
    let why = refused(ALL_KINDS_BOARD, &map);
    assert!(
        why.contains("\"gpio:gpio-test:9\"") && why.contains("has no `path` in the test-soc map"),
        "{why}"
    );
}

#[test]
fn a_vbus_line_a_device_uses_is_refused_by_name() {
    // The line must free something for the device's `[[lines]]` entry to be accepted: here the base
    // holds it, so the entry makes a node disabled, and the device uses the line at run time.
    let map = format!(
        "{RK3588_MAP}\n[bases.usb-gpio-vbus.lines.\"gpio:gpio4:11\"]\nholders = [\"/vcc5v0-otg-vbus\"]\nreason = \"fixture: the base's regulator enable\"\n"
    );
    let board = format!(
        "{RK3588_BOARD}\n[[lines]]\nline = \"gpio:gpio4:11\"\n\n[[devices]]\nid = \"switch\"\ndriver = \"gpio-power-switch\"\nuses = {{ line = \"gpio:gpio4:11\" }}\n"
    );
    let why = refused(&board, &map);
    assert!(
        why.contains("VBUS line gpio:gpio4:11 is also used by device \"switch\""),
        "{why}"
    );
}

#[test]
fn a_line_with_two_boot_states_is_refused_by_name() {
    let board = format!(
        "{RK3566_BOARD}\n[[lines]]\nline = \"gpio:gpio1:4\"\nboot = \"high\"\nreason = \"a second entry\"\n"
    );
    let why = refused(&board, RK3566_MAP);
    assert!(
        why.contains("[[lines]] \"gpio:gpio1:4\" has a boot state twice"),
        "{why}"
    );
}

#[test]
fn the_u_boot_fragment_is_refused_by_name_where_the_family_has_no_naming() {
    // The RP1 has no U-Boot driver: the reference board's fan hold is written by the boot fragment
    // only, and the U-Boot fragment says so instead of writing a line U-Boot cannot apply.
    let out = generated(REFERENCE_BOARD, REFERENCE_MAP).unwrap();
    let why = out.uboot.unwrap_err();
    assert!(
        why.contains("the bcm2712-rp1 map gives no U-Boot GPIO naming")
            && why.contains("gpio:pinctrl-rp1:45"),
        "{why}"
    );
}

#[test]
fn a_map_names_only_a_u_boot_scheme_it_knows() {
    let map = RK3588_MAP.replace("uboot = \"rockchip-bank\"", "uboot = \"sunxi-port\"");
    let why = SocMap::parse(&map).unwrap_err();
    assert!(
        why.contains("\"sunxi-port\" is not a U-Boot GPIO naming"),
        "{why}"
    );
}

#[test]
fn a_chip_node_path_must_be_absolute() {
    let map = RK3588_MAP.replace(
        "path = \"/pinctrl/gpio@fec50000\"",
        "path = \"pinctrl/gpio@fec50000\"",
    );
    let why = SocMap::parse(&map).unwrap_err();
    assert!(
        why.contains("chip \"gpio4\": its path must be absolute"),
        "{why}"
    );
}

#[test]
fn a_map_controller_kind_is_still_checked_for_hogs() {
    // A hog never takes a controller's node: the chips and the controllers are separate tables.
    let map = SocMap::parse(RK3588_MAP).unwrap();
    assert!(
        map.controllers
            .values()
            .all(|c| c.kind != Kind::Watchdog || c.path.starts_with('/'))
    );
}
