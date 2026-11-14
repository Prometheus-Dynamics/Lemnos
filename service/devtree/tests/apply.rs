//! The generated overlay, applied to a base tree, by the loaders that exist: libfdt's `fdtoverlay`
//! (the tool the Raspberry Pi firmware and U-Boot's `fdt apply` also use, as libfdt overlay
//! application), and the form a board's own `.dts` takes, compiled by `dtc` with the base. Each
//! must leave the device port's PHY without a VBUS supply: its `phy-supply` names the stand-in.
//!
//! `dtc` and `fdtoverlay`, `fdtget` are needed. Where they are not installed the test says so and
//! skips, except in CI (the `CI` variable is set there), where a missing tool fails the test.

#![allow(clippy::print_stderr)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use lemnos_devtree::map::Form;
use lemnos_devtree::{SocMap, generate};
use lemnos_host::board;

const RK3588_MAP: &str = include_str!("../../../socs/rk3588.toml");
const BASE: &str = include_str!("fixtures/rk3588-usb-base.dts");

/// The smallest board that names the RK3588 device port: the port, its gadget, and its VBUS line.
const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n\n[board]\nid = \"rk3588-device-port\"\nsoc = \"rk3588\"\nbase = \"usb-gpio-vbus\"\n\n[[hardware]]\nname = \"udc:fc000000.usb\"\ncontroller = \"usb-otg0\"\nrole = \"peripheral\"\noptions = [\"usb3\"]\nvbus = \"gpio:gpio4:11\"\nvbus_off = \"low\"\nvbus_reason = \"the OTG port's 5 V regulator is enabled by GPIO4 line 11; low is off\"\n\n[[devices]]\nid = \"gadget\"\ndriver = \"usb-gadget\"\nuses = { function = \"udc:fc000000.usb\" }\n";

/// Whether a tool runs. Missing outside CI: skipped with a message. Missing in CI: a failure.
fn tool(name: &str) -> bool {
    let found = Command::new(name).arg("--version").output().is_ok();
    if !found {
        if std::env::var_os("CI").is_some() {
            panic!(
                "{name} is not installed, and CI must run this test: install device-tree-compiler"
            );
        }
        eprintln!(
            "skipped: {name} is not installed (device-tree-compiler provides dtc and fdtoverlay)"
        );
    }
    found
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("apply-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// `contracts/overlay-dtc.toml` (version 1): the flags and the error classes an overlay is compiled
/// with. The consumer's overlay step reads the same file, so both use one set.
const OVERLAY_DTC: &str = include_str!("../../../contracts/overlay-dtc.toml");

/// The compiler the overlay tests run, or `None` when they must skip. `LEMNOS_DTC` names the release
/// the contract is for (testing/devtree/check.sh builds it and sets the variable, and the gate runs
/// that script). Without it, the `dtc` on PATH is used only if it knows every class the contract
/// lists: dtc 1.8 has no `graph_child_address`, and a skip there is said out loud, not silent.
fn overlay_dtc() -> Option<String> {
    if let Ok(program) = std::env::var("LEMNOS_DTC") {
        return Some(program);
    }
    let probe = Command::new("dtc")
        .args(overlay_flags())
        .args(["-o", "/dev/null", "/dev/null"])
        .output()
        .ok()?;
    if String::from_utf8_lossy(&probe.stderr).contains("Unrecognized check name") {
        eprintln!(
            "skipped: the dtc on PATH is not the release contracts/overlay-dtc.toml names; run testing/devtree/check.sh"
        );
        return None;
    }
    Some("dtc".into())
}

/// The flags of the contract, then `-E <class>` for each error class, as the contract lists them.
fn overlay_flags() -> Vec<String> {
    let table: toml::Table = OVERLAY_DTC
        .parse()
        .expect("contracts/overlay-dtc.toml parses");
    assert_eq!(table["version"].as_integer(), Some(1));
    let strings = |key: &str| -> Vec<String> {
        table[key]
            .as_array()
            .unwrap_or_else(|| panic!("{key} is an array"))
            .iter()
            .map(|v| v.as_str().expect("a string").to_string())
            .collect()
    };
    let mut flags = strings("flags");
    for class in strings("error_classes") {
        flags.push("-E".into());
        flags.push(class);
    }
    flags
}

/// Compiles a generated overlay's source to `output` with the contract's flags. The compile must
/// print nothing: every warning class is an error, so a warning is a defect in the overlay text.
/// (The base trees the tests build from the SoC maps are fixtures, and dtc may warn about them.)
fn compile_overlay(source: &Path, output: &Path) {
    let mut args = overlay_flags();
    args.push("-o".into());
    args.push(output.to_str().unwrap().into());
    args.push(source.to_str().unwrap().into());
    let program = overlay_dtc().expect("compile_overlay runs only where overlay_dtc() is set");
    let out = Command::new(&program)
        .args(&args)
        .output()
        .unwrap_or_else(|e| panic!("{program}: {e}"));
    assert!(
        out.status.success(),
        "dtc {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "dtc {args:?} printed diagnostics on the generated overlay, which are errors here:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run(program: &str, args: &[&str]) -> String {
    let out = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{program}: {e}"));
    assert!(
        out.status.success(),
        "{program} {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// One property of a node, as the tool prints it (a cell as a decimal number).
fn cell(dtb: &Path, node: &str, prop: &str) -> String {
    run("fdtget", &["-t", "u", dtb.to_str().unwrap(), node, prop])
}

/// The manifests of the drivers the tool knows, as the generator checks a device's declared needs.
fn manifests() -> Vec<lemnos_core::Manifest> {
    lemnosd::builtin::drivers()
        .iter()
        .map(|entry| *entry.manifest())
        .collect()
}

fn overlay_text() -> String {
    let board = board::parse(BOARD).unwrap();
    let map = SocMap::parse(RK3588_MAP).unwrap();
    generate(&board, &map, &manifests()).unwrap().overlay
}

#[test]
fn fdtoverlay_leaves_the_port_with_the_stand_in_and_no_vbus_supply() {
    let Some(dtc) = overlay_dtc() else { return };
    if !tool(&dtc) || !tool("fdtoverlay") || !tool("fdtget") {
        return;
    }
    let dir = scratch("fdtoverlay");
    let base = dir.join("base.dts");
    let base_dtb = dir.join("base.dtb");
    fs::write(&base, BASE).unwrap();
    run(
        "dtc",
        &[
            "-@",
            "-I",
            "dts",
            "-O",
            "dtb",
            "-o",
            base_dtb.to_str().unwrap(),
            base.to_str().unwrap(),
        ],
    );

    // Before: the base's OTG port names the regulator, which is a host 5 V supply.
    let regulator = cell(&base_dtb, "/vcc5v0-otg-vbus", "phandle");
    assert_eq!(
        cell(
            &base_dtb,
            "/syscon@fd5d0000/usb2phy@0/otg-port",
            "phy-supply"
        ),
        regulator
    );

    let overlay = dir.join("overlay.dts");
    let overlay_dtbo = dir.join("overlay.dtbo");
    fs::write(&overlay, overlay_text()).unwrap();
    compile_overlay(&overlay, &overlay_dtbo);

    let applied = dir.join("applied.dtb");
    run(
        "fdtoverlay",
        &[
            "-i",
            base_dtb.to_str().unwrap(),
            "-o",
            applied.to_str().unwrap(),
            overlay_dtbo.to_str().unwrap(),
        ],
    );

    // After: the PHY's supply is the stand-in, and the base regulator is still there, unused by it.
    let stand_in = cell(&applied, "/regulator-lemnos-no-vbus", "phandle");
    assert_eq!(
        cell(
            &applied,
            "/syscon@fd5d0000/usb2phy@0/otg-port",
            "phy-supply"
        ),
        stand_in
    );
    assert_ne!(stand_in, regulator);
    assert_eq!(cell(&applied, "/vcc5v0-otg-vbus", "phandle"), regulator);
    assert_eq!(
        run(
            "fdtget",
            &[
                "-t",
                "s",
                applied.to_str().unwrap(),
                "/regulator-lemnos-no-vbus",
                "compatible"
            ]
        ),
        "regulator-fixed"
    );
    assert_eq!(
        run(
            "fdtget",
            &[
                "-t",
                "s",
                applied.to_str().unwrap(),
                "/usb@fc000000",
                "dr_mode"
            ]
        ),
        "peripheral"
    );
    assert_eq!(
        run(
            "fdtget",
            &[
                "-t",
                "s",
                applied.to_str().unwrap(),
                "/syscon@fd5d0000/usb2phy@0/otg-port",
                "status"
            ]
        ),
        "okay"
    );
}

#[test]
fn the_fragment_form_leaves_the_port_with_the_stand_in_when_compiled_with_the_base() {
    let Some(dtc) = overlay_dtc() else { return };
    if !tool(&dtc) || !tool("fdtget") {
        return;
    }
    let dir = scratch("fragment");
    let board = board::parse(BOARD).unwrap();
    let map =
        SocMap::parse(&RK3588_MAP.replace("form = \"overlay\"", "form = \"fragment\"")).unwrap();
    let fragments = generate(&board, &map, &manifests()).unwrap().overlay;
    assert!(!fragments.contains("/plugin/"), "{fragments}");

    // The board's own `.dts`: the base, then the fragments the generator writes.
    let both = dir.join("board.dts");
    fs::write(&both, format!("{BASE}\n{fragments}")).unwrap();
    let dtb = dir.join("board.dtb");
    run(
        "dtc",
        &[
            "-@",
            "-I",
            "dts",
            "-O",
            "dtb",
            "-o",
            dtb.to_str().unwrap(),
            both.to_str().unwrap(),
        ],
    );

    let stand_in = cell(&dtb, "/regulator-lemnos-no-vbus", "phandle");
    assert_eq!(
        cell(&dtb, "/syscon@fd5d0000/usb2phy@0/otg-port", "phy-supply"),
        stand_in
    );
    assert_eq!(
        run(
            "fdtget",
            &["-t", "s", dtb.to_str().unwrap(), "/usb@fc000000", "dr_mode"]
        ),
        "peripheral"
    );
}

// Every board fixture, with a base tree built from its SoC map.
//
// The base is what the map relies on and nothing more: each controller, needed node and pin set at
// the path the map gives, with the label the map gives it (`pinctrl-0 = <&LABEL>`, `phys = <&LABEL>`),
// each disabled as a base tree has them, and the lines' holders and VBUS regulators. It is compiled
// with `dtc -@`, so every label is a symbol, as a real base must be. Each board's overlay is compiled
// with `dtc -@` and applied with `fdtoverlay`; a fragment-form output is merged into the base by
// `dtc`. Then the result is read back: every controller the board enables is `okay`, and each device
// port's PHY names the stand-in supply. A real board's base is checked in its board-run note; this
// test checks the mechanics on every fixture, in CI.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use lemnos_devtree::map::Kind;

#[derive(Default)]
struct Node {
    label: Option<String>,
    /// Enabled in the base, as a base tree has a live regulator or a live GPIO bank: a node the overlay
    /// disables is then seen to be disabled, not merely absent of a status.
    okay: bool,
    children: BTreeMap<String, Node>,
}

fn insert(root: &mut Node, path: &str, label: Option<&str>) {
    let mut node = root;
    for part in path.split('/').filter(|p| !p.is_empty()) {
        node = node.children.entry(part.to_string()).or_default();
    }
    if let Some(label) = label {
        node.label = Some(label.to_string());
    }
}

/// A node the base has enabled, at `path`.
fn insert_enabled(root: &mut Node, path: &str) {
    let mut node = root;
    for part in path.split('/').filter(|p| !p.is_empty()) {
        node = node.children.entry(part.to_string()).or_default();
    }
    node.okay = true;
}

fn write_node(node: &Node, name: &str, depth: usize, out: &mut String) {
    let pad = "\t".repeat(depth);
    let label = node
        .label
        .as_deref()
        .map(|l| format!("{l}: "))
        .unwrap_or_default();
    let _ = writeln!(out, "{pad}{label}{name} {{");
    let status = if node.okay { "okay" } else { "disabled" };
    let _ = writeln!(out, "{pad}\tstatus = \"{status}\";");
    for (child, child_node) in &node.children {
        write_node(child_node, child, depth + 1, out);
    }
    let _ = writeln!(out, "{pad}}};");
}

/// The base tree a map relies on: its nodes at the map's paths, labelled as the map names them.
fn base_from_map(map: &SocMap) -> String {
    let mut root = Node::default();
    // Every base tree has `/aliases`: the overlay's runtime names are written into it.
    insert(&mut root, "/aliases", None);
    for controller in map.controllers.values() {
        insert(&mut root, &controller.path, None);
        for need in &controller.needs {
            insert(&mut root, &need.path, need.label.as_deref());
        }
        for set in controller.pins.values() {
            insert(&mut root, &set.path, Some(&set.label));
        }
    }
    for chip in map.chips.values() {
        if let Some(path) = &chip.path {
            insert_enabled(&mut root, path);
        }
    }
    for base in map.bases.values() {
        for holders in base.lines.values() {
            for holder in &holders.holders {
                insert(&mut root, holder, None);
            }
        }
        for vbus in base.vbus.values() {
            if let Some(regulator) = &vbus.regulator {
                insert_enabled(&mut root, regulator);
            }
        }
    }
    let mut out = String::from("/dts-v1/;\n/ {\n\t#address-cells = <1>;\n\t#size-cells = <1>;\n");
    for (name, node) in &root.children {
        write_node(node, name, 1, &mut out);
    }
    out.push_str("};\n");
    out
}

/// A property of a node in a compiled tree, or `None` when the tool finds none.
fn prop(tool_dtb: &Path, node: &str, name: &str, kind: &str) -> Option<String> {
    let out = Command::new("fdtget")
        .args(["-t", kind, tool_dtb.to_str().unwrap(), node, name])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The properties a node has in a compiled tree, or `None` when the node is not there.
fn node_props(tree: &Path, node: &str) -> Option<Vec<String>> {
    let out = Command::new("fdtget")
        .args(["-p", tree.to_str().unwrap(), node])
        .output()
        .ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .collect()
    })
}

/// Each hold the board makes, as the line and its level: a `[[lines]]` boot state, and each device
/// port's VBUS line at its off level.
fn holds_of(board: &lemnos_host::board::BoardFile) -> Vec<(String, String)> {
    let mut holds: Vec<(String, String)> = board
        .lines
        .iter()
        .filter_map(|l| Some((l.line.clone(), l.boot.clone()?)))
        .collect();
    for entry in &board.hardware {
        if let (Some(line), Some(off)) = (&entry.vbus, &entry.vbus_off) {
            holds.push((line.clone(), off.clone()));
        }
    }
    holds
}

fn board_fixtures() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testing/boards");
    let mut boards: Vec<(String, String)> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .map(|p| (p.display().to_string(), fs::read_to_string(&p).unwrap()))
        .collect();
    boards.sort();
    boards
}

/// The gate's flags are the contract's: the compile flags first, then every class as an error.
#[test]
fn the_gate_compiles_with_the_contracts_flags_and_every_class_an_error() {
    let flags = overlay_flags();
    assert_eq!(&flags[..5], ["-@", "-I", "dts", "-O", "dtb"], "{flags:?}");
    let table: toml::Table = OVERLAY_DTC.parse().unwrap();
    let classes = table["error_classes"].as_array().unwrap();
    assert!(classes.len() >= 50, "dtc 1.7.2 has 56 warning classes");
    assert!(
        flags.windows(2).any(|w| w == ["-E", "alias_paths"]),
        "{flags:?}"
    );
    assert_eq!(flags.len(), 5 + 2 * classes.len());
}

/// The defect the alias form fixes: an `aliases` node inside a fragment whose target is the root
/// is a warning (alias_paths) under the contract's flags, so the gate refuses it. Compiled here
/// with the same flags the gate uses, so a dtc that stops reporting it fails this test.
#[test]
fn the_gate_refuses_the_pre_fix_alias_form() {
    let Some(dtc) = overlay_dtc() else { return };
    if !tool(&dtc) {
        return;
    }
    let dir = scratch("pre-fix-alias");
    let source = dir.join("pre-fix.dts");
    fs::write(
        &source,
        "/dts-v1/;\n/plugin/;\n/ {\n\tfragment@0 {\n\t\ttarget-path = \"/\";\n\t\t__overlay__ {\n\t\t\taliases { i2c1 = \"/soc/i2c@1000\"; };\n\t\t};\n\t};\n};\n",
    )
    .unwrap();
    let mut args = overlay_flags();
    args.push("-o".into());
    args.push(dir.join("pre-fix.dtbo").to_str().unwrap().into());
    args.push(source.to_str().unwrap().into());
    let out = Command::new(dtc).args(&args).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("alias_paths"),
        "the pre-fix form must be refused by alias_paths: exit {:?}\n{stderr}",
        out.status.code()
    );
}

/// The reference board's overlay sets its two I2C aliases under `/aliases`: the form that compiles
/// with no alias_paths warning. After fdtoverlay onto a base built from the RP1 map, the aliases
/// read back as the controllers' paths (a warning-free form that set no alias would fail here).
#[test]
fn the_fixed_alias_form_sets_the_board_aliases_after_fdtoverlay() {
    let Some(dtc) = overlay_dtc() else { return };
    if !tool(&dtc) || !tool("fdtoverlay") || !tool("fdtget") {
        return;
    }
    let board = board::parse(include_str!("../../../testing/boards/reference.toml")).unwrap();
    let map = SocMap::parse(include_str!("../../../socs/bcm2712-rp1.toml")).unwrap();
    let out = generate(&board, &map, &manifests()).unwrap();
    let dir = scratch("aliases");
    let base_dts = dir.join("base.dts");
    let base_dtb = dir.join("base.dtb");
    fs::write(&base_dts, base_from_map(&map)).unwrap();
    run(
        &dtc,
        &[
            "-@",
            "-I",
            "dts",
            "-O",
            "dtb",
            "-o",
            base_dtb.to_str().unwrap(),
            base_dts.to_str().unwrap(),
        ],
    );
    let overlay = dir.join("overlay.dts");
    let overlay_dtbo = dir.join("overlay.dtbo");
    fs::write(&overlay, &out.overlay).unwrap();
    compile_overlay(&overlay, &overlay_dtbo);
    let result = dir.join("result.dtb");
    run(
        "fdtoverlay",
        &[
            "-i",
            base_dtb.to_str().unwrap(),
            "-o",
            result.to_str().unwrap(),
            overlay_dtbo.to_str().unwrap(),
        ],
    );
    for (alias, path) in [
        ("i2c1", "/axi/pcie@1000120000/rp1/i2c@74000"),
        ("i2c10", "/axi/pcie@1000120000/rp1/i2c@88000"),
    ] {
        let read = run(
            "fdtget",
            &["-t", "s", result.to_str().unwrap(), "/aliases", alias],
        );
        assert_eq!(read.trim(), path, "the /aliases/{alias} read-back");
    }
}

#[test]
fn every_fixture_overlay_applies_to_a_base_built_from_its_map() {
    let Some(dtc) = overlay_dtc() else { return };
    if !tool(&dtc) || !tool("fdtoverlay") || !tool("fdtget") {
        return;
    }
    let socs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../socs");
    let mut applied = 0usize;
    for (path, text) in board_fixtures() {
        let board = board::parse(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
        let Some(soc) = board.board.soc.clone() else {
            continue;
        };
        let map_text = fs::read_to_string(socs.join(format!("{soc}.toml")))
            .unwrap_or_else(|e| panic!("{path}: the map for {soc:?}: {e}"));
        let map = SocMap::parse(&map_text).unwrap();
        let out = generate(&board, &map, &manifests()).unwrap_or_else(|e| panic!("{path}: {e}"));
        let name = Path::new(&path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let dir = scratch(&format!("every-{name}"));
        let base_dts = dir.join("base.dts");
        let base_dtb = dir.join("base.dtb");
        fs::write(&base_dts, base_from_map(&map)).unwrap();
        run(
            "dtc",
            &[
                "-@",
                "-I",
                "dts",
                "-O",
                "dtb",
                "-o",
                base_dtb.to_str().unwrap(),
                base_dts.to_str().unwrap(),
            ],
        );

        let result = dir.join("result.dtb");
        if map.form == Form::Overlay {
            let overlay = dir.join("overlay.dts");
            let overlay_dtbo = dir.join("overlay.dtbo");
            fs::write(&overlay, &out.overlay).unwrap();
            compile_overlay(&overlay, &overlay_dtbo);
            run(
                "fdtoverlay",
                &[
                    "-i",
                    base_dtb.to_str().unwrap(),
                    "-o",
                    result.to_str().unwrap(),
                    overlay_dtbo.to_str().unwrap(),
                ],
            );
        } else {
            // A fragment is merged into the board's own .dts, with the base it names.
            let both = dir.join("board.dts");
            fs::write(&both, format!("{}\n{}", base_from_map(&map), out.overlay)).unwrap();
            run(
                "dtc",
                &[
                    "-@",
                    "-I",
                    "dts",
                    "-O",
                    "dtb",
                    "-o",
                    result.to_str().unwrap(),
                    both.to_str().unwrap(),
                ],
            );
        }
        let result = result.as_path();

        // Read back: each controller the board names is enabled, and each device port's PHY names
        // the stand-in supply, which the overlay adds (a phandle, so the stand-in must be a node).
        let stand_in = prop(result, "/regulator-lemnos-no-vbus", "phandle", "u");
        for entry in &board.hardware {
            let controller = &map.controllers[&entry.controller];
            let status = prop(result, &controller.path, "status", "s");
            assert_eq!(
                status.as_deref(),
                Some("okay"),
                "{path}: {} {}",
                entry.controller,
                controller.path
            );
            // A port whose map names a USB 2 PHY gets the stand-in on it; a port with no PHY node in
            // its map (the reference SoC's) has no supply in the overlay to replace.
            let phy = controller
                .needs
                .iter()
                .find(|n| n.phy.as_deref() == Some("usb2-phy"));
            if let (Kind::UsbPeripheral, Some(phy)) = (controller.kind, phy) {
                assert_eq!(
                    prop(result, &phy.path, "phy-supply", "u"),
                    stand_in,
                    "{path}: the PHY of {} names the stand-in",
                    entry.controller
                );
            }
        }
        // Each hold the overlay hogs is a `gpio-hog` child of its chip, with its cells and its level;
        // a hold the boot path alone holds has no hog node. A hogged VBUS port's base regulator is
        // disabled, so it cannot own the line.
        let hogged = lemnos_devtree::generate::hogged_lines(&board, &map).unwrap();
        for (line, level) in holds_of(&board) {
            let name = line.strip_prefix("gpio:").unwrap();
            let (label, number) = name.rsplit_once(':').unwrap();
            let chip = map.chip_labelled(label).unwrap();
            let node = format!("{}/lemnos-hog-{number}", chip.path.as_deref().unwrap());
            if hogged.contains(&line) {
                let props = node_props(result, &node)
                    .unwrap_or_else(|| panic!("{path}: no hog node {node} for {line}"));
                assert!(props.iter().any(|p| p == "gpio-hog"), "{path}: {node}");
                assert!(
                    props.iter().any(|p| p == &format!("output-{level}")),
                    "{path}: {node} holds {line} {level}, props {props:?}"
                );
                let cells = prop(result, &node, "gpios", "u");
                assert_eq!(
                    cells.as_deref(),
                    Some(format!("{number} 0").as_str()),
                    "{path}: {node}"
                );
            } else {
                assert!(
                    node_props(result, &node).is_none(),
                    "{path}: {node} is not hogged"
                );
            }
        }
        for entry in &board.hardware {
            if let (Some(_), Some(_)) = (&entry.vbus, &entry.vbus_off) {
                let controller = &map.controllers[&entry.controller];
                let base_name = board.board.base.as_deref().unwrap();
                let fact = map.bases[base_name].vbus.get(&entry.controller);
                if let Some(regulator) = fact.and_then(|f| f.regulator.as_deref()) {
                    assert_eq!(
                        run(
                            "fdtget",
                            &["-t", "s", result.to_str().unwrap(), regulator, "status"]
                        ),
                        "disabled",
                        "{path}: the VBUS regulator of {} is disabled",
                        controller.path
                    );
                }
            }
        }
        applied += 1;
    }
    assert!(
        applied >= 4,
        "only {applied} fixtures have a map and were applied"
    );
}
