#![cfg(all(feature = "mock", feature = "builtin-drivers"))]

use lemnos::core::{DeviceId, InterfaceKind, Value};
use lemnos::discovery::DiscoveryContext;
use lemnos::drivers::pwm::hwmon_fan::{self, FAN_SET_DUTY_INTERACTION, FAN_SET_MODE_INTERACTION};
use lemnos::mock::{MockGpioLine, MockHardware, MockHwmonFan};
use lemnos::prelude::*;

fn hardware() -> (MockHardware, DeviceId, DeviceId) {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 16))
        .with_hwmon_fan(
            MockHwmonFan::new("hwmon3")
                .with_pwm(102)
                .with_mode(1)
                .with_rpm(3000),
        )
        .build();
    let find = |interface| {
        hardware
            .descriptors()
            .into_iter()
            .find(|device| device.interface == interface)
            .expect("descriptor")
            .id
    };
    let gpio_id = find(InterfaceKind::Gpio);
    let fan_id = find(InterfaceKind::Pwm);
    (hardware, gpio_id, fan_id)
}

#[test]
fn mock_hwmon_fan_binds_through_builtin_driver_without_real_sysfs() {
    let (hardware, gpio_id, fan_id) = hardware();
    let mut lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_builtin_drivers()
        .expect("builtin drivers")
        .with_bind_policy(RuntimeBindPolicy::new().with_driver(hwmon_fan::DRIVER_ID))
        .build();

    let report = lemnos
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    assert_eq!(report.rebinds.rebound, vec![fan_id.clone()]);
    assert!(!lemnos.is_bound(&gpio_id));
    assert_eq!(
        lemnos.device_status(&gpio_id),
        Some(DeviceStatus::Available)
    );
    assert_eq!(
        lemnos
            .state(&fan_id)
            .and_then(|state| state.telemetry.get(hwmon_fan::TELEMETRY_DUTY_RATIO)),
        Some(&Value::from(0.4))
    );

    lemnos
        .request_custom_value(fan_id.clone(), FAN_SET_DUTY_INTERACTION, 1.0)
        .expect("set duty");
    lemnos
        .request_custom_value(fan_id.clone(), FAN_SET_MODE_INTERACTION, "automatic")
        .expect("set mode");
    assert_eq!(hardware.hwmon_fan_pwm(&fan_id), Some(255));
    assert_eq!(hardware.hwmon_fan_mode(&fan_id), Some(2));

    assert!(hardware.set_hwmon_fan_rpm(&fan_id, 4800));
    let state = lemnos
        .refresh_state(&fan_id)
        .expect("refresh state")
        .cloned()
        .expect("fan state");
    assert_eq!(
        state.telemetry.get(hwmon_fan::TELEMETRY_RPM),
        Some(&Value::from(4800_u64))
    );
}

#[test]
fn removing_mock_hwmon_fan_cleans_up_its_sysfs_directory() {
    let (hardware, _gpio_id, fan_id) = hardware();
    let root = hardware.hwmon_fan_root(&fan_id).expect("fan root");
    assert!(root.join("pwm1").exists());

    assert!(hardware.remove_device(&fan_id));
    assert!(!root.exists());
    assert_eq!(hardware.hwmon_fan_pwm(&fan_id), None);
}
