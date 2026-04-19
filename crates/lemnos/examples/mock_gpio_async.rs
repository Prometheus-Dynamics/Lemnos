#![allow(clippy::print_stdout)]

#[path = "support/mock_gpio.rs"]
mod mock_gpio_support;

use lemnos::mock::{MockGpioLine, MockHardware};
use lemnos::prelude::*;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 17)
                .with_line_name("status")
                .with_configuration(mock_gpio_support::output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_builtin_drivers()?
        .build_async();

    lemnos.refresh_with_mock_default(&hardware).await?;
    lemnos
        .write_gpio(device_id.clone(), GpioLevel::High)
        .await?;

    let state = lemnos
        .state(&device_id)
        .ok_or("runtime should cache GPIO state after the async write")?;
    println!(
        "device {device_id} level = {:?}",
        state.telemetry.get("level")
    );

    Ok(())
}
