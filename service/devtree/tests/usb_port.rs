//! The A733 USB OTG port as a device port: a board file that gives the gadget the port by its platform
//! device name passes the generator, and one naming a port the map does not have is refused. The map is the
//! SoC's (`socs/sun60i-a733.toml`); the board is written here, not a fixture of the repository.

use lemnos_devtree::{SocMap, generate};
use lemnos_host::board;

const A733_MAP: &str = include_str!("../../../socs/sun60i-a733.toml");

/// A board on the A733 with one device port (the OTG controller), named by `port` in its hardware entry, and the
/// gadget given that port by `udc`.
fn board_text(port: &str, udc: &str) -> String {
    format!(
        r#"format = "lemnos.board"
schema_version = 1

[board]
id = "sun60i-a733-usb"
soc = "sun60i-a733"

[[hardware]]
controller = "usb-otg0"
name = "{port}"
role = "peripheral"
vbus_undriven = true
vbus_reason = "test board: this port's VBUS is not driven by any supply in the SoC's tree (a meter confirms it)"

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

fn generated(port: &str, udc: &str) -> Result<(), String> {
    let board = board::parse(&board_text(port, udc)).map_err(|e| e.to_string())?;
    let map = SocMap::parse(A733_MAP)?;
    generate(&board, &map, &manifests()).map(drop)
}

#[test]
fn a_gadget_on_the_a733_port_by_its_platform_device_name_passes() {
    if let Err(why) = generated("udc:4100000.usb", "udc:4100000.usb") {
        panic!("the A733 port should pass: {why}");
    }
}

#[test]
fn a_gadget_naming_a_port_the_map_does_not_have_is_refused() {
    // The port is named on the hardware entry and by the gadget, and the map has no such port.
    let why = generated("udc:4100001.usb", "udc:4100001.usb")
        .expect_err("a port the map lacks must be refused");
    assert!(
        why.contains("4100001.usb"),
        "the refusal names the port: {why}"
    );
}
