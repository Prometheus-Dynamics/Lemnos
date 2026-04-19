#![cfg(feature = "linux")]

#[path = "../examples/support/linux_led.rs"]
mod linux_led;
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
fn runtime_binds_and_drives_linux_led_class_device() {
    let root = linux_led::LinuxLedTestRoot::new();
    root.create_led("ACT", 1, 255, "none [default-on] heartbeat\n", "leds-gpio");

    let backend = lemnos::linux::LinuxBackend::with_paths(root.paths());
    let mut lemnos = Lemnos::builder()
        .with_linux_backend_ref(&backend)
        .with_driver(linux_led::ExampleLinuxLedDriver)
        .expect("register LED driver")
        .build();

    let report = lemnos
        .refresh_with_linux(&DiscoveryContext::new(), &backend)
        .expect("refresh with linux backend");
    assert_eq!(report.diff.added.len(), 1);

    let device_id = linux_led::led_device_id(&lemnos).expect("led device id");
    lemnos.bind(&device_id).expect("bind LED");

    let read = lemnos
        .request_custom(device_id.clone(), linux_led::LED_READ_INTERACTION)
        .expect("read LED");
    assert_eq!(custom_u64(&read, "brightness"), Some(1));

    lemnos
        .request_custom(device_id.clone(), linux_led::LED_ON_INTERACTION)
        .expect("turn LED on");
    assert_eq!(root.read("sys/class/leds/ACT/brightness"), "255");

    lemnos
        .request_custom_value(
            device_id.clone(),
            linux_led::LED_SET_BRIGHTNESS_INTERACTION,
            17_u64,
        )
        .expect("set LED brightness");
    assert_eq!(root.read("sys/class/leds/ACT/brightness"), "17");

    lemnos
        .request_custom_value(
            device_id.clone(),
            linux_led::LED_SET_TRIGGER_INTERACTION,
            "heartbeat",
        )
        .expect("set LED trigger");
    assert_eq!(root.read("sys/class/leds/ACT/trigger"), "heartbeat");

    let state = lemnos
        .refresh_state(&device_id)
        .expect("refresh LED state")
        .cloned()
        .expect("cached LED state");
    assert_eq!(
        state.telemetry.get("brightness").and_then(Value::as_u64),
        Some(17)
    );
}
