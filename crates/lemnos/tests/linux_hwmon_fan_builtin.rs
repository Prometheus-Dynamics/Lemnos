#![cfg(all(feature = "linux", feature = "builtin-drivers"))]

#[path = "../examples/support/linux_test_root.rs"]
mod linux_test_root;

use lemnos::core::{
    CustomInteractionResponse, DeviceId, DeviceKind, DeviceResponse, InteractionResponse,
    InterfaceKind, Value,
};
use lemnos::discovery::DiscoveryContext;
use lemnos::drivers::pwm::hwmon_fan::{
    self, FAN_READ_INTERACTION, FAN_SET_DUTY_INTERACTION, FAN_SET_MODE_INTERACTION,
    FAN_SET_PWM_INTERACTION,
};
use lemnos::prelude::*;

fn output(response: &DeviceResponse, key: &str) -> Option<Value> {
    match &response.interaction {
        InteractionResponse::Custom(CustomInteractionResponse {
            output: Some(Value::Map(map)),
            ..
        }) => map.get(key).cloned(),
        _ => None,
    }
}

/// A fake sysfs tree with one `pwm-fan` hwmon device.
fn fan_root(hwmon: &str, pwm: u64, mode: u64, rpm: u64) -> linux_test_root::ExampleLinuxTestRoot {
    let root = linux_test_root::ExampleLinuxTestRoot::new("lemnos-builtin-hwmon-fan");
    let class = format!("sys/class/hwmon/{hwmon}");
    root.write(format!("{class}/name"), "pwmfan\n");
    root.write(format!("{class}/pwm1"), &format!("{pwm}\n"));
    root.write(format!("{class}/pwm1_enable"), &format!("{mode}\n"));
    root.write(format!("{class}/fan1_input"), &format!("{rpm}\n"));
    root.create_dir("sys/devices/platform/pwmfan");
    root.create_dir("sys/bus/platform/drivers/pwm-fan");
    std::os::unix::fs::symlink(
        root.root_path("sys/devices/platform/pwmfan"),
        root.root_path(format!("{class}/device")),
    )
    .expect("device symlink");
    std::os::unix::fs::symlink(
        root.root_path("sys/bus/platform/drivers/pwm-fan"),
        root.root_path("sys/devices/platform/pwmfan/driver"),
    )
    .expect("driver symlink");
    root
}

fn fan_device_id(lemnos: &Lemnos) -> DeviceId {
    lemnos
        .inventory()
        .by_kind(DeviceKind::Unspecified(InterfaceKind::Pwm))
        .into_iter()
        .next()
        .expect("hwmon fan discovered")
        .id
        .clone()
}

fn fan_lemnos(
    root: &linux_test_root::ExampleLinuxTestRoot,
) -> (Lemnos, lemnos::linux::LinuxBackend) {
    let backend = lemnos::linux::LinuxBackend::with_paths(root.paths());
    let lemnos = Lemnos::builder()
        .with_linux_backend_ref(&backend)
        .with_builtin_drivers()
        .expect("register builtin drivers")
        .with_bind_policy(RuntimeBindPolicy::new().with_driver(hwmon_fan::DRIVER_ID))
        .build();
    (lemnos, backend)
}

#[test]
fn builtin_hwmon_fan_driver_is_bound_by_policy_and_reports_fan_telemetry() {
    let root = fan_root("hwmon3", 51, 1, 4321);
    let (mut lemnos, backend) = fan_lemnos(&root);

    let report = lemnos
        .refresh_with_linux(&DiscoveryContext::new(), &backend)
        .expect("refresh");
    let device_id = fan_device_id(&lemnos);

    assert_eq!(report.rebinds.rebound, vec![device_id.clone()]);
    assert_eq!(
        lemnos.device_status(&device_id),
        Some(DeviceStatus::Available)
    );

    let state = lemnos.state(&device_id).expect("state cached on bind");
    assert_eq!(
        state.telemetry.get(hwmon_fan::TELEMETRY_DUTY_RATIO),
        Some(&Value::from(0.2))
    );
    assert_eq!(
        state.telemetry.get(hwmon_fan::TELEMETRY_MODE),
        Some(&Value::from("manual"))
    );
    assert_eq!(
        state.telemetry.get(hwmon_fan::TELEMETRY_RPM),
        Some(&Value::from(4321_u64))
    );
    assert_eq!(
        state.realized_config.get(hwmon_fan::CONFIG_FAN_NAME),
        Some(&Value::from("pwmfan"))
    );
}

#[test]
fn builtin_hwmon_fan_driver_sets_duty_pwm_and_mode() {
    let root = fan_root("hwmon1", 0, 1, 0);
    let (mut lemnos, backend) = fan_lemnos(&root);
    lemnos
        .refresh_with_linux(&DiscoveryContext::new(), &backend)
        .expect("refresh");
    let device_id = fan_device_id(&lemnos);

    let duty = lemnos
        .request_custom_value(device_id.clone(), FAN_SET_DUTY_INTERACTION, 0.5)
        .expect("set duty");
    assert_eq!(root.read("sys/class/hwmon/hwmon1/pwm1"), "128");
    assert_eq!(output(&duty, "pwm"), Some(Value::from(128_u64)));

    lemnos
        .request_custom_value(device_id.clone(), FAN_SET_PWM_INTERACTION, 255_u64)
        .expect("set pwm");
    assert_eq!(root.read("sys/class/hwmon/hwmon1/pwm1"), "255");

    let mode = lemnos
        .request_custom_value(device_id.clone(), FAN_SET_MODE_INTERACTION, "automatic")
        .expect("set mode by name");
    assert_eq!(root.read("sys/class/hwmon/hwmon1/pwm1_enable"), "2");
    assert_eq!(output(&mode, "mode"), Some(Value::from("automatic")));

    lemnos
        .request_custom_value(device_id.clone(), FAN_SET_MODE_INTERACTION, 0_u64)
        .expect("set raw mode");
    assert_eq!(root.read("sys/class/hwmon/hwmon1/pwm1_enable"), "0");

    let read = lemnos
        .request_custom(device_id.clone(), FAN_READ_INTERACTION)
        .expect("read");
    assert_eq!(output(&read, "mode"), Some(Value::from("full-speed")));

    for (interaction, input) in [
        (FAN_SET_DUTY_INTERACTION, Value::from(1.5)),
        (FAN_SET_PWM_INTERACTION, Value::from(256_u64)),
        (FAN_SET_MODE_INTERACTION, Value::from("turbo")),
        (FAN_SET_MODE_INTERACTION, Value::from(9_u64)),
    ] {
        assert!(
            lemnos
                .request_custom_value(device_id.clone(), interaction, input.clone())
                .is_err(),
            "{interaction} should reject {input:?}"
        );
    }
    assert_eq!(root.read("sys/class/hwmon/hwmon1/pwm1"), "255");
}
