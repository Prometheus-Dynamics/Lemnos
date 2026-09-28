use super::super::support::output_config;
use crate::{Runtime, RuntimeError};
use lemnos_core::{DeviceId, ErrorKind};
use lemnos_discovery::DiscoveryContext;
use lemnos_drivers_gpio::GpioDriver;
use lemnos_mock::{MockGpioLine, MockHardware};

#[test]
fn runtime_errors_classify_through_driver_and_bus_layers() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 21).with_configuration(output_config()))
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime.register_driver(GpioDriver).expect("register gpio");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    hardware.queue_timeout(&device_id, "open");
    let timeout = runtime.bind(&device_id).expect_err("bind times out");
    assert!(matches!(timeout, RuntimeError::Driver { .. }));
    assert_eq!(timeout.kind(), ErrorKind::Timeout);
    assert!(timeout.kind().is_transient());

    hardware.queue_disconnect(&device_id, "open");
    assert_eq!(
        runtime.bind(&device_id).expect_err("disconnected").kind(),
        ErrorKind::Unavailable
    );

    let unknown = DeviceId::new("missing").expect("id");
    assert_eq!(
        runtime.bind(&unknown).expect_err("unknown").kind(),
        ErrorKind::NotFound
    );

    runtime.shutdown();
    assert_eq!(
        runtime
            .refresh(&DiscoveryContext::new(), &[&hardware])
            .expect_err("stopped")
            .kind(),
        ErrorKind::Unavailable
    );
}
