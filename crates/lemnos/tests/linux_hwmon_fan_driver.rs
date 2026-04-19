#![cfg(feature = "linux")]

#[path = "../examples/support/linux_hwmon_fan.rs"]
mod linux_hwmon_fan;
#[path = "../examples/support/linux_test_root.rs"]
mod linux_test_root;

use lemnos::core::{DeviceResponse, Value};
use lemnos::discovery::DiscoveryContext;
use lemnos::prelude::*;

fn custom_u64(response: &DeviceResponse, key: &str) -> Option<u64> {
    match &response.interaction {
        lemnos::core::InteractionResponse::Custom(lemnos::core::CustomInteractionResponse {
            output: Some(Value::Map(map)),
            ..
        }) => map.get(key).and_then(Value::as_u64),
        _ => None,
    }
}

#[test]
fn runtime_binds_and_drives_linux_hwmon_fan_device() {
    let root = linux_hwmon_fan::LinuxHwmonFanTestRoot::new();
    root.create_fan("hwmon3", "pwmfan", 120, 1, 4321, "pwm-fan");

    let backend = lemnos::linux::LinuxBackend::with_paths(root.paths());
    let mut lemnos = Lemnos::builder()
        .with_linux_backend_ref(&backend)
        .with_driver(linux_hwmon_fan::ExampleLinuxHwmonFanDriver)
        .expect("register hwmon fan driver")
        .build();

    let report = lemnos
        .refresh_with_linux(&DiscoveryContext::new(), &backend)
        .expect("refresh with linux backend");
    assert_eq!(report.diff.added.len(), 1);

    let device_id = linux_hwmon_fan::fan_device_id(&lemnos).expect("fan device id");
    lemnos.bind(&device_id).expect("bind fan");

    let read = lemnos
        .request_custom(device_id.clone(), linux_hwmon_fan::FAN_READ_INTERACTION)
        .expect("read fan");
    assert_eq!(custom_u64(&read, "pwm"), Some(120));
    assert_eq!(custom_u64(&read, "rpm"), Some(4321));

    lemnos
        .request_custom_value(
            device_id.clone(),
            linux_hwmon_fan::FAN_SET_PWM_INTERACTION,
            200_u64,
        )
        .expect("set fan pwm");
    assert_eq!(root.read("sys/class/hwmon/hwmon3/pwm1"), "200");

    lemnos
        .request_custom_value(
            device_id.clone(),
            linux_hwmon_fan::FAN_SET_MODE_INTERACTION,
            2_u64,
        )
        .expect("set fan mode");
    assert_eq!(root.read("sys/class/hwmon/hwmon3/pwm1_enable"), "2");

    let state = lemnos
        .refresh_state(&device_id)
        .expect("refresh fan state")
        .cloned()
        .expect("cached fan state");
    assert_eq!(
        state.telemetry.get("pwm").and_then(Value::as_u64),
        Some(200)
    );
    assert_eq!(
        state.telemetry.get("rpm").and_then(Value::as_u64),
        Some(4321)
    );
}
