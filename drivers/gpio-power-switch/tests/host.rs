//! The switch on a host, from the board file a person writes: its one line as `bus`, and no
//! fault line. The optional `fault` need is named nowhere, so it is absent, and `bus` is not
//! handed to it as well.

use std::sync::Arc;

use lemnos_core::MethodId;
use lemnos_driver_gpio_power_switch::GpioPowerSwitch;
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::SimBoard;

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1
[board]
id = "switch-test"

[[devices]]
id = "port-power"
driver = "gpio-power-switch"
bus = "gpiochip0:16"
config = { default_on = false }
"#;

#[test]
fn a_bus_and_an_unnamed_optional_need_bind_with_the_optional_need_absent() {
    let sim = SimBoard::new();
    let clock = sim.clock();
    let line = sim.add_line("gpiochip0:16");
    let host = Host::start(
        &parse(BOARD).unwrap(),
        &[entry::<GpioPowerSwitch>()],
        Box::new(sim.clone()),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap();

    let info = host.device("port-power").unwrap();
    assert_eq!(info.state, DeviceState::Running, "{info:?}");
    // The bus is the switched line, an output, and nothing reconfigured it as the fault input.
    assert_eq!(line.level(), Some(false));
    assert_eq!(line.configs().len(), 1);

    // `set(true)` drives it.
    let mut reply = [0u8; 16];
    host.call(
        info.index,
        MethodId {
            contract: 0,
            method: 1,
        },
        &[1],
        &mut reply,
    )
    .unwrap();
    assert_eq!(line.level(), Some(true));
}
