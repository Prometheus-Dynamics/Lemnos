#![allow(clippy::print_stdout)]

#[path = "support/mock_gpio.rs"]
mod mock_gpio_support;

use lemnos::mock::{MockGpioLine, MockHardware};
use lemnos::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 17)
                .with_line_name("status")
                .with_configuration(mock_gpio_support::output_config()),
        )
        .build();

    let mut lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_builtin_drivers()?
        .build();

    lemnos.refresh_with_mock_default(&hardware)?;

    let device_id = lemnos
        .inventory()
        .first_id_by_kind(lemnos::core::DeviceKind::GpioLine)
        .expect("mock GPIO line should be present");

    lemnos.write_gpio(device_id.clone(), GpioLevel::High)?;

    let state = lemnos
        .state(&device_id)
        .expect("runtime should cache GPIO state after the write");
    println!(
        "device {device_id} level = {:?}",
        state.telemetry.get("level")
    );

    Ok(())
}
