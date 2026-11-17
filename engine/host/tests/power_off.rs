//! The power switch through a host: an off request drives its line low and holds it, and an on
//! request drives it high again on the same line, with no rebind. Fails on the host code before
//! the off path kept its driver: the drop released the line and it kept its high level.

use std::sync::Arc;

use lemnos_driver_gpio_power_switch::GpioPowerSwitch;
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, OffReason, Options, entry};
use lemnos_sim::SimBoard;

const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
[[devices]]\nid = \"usb-a-power\"\ndriver = \"gpio-power-switch\"\nuses = { line = \"power\" }\n";

#[test]
fn off_drives_the_line_low_and_holds_it_and_on_drives_it_high() {
    let sim = SimBoard::new();
    let line = sim.add_line("power");
    let clock = sim.clock();
    let host = Host::start(
        &parse(BOARD).unwrap(),
        &[entry::<GpioPowerSwitch>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap();
    assert_eq!(
        host.device("usb-a-power").unwrap().state,
        DeviceState::Running
    );
    assert_eq!(line.level(), Some(true), "bound at its default: on");
    let configs = line.configs().len();

    host.off("usb-a-power").unwrap();
    assert_eq!(
        host.device("usb-a-power").unwrap().state,
        DeviceState::Off(OffReason::Manual)
    );
    assert_eq!(line.level(), Some(false), "off drives the line low");
    assert_eq!(
        line.configs().len(),
        configs,
        "held: the line is not released or rebound"
    );

    host.on("usb-a-power").unwrap();
    assert_eq!(
        host.device("usb-a-power").unwrap().state,
        DeviceState::Running
    );
    assert_eq!(line.level(), Some(true));
    assert_eq!(
        line.configs().len(),
        configs,
        "on uses the held line, no rebind"
    );
    host.shutdown();
}
