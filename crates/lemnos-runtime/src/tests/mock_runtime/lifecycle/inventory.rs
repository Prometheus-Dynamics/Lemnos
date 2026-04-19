use super::*;
use lemnos_core::{DeviceRequest, GpioRequest, InteractionResponse, StandardResponse};
use lemnos_drivers_gpio::GpioDriver;
use lemnos_drivers_i2c::I2cDriver;
use lemnos_mock::{MockGpioLine, MockHardware, MockI2cDevice};

#[test]
fn runtime_refreshes_inventory_and_dispatches_gpio_requests() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 7)
                .with_line_name("status")
                .with_configuration(output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");

    let report = runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    assert_eq!(report.discovery.snapshot.len(), 1);
    assert!(runtime.inventory().contains(&device_id));

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect("write request");

    assert_eq!(
        response.interaction,
        InteractionResponse::Standard(StandardResponse::Gpio(lemnos_core::GpioResponse::Applied))
    );
    assert!(runtime.is_bound(&device_id));
    assert_eq!(hardware.gpio_level(&device_id), Some(GpioLevel::High));
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("state")
            .telemetry
            .get("level"),
        Some(&"high".into())
    );
}

#[test]
fn runtime_incremental_refresh_replaces_only_requested_interface_slice() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 7)
                .with_line_name("status")
                .with_configuration(output_config()),
        )
        .with_i2c_device(MockI2cDevice::new(1, 0x40))
        .build();
    let gpio_id = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == lemnos_core::InterfaceKind::Gpio)
        .expect("gpio descriptor")
        .id;

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime.set_i2c_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register gpio driver");
    runtime
        .register_driver(I2cDriver)
        .expect("register i2c driver");

    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("full refresh");
    runtime.bind(&gpio_id).expect("bind gpio");

    let empty_probe = MockHardware::builder().build();
    let report = runtime
        .refresh_incremental(
            &DiscoveryContext::new().with_requested_interface(lemnos_core::InterfaceKind::I2c),
            &[&empty_probe],
        )
        .expect("incremental refresh");

    assert_eq!(
        report
            .discovery
            .snapshot
            .count_for(lemnos_core::InterfaceKind::Gpio),
        1
    );
    assert_eq!(
        report
            .discovery
            .snapshot
            .count_for(lemnos_core::InterfaceKind::I2c),
        0
    );
    assert!(runtime.inventory().contains(&gpio_id));
    assert!(runtime.is_bound(&gpio_id));
    assert_eq!(report.diff.removed.len(), 1);
    assert_eq!(report.diff.added.len(), 0);
}

#[test]
fn runtime_refresh_preserves_existing_devices_when_a_probe_fails() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 7)
                .with_line_name("status")
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
        .expect("initial refresh");
    runtime.bind(&device_id).expect("bind");
    assert!(runtime.is_bound(&device_id));

    let report = runtime
        .refresh(&DiscoveryContext::new(), &[&FailingGpioProbe])
        .expect("refresh should preserve prior inventory on probe failure");

    assert!(report.discovery.has_probe_failures());
    assert!(runtime.inventory().contains(&device_id));
    assert!(runtime.is_bound(&device_id));
    assert!(report.diff.added.is_empty());
    assert!(report.diff.removed.is_empty());
    assert!(report.diff.changed.is_empty());
}

#[test]
fn runtime_refresh_keeps_bound_device_alive_across_metadata_only_descriptor_changes() {
    let generation = Arc::new(AtomicUsize::new(0));
    let bind_count = Arc::new(AtomicUsize::new(0));
    let close_count = Arc::new(AtomicUsize::new(0));
    let probe = MetadataChangeProbe {
        generation: Arc::clone(&generation),
    };

    let mut runtime = Runtime::new();
    runtime
        .register_driver(CountingDriver {
            bind_count: Arc::clone(&bind_count),
            close_count: Arc::clone(&close_count),
        })
        .expect("register counting driver");

    let initial = runtime
        .refresh(&DiscoveryContext::new(), &[&probe])
        .expect("initial refresh");
    let device_id = initial.discovery.snapshot.devices[0].id.clone();
    runtime.bind(&device_id).expect("bind");

    generation.store(1, Ordering::SeqCst);
    let refresh = runtime
        .refresh(&DiscoveryContext::new(), &[&probe])
        .expect("metadata refresh");

    assert_eq!(refresh.diff.changed.len(), 1);
    assert!(refresh.rebinds.attempted.is_empty());
    assert!(runtime.is_bound(&device_id));
    assert_eq!(bind_count.load(Ordering::SeqCst), 1);
    assert_eq!(close_count.load(Ordering::SeqCst), 0);
    assert_eq!(
        runtime
            .inventory()
            .get(&device_id)
            .expect("updated descriptor")
            .labels
            .get("generation")
            .map(String::as_str),
        Some("1")
    );
}

#[test]
fn runtime_can_be_stopped_and_restarted() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 9)
                .with_line_name("runtime-power")
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

    runtime.shutdown();
    assert!(!runtime.is_running());
    assert!(matches!(
        runtime.request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Read)),
        )),
        Err(RuntimeError::NotRunning)
    ));

    runtime.start();
    assert!(runtime.is_running());
    let response = runtime
        .request(DeviceRequest::new(
            device_id,
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Read)),
        ))
        .expect("request after restart");

    assert_eq!(
        response.interaction,
        InteractionResponse::Standard(StandardResponse::Gpio(lemnos_core::GpioResponse::Level(
            GpioLevel::Low
        )))
    );
}

#[test]
fn runtime_can_require_explicit_binding_before_requests() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 11).with_configuration(output_config()))
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::with_config(RuntimeConfig::new().with_auto_bind_on_request(false));
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    let err = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Read)),
        ))
        .expect_err("request should require explicit bind");
    assert!(matches!(err, RuntimeError::DeviceNotBound { .. }));

    runtime.bind(&device_id).expect("bind");
    runtime
        .request(DeviceRequest::new(
            device_id,
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Read)),
        ))
        .expect("request after bind");
}

#[test]
fn runtime_can_disable_automatic_state_caching() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 12)
                .with_line_name("cache-test")
                .with_configuration(output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::with_config(
        RuntimeConfig::new()
            .with_cache_state_on_bind(false)
            .with_cache_state_on_request(false),
    );
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    runtime.bind(&device_id).expect("bind");
    assert!(runtime.state(&device_id).is_none());

    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect("write request");
    assert_eq!(hardware.gpio_level(&device_id), Some(GpioLevel::High));
    assert!(runtime.state(&device_id).is_none());

    runtime
        .refresh_state(&device_id)
        .expect("explicit refresh_state should still work");
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("explicitly refreshed state")
            .telemetry
            .get("level"),
        Some(&"high".into())
    );
}
