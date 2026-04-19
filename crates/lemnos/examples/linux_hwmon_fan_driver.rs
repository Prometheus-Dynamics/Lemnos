#![allow(clippy::print_stdout)]

#[path = "support/linux_hwmon_fan.rs"]
mod linux_hwmon_fan;
#[path = "support/linux_response.rs"]
mod linux_response;
#[path = "support/linux_test_root.rs"]
mod linux_test_root;

use lemnos::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = linux_hwmon_fan::LinuxHwmonFanTestRoot::new();
    root.create_fan("hwmon3", "pwmfan", 120, 1, 4321, "pwm-fan");

    let backend = lemnos::linux::LinuxBackend::with_paths(root.paths());
    let mut lemnos = Lemnos::builder()
        .with_linux_backend_ref(&backend)
        .with_driver(linux_hwmon_fan::ExampleLinuxHwmonFanDriver)?
        .build();

    lemnos.refresh_with_linux_default(&backend)?;
    let device_id = linux_hwmon_fan::fan_device_id(&lemnos)?;
    lemnos.bind(&device_id)?;

    let read = lemnos.request_custom(device_id.clone(), linux_hwmon_fan::FAN_READ_INTERACTION)?;
    let set_pwm = lemnos.request_custom_value(
        device_id.clone(),
        linux_hwmon_fan::FAN_SET_PWM_INTERACTION,
        200_u64,
    )?;
    let set_mode = lemnos.request_custom_value(
        device_id.clone(),
        linux_hwmon_fan::FAN_SET_MODE_INTERACTION,
        2_u64,
    )?;
    let state = lemnos
        .refresh_state(&device_id)?
        .cloned()
        .ok_or("missing fan state")?;

    println!("discovered fan: {device_id}");
    println!(
        "read pwm={:?} rpm={:?}",
        linux_response::custom_output_field(&read, "pwm").and_then(lemnos::core::Value::as_u64),
        linux_response::custom_output_field(&read, "rpm").and_then(lemnos::core::Value::as_u64)
    );
    println!(
        "after set_pwm pwm_file={:?} response_pwm={:?}",
        root.read("sys/class/hwmon/hwmon3/pwm1"),
        linux_response::custom_output_field(&set_pwm, "pwm").and_then(lemnos::core::Value::as_u64)
    );
    println!(
        "after set_mode mode_file={:?} response_mode={:?}",
        root.read("sys/class/hwmon/hwmon3/pwm1_enable"),
        linux_response::custom_output_field(&set_mode, "pwm_mode")
            .and_then(lemnos::core::Value::as_u64)
    );
    println!("state telemetry={:?}", state.telemetry);

    Ok(())
}
