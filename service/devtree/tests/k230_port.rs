//! The K230 USB OTG port in `socs/k230.toml`: a board that gives a gadget the port by its platform device name
//! passes the generator; one naming the wrong UDC is refused by name. A board that boots from an MMC host names
//! it in its system-slots `boot_sources`, which the generator does not read; that list is checked here with the
//! slot store's parser, the one the system-slots driver uses. The map is the SoC's; the boards are written here.

use lemnos_devtree::{SocMap, generate};
use lemnos_host::board;
use lemnos_slot_store::disk::Table;

const K230_MAP: &str = include_str!("../../../socs/k230.toml");

/// A K230 board with the OTG port named `port` on its hardware entry and given to a gadget as `udc`.
fn board_text(port: &str, udc: &str) -> String {
    format!(
        r#"format = "lemnos.board"
schema_version = 1

[board]
id = "k230-usb"
soc = "k230"

[[hardware]]
controller = "usb-otg0"
name = "{port}"
role = "peripheral"
vbus_undriven = true
vbus_reason = "test board: the K230 base describes no VBUS supply for this port, and this fixture drives none"

[[devices]]
id = "gadget"
driver = "usb-gadget"
uses = {{ function = "{udc}" }}
"#
    )
}

fn manifests() -> Vec<lemnos_core::Manifest> {
    lemnosd::builtin::drivers()
        .iter()
        .map(|entry| *entry.manifest())
        .collect()
}

fn generated(port: &str, udc: &str) -> Result<String, String> {
    let board = board::parse(&board_text(port, udc)).map_err(|e| e.to_string())?;
    let map = SocMap::parse(K230_MAP)?;
    generate(&board, &map, &manifests()).map(|out| out.overlay)
}

#[test]
fn a_gadget_on_the_k230_port_by_its_platform_device_name_passes() {
    let overlay = match generated("udc:91500000.usb-otg", "udc:91500000.usb-otg") {
        Ok(overlay) => overlay,
        Err(why) => panic!("the K230 port should pass: {why}"),
    };
    assert!(
        overlay.contains("&{/soc/usb-otg@91500000}"),
        "the port's node is in the overlay: {overlay}"
    );
    assert!(
        overlay.contains("dr_mode = \"peripheral\""),
        "the port is in device mode: {overlay}"
    );
}

#[test]
fn a_gadget_naming_the_k230_port_by_the_wrong_udc_is_refused_by_name() {
    // `udc:91500000.usb` is the name a guess from the node's bus address gives: the driver names the port `91500000.usb-otg`.
    let why = generated("udc:91500000.usb", "udc:91500000.usb")
        .expect_err("a UDC the K230 port does not have must be refused");
    assert!(
        why.contains("91500000.usb") && why.contains("91500000.usb-otg"),
        "the refusal names the wrong and the right UDC: {why}"
    );
}

/// A K230 board with the USB port and a system-slots device whose `boot_sources` names `source` (its path
/// lists the SD slot, the eMMC, or neither).
fn slots_board(source: &str) -> String {
    format!(
        "{}\n[[devices]]\nid = \"slots\"\ndriver = \"system-slots\"\nconfig = {{ data = \"/data\", boot = \"/boot\", record = \"uboot\", boot_args = \"os\", boot_sources = [\"{source}\"] }}\n",
        board_text("udc:91500000.usb-otg", "udc:91500000.usb-otg")
    )
}

#[test]
fn a_board_that_boots_from_the_k230_sd_slot_passes_and_its_boot_source_parses() {
    // `<U-Boot path>=<kernel path>`: U-Boot names the SD host `mmc1`, the kernel `sdhci1`.
    let sd = "/soc/mmc1@91581000=/soc/sdhci1@91581000";
    let board = board::parse(&slots_board(sd)).expect("the board parses");
    let map = SocMap::parse(K230_MAP).expect("the map parses");
    generate(&board, &map, &manifests())
        .expect("a board naming the SD host in boot_sources generates");

    let table = Table::parse(&[sd]).expect("the SD host's boot source is valid");
    assert_eq!(
        table.controller("/soc/mmc1@91581000").ok(),
        Some("/soc/sdhci1@91581000")
    );
}

#[test]
fn a_boot_source_that_is_not_source_equals_path_is_refused_by_the_slot_store() {
    let why =
        Table::parse(&["/soc/sdhci1@91581000"]).expect_err("a source with no path must be refused");
    assert!(why.contains("not source=path"), "{why}");
}
