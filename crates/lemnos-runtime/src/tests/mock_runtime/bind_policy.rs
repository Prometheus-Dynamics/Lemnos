use super::super::support::output_config;
use crate::{Runtime, RuntimeBindPolicy, RuntimeFailureOperation};
use lemnos_core::{DeviceId, DeviceStatus, InterfaceKind};
use lemnos_discovery::DiscoveryContext;
use lemnos_drivers_gpio::GpioDriver;
use lemnos_drivers_i2c::I2cDriver;
use lemnos_mock::{MockGpioLine, MockHardware, MockI2cDevice};

fn gpio_and_i2c_hardware() -> (MockHardware, DeviceId, DeviceId) {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 3)
                .with_line_name("policy")
                .with_configuration(output_config()),
        )
        .with_i2c_device(MockI2cDevice::new(1, 0x40))
        .build();
    let id_for = |interface| {
        hardware
            .descriptors()
            .into_iter()
            .find(|device| device.interface == interface)
            .expect("descriptor")
            .id
    };
    let gpio_id = id_for(InterfaceKind::Gpio);
    let i2c_id = id_for(InterfaceKind::I2c);
    (hardware, gpio_id, i2c_id)
}

fn runtime_for(hardware: &MockHardware) -> Runtime {
    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime.set_i2c_backend(hardware.clone());
    runtime.register_driver(GpioDriver).expect("register gpio");
    runtime.register_driver(I2cDriver).expect("register i2c");
    runtime
}

#[test]
fn default_bind_policy_leaves_devices_unbound_but_reports_status() {
    let (hardware, gpio_id, i2c_id) = gpio_and_i2c_hardware();
    let mut runtime = runtime_for(&hardware);
    assert!(runtime.bind_policy().is_empty());

    let report = runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    assert!(report.rebinds.attempted.is_empty());
    assert!(!runtime.is_bound(&gpio_id));
    assert!(!runtime.is_bound(&i2c_id));
    assert_eq!(
        runtime.device_status(&gpio_id),
        Some(DeviceStatus::Available)
    );
    assert_eq!(
        runtime.device_status(&DeviceId::new("not-discovered").expect("id")),
        None
    );
}

#[test]
fn bind_policy_binds_only_matching_devices_on_refresh() {
    let (hardware, gpio_id, i2c_id) = gpio_and_i2c_hardware();
    let mut runtime = runtime_for(&hardware);
    runtime.set_bind_policy(RuntimeBindPolicy::new().with_driver("lemnos.gpio.generic"));

    let report = runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    assert_eq!(report.rebinds.rebound, vec![gpio_id.clone()]);
    assert!(runtime.is_bound(&gpio_id));
    assert!(runtime.wants_binding(&gpio_id));
    assert!(runtime.has_state(&gpio_id));
    assert!(!runtime.is_bound(&i2c_id));
    assert_eq!(
        runtime.device_status(&gpio_id),
        Some(DeviceStatus::Available)
    );

    let unchanged = runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("second refresh");
    assert!(unchanged.rebinds.attempted.is_empty());
}

#[test]
fn bind_policy_binds_devices_added_by_later_refreshes() {
    let (hardware, _gpio_id, i2c_id) = gpio_and_i2c_hardware();
    let mut runtime = runtime_for(&hardware);
    runtime.set_bind_policy(RuntimeBindPolicy::new().with_interface(InterfaceKind::Gpio));
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("initial refresh");

    let added = hardware.attach_gpio_line(
        MockGpioLine::new("gpiochip0", 4)
            .with_line_name("hotplugged")
            .with_configuration(output_config()),
    );
    let report = runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after attach");

    assert_eq!(report.rebinds.rebound, vec![added.clone()]);
    assert!(runtime.is_bound(&added));
    assert!(!runtime.is_bound(&i2c_id));
}

#[test]
fn bind_policy_failures_are_recorded_as_binds_and_not_retried_until_change() {
    let (hardware, gpio_id, _i2c_id) = gpio_and_i2c_hardware();
    let mut runtime = runtime_for(&hardware);
    runtime.set_bind_policy(RuntimeBindPolicy::all());
    hardware.queue_timeout(&gpio_id, "open");

    let report = runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    assert!(report.rebinds.failed.contains(&gpio_id));
    assert!(!runtime.is_bound(&gpio_id));
    assert_eq!(
        runtime.failure(&gpio_id).expect("failure").operation,
        RuntimeFailureOperation::Bind
    );
    assert_eq!(runtime.device_status(&gpio_id), Some(DeviceStatus::Faulted));

    let unchanged = runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("second refresh");
    assert!(!unchanged.rebinds.attempted.contains(&gpio_id));

    runtime.bind(&gpio_id).expect("explicit bind succeeds");
    assert_eq!(
        runtime.device_status(&gpio_id),
        Some(DeviceStatus::Available)
    );
}
