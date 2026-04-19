use super::super::support::output_config;
use crate::{Runtime, RuntimeFailureCategory, RuntimeFailureOperation};
use lemnos_core::{DeviceRequest, GpioRequest, InteractionRequest, StandardRequest};
use lemnos_discovery::DiscoveryContext;
use lemnos_drivers_gpio::GpioDriver;
use lemnos_mock::{MockFaultScript, MockGpioLine, MockHardware};

#[test]
fn runtime_refresh_state_tracks_and_clears_failures() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 24)
                .with_line_name("refresh-state-failure")
                .with_configuration(output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    runtime.bind(&device_id).expect("bind");

    hardware.queue_script(
        &device_id,
        MockFaultScript::new().transport_failure("gpio.read", "refresh state injected failure"),
    );

    let error = runtime
        .refresh_state(&device_id)
        .expect_err("refresh_state should surface a driver failure");
    assert!(matches!(error, crate::RuntimeError::Driver { .. }));

    let failure = runtime
        .failure(&device_id)
        .expect("failure should be recorded");
    assert_eq!(failure.operation, RuntimeFailureOperation::RefreshState);
    assert_eq!(failure.category, RuntimeFailureCategory::Driver);
    assert!(failure.message.contains("refresh state injected failure"));
    assert_eq!(failure.occurrence_count, 1);
    assert!(failure.first_occurred_at.is_some());
    assert_eq!(failure.first_occurred_at, failure.last_occurred_at);

    runtime
        .refresh_state(&device_id)
        .expect("refresh_state should succeed after the one-shot fault");
    assert!(runtime.failure(&device_id).is_none());
}

#[test]
fn runtime_failure_records_accumulate_repeat_context() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 25)
                .with_line_name("repeat-failure")
                .with_configuration(output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    hardware.queue_script(
        &device_id,
        MockFaultScript::new()
            .transport_failure("gpio.read", "first repeated failure")
            .transport_failure("gpio.read", "second repeated failure"),
    );

    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Read)),
        ))
        .expect_err("first request should fail");
    let first = runtime
        .failure(&device_id)
        .expect("first failure should be recorded")
        .clone();
    assert_eq!(first.occurrence_count, 1);
    assert!(first.message.contains("first repeated failure"));
    assert!(first.first_occurred_at.is_some());

    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Read)),
        ))
        .expect_err("second request should fail");
    let second = runtime
        .failure(&device_id)
        .expect("second failure should be recorded");
    assert_eq!(second.operation, RuntimeFailureOperation::Request);
    assert_eq!(second.category, RuntimeFailureCategory::Driver);
    assert_eq!(second.occurrence_count, 2);
    assert!(second.message.contains("second repeated failure"));
    assert_eq!(second.first_occurred_at, first.first_occurred_at);
    assert!(second.last_occurred_at.is_some());
    assert!(second.last_occurred_at >= second.first_occurred_at);
}
