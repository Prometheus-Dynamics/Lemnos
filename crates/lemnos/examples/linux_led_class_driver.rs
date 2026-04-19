#![allow(clippy::print_stdout)]

#[path = "support/linux_led.rs"]
mod linux_led;
#[path = "support/linux_response.rs"]
mod linux_response;
#[path = "support/linux_test_root.rs"]
mod linux_test_root;

use lemnos::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = linux_led::LinuxLedTestRoot::new();
    root.create_led("ACT", 1, 255, "none [default-on] heartbeat\n", "leds-gpio");

    let backend = lemnos::linux::LinuxBackend::with_paths(root.paths());
    let mut lemnos = Lemnos::builder()
        .with_linux_backend_ref(&backend)
        .with_driver(linux_led::ExampleLinuxLedDriver)?
        .build();

    lemnos.refresh_with_linux_default(&backend)?;
    let device_id = linux_led::led_device_id(&lemnos)?;
    lemnos.bind(&device_id)?;

    let read = lemnos.request_custom(device_id.clone(), linux_led::LED_READ_INTERACTION)?;
    let on = lemnos.request_custom(device_id.clone(), linux_led::LED_ON_INTERACTION)?;
    let trigger = lemnos.request_custom_value(
        device_id.clone(),
        linux_led::LED_SET_TRIGGER_INTERACTION,
        "heartbeat",
    )?;
    let state = lemnos
        .refresh_state(&device_id)?
        .cloned()
        .ok_or("missing LED state")?;

    println!("discovered LED: {device_id}");
    println!(
        "read brightness={:?} active_trigger={:?}",
        linux_response::custom_output_field(&read, "brightness")
            .and_then(lemnos::core::Value::as_u64),
        linux_response::custom_output_field(&read, "active_trigger")
            .and_then(lemnos::core::Value::as_str)
    );
    println!(
        "after on brightness={:?}",
        linux_response::custom_output_field(&on, "brightness")
            .and_then(lemnos::core::Value::as_u64)
    );
    println!(
        "after trigger trigger_file={:?} response_trigger={:?}",
        root.read("sys/class/leds/ACT/trigger"),
        linux_response::custom_output_field(&trigger, "active_trigger")
            .and_then(lemnos::core::Value::as_str)
    );
    println!("state telemetry={:?}", state.telemetry);

    Ok(())
}
