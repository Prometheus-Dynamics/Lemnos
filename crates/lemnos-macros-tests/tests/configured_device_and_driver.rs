extern crate self as lemnos;

#[path = "support/lemnos_shim.rs"]
mod lemnos_shim;

pub use lemnos_shim::{core, discovery, driver};

use crate::discovery::DiscoveryProbe;
use lemnos_macros::ConfiguredDevice;

#[lemnos_macros::enum_values(bits: u8, hertz: f64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExampleRate {
    #[lemnos(bits = 0x00, hertz = 10.0)]
    Hz10,
    #[lemnos(bits = 0x07, hertz = 30.0)]
    Hz30,
}

#[derive(Debug, Clone, PartialEq, ConfiguredDevice)]
struct ExampleConfig {
    bus: u32,
    label: String,
    irq_line: Option<u32>,
}

#[lemnos_macros::driver(
    id = "example.sensor.mock",
    summary = "Mock sensor driver",
    description = "Mock sensor driver used for macro tests",
    interface = I2c,
    kind = I2cDevice,
    priority = Exact,
    version = (1, 4, 2),
    tags("sensor", "mock")
)]
#[derive(Debug, Clone, PartialEq)]
struct ExampleDriver {
    config: ExampleConfig,
    poll_interval_ms: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExampleAccelRange {
    G6,
}

#[derive(Debug, Clone, PartialEq, ConfiguredDevice)]
#[lemnos(
    interface = I2c,
    id = "helios.bmi088",
    driver = "helios.sensor.bmi088",
    summary = "Configured BMI088 IMU"
)]
struct ExampleImuConfig {
    #[lemnos(bus(i2c))]
    bus: u32,
    #[lemnos(endpoint(i2c, name = "accel"))]
    accel_address: u16,
    #[lemnos(endpoint(i2c, name = "gyro"))]
    gyro_address: u16,
    #[lemnos(display_name, label)]
    label: String,
    #[lemnos(property)]
    accel_range: ExampleAccelRange,
    #[lemnos(signal(gpio, name = "accel_int"))]
    accel_int: Option<lemnos::core::ConfiguredGpioSignal>,
}

#[derive(Debug, Clone, PartialEq, ConfiguredDevice)]
#[lemnos(
    interface = Spi,
    id = "example.flash",
    driver = "example.flash.driver",
    summary = "Configured SPI flash"
)]
struct ExampleSpiConfig {
    #[lemnos(bus(spi))]
    bus: u32,
    #[lemnos(endpoint(spi, name = "flash"))]
    chip_select: u16,
    #[lemnos(display_name)]
    label: String,
}

#[test]
fn configured_device_derive_generates_builder() {
    let config = ExampleConfig::builder()
        .bus(4_u32)
        .label("board-imu")
        .irq_line(23_u32)
        .build()
        .expect("builder should succeed");

    assert_eq!(
        config,
        ExampleConfig {
            bus: 4,
            label: "board-imu".into(),
            irq_line: Some(23),
        }
    );
}

#[test]
fn driver_attribute_generates_typed_metadata_and_builder() {
    let config = ExampleConfig::builder()
        .bus(1_u32)
        .label("test-sensor")
        .build()
        .expect("config builder");
    let driver = ExampleDriver::builder()
        .config(config.clone())
        .poll_interval_ms(5_u32)
        .build()
        .expect("driver builder");

    assert_eq!(ExampleDriver::DRIVER_ID, "example.sensor.mock");
    assert_eq!(ExampleDriver::DRIVER_SUMMARY, "Mock sensor driver");
    assert_eq!(
        ExampleDriver::DRIVER_DESCRIPTION,
        Some("Mock sensor driver used for macro tests")
    );
    assert_eq!(
        ExampleDriver::DRIVER_INTERFACE,
        lemnos::core::InterfaceKind::I2c
    );
    assert_eq!(
        ExampleDriver::DRIVER_VERSION,
        lemnos::driver::DriverVersion::new(1, 4, 2)
    );
    assert_eq!(
        ExampleDriver::DRIVER_PRIORITY,
        lemnos::driver::DriverPriority::Exact
    );
    assert_eq!(
        ExampleDriver::DRIVER_KIND,
        Some(lemnos::core::DeviceKind::I2cDevice)
    );
    assert_eq!(ExampleDriver::DRIVER_TAGS, &["sensor", "mock"]);

    let manifest = ExampleDriver::driver_manifest_base();
    assert_eq!(manifest.id, "example.sensor.mock");
    assert_eq!(
        manifest.version,
        lemnos::driver::DriverVersion::new(1, 4, 2)
    );
    assert_eq!(manifest.summary, "Mock sensor driver");
    assert_eq!(
        manifest.description.as_deref(),
        Some("Mock sensor driver used for macro tests")
    );
    assert_eq!(manifest.priority, lemnos::driver::DriverPriority::Exact);
    assert_eq!(manifest.interfaces, vec![lemnos::core::InterfaceKind::I2c]);
    assert_eq!(manifest.kinds, vec![lemnos::core::DeviceKind::I2cDevice]);
    assert_eq!(
        manifest.tags,
        vec!["sensor".to_string(), "mock".to_string()]
    );

    assert_eq!(driver.config, config);
    assert_eq!(driver.poll_interval_ms, Some(5));
}

#[test]
fn required_builder_fields_are_enforced() {
    let error = ExampleConfig::builder()
        .label("missing bus")
        .build()
        .expect_err("missing required fields should fail");

    assert!(error.contains("bus"));
}

#[test]
fn configured_device_derive_generates_typed_composite_helpers() {
    let config = ExampleImuConfig::builder()
        .bus(4_u32)
        .accel_address(0x18_u16)
        .gyro_address(0x68_u16)
        .label("board-imu")
        .accel_range(ExampleAccelRange::G6)
        .accel_int(
            lemnos::core::ConfiguredGpioSignal::by_chip_line("gpiochip4", 23)
                .with_global_line(311)
                .with_edge(lemnos::core::GpioEdge::Rising)
                .with_required(true),
        )
        .build()
        .expect("imu config builder");

    assert_eq!(
        config.configured_device_id(),
        "helios.bmi088.bus4.accel0x18.gyro0x68"
    );
    assert_eq!(
        ExampleImuConfig::CONFIGURED_DEVICE_INTERFACES,
        &[
            lemnos::core::InterfaceKind::I2c,
            lemnos::core::InterfaceKind::Gpio
        ]
    );

    let endpoints = config.configured_i2c_endpoints();
    assert_eq!(endpoints.len(), 2);
    assert_eq!(endpoints[0].name, "accel");
    assert_eq!(endpoints[1].name, "gyro");

    let signals = config.configured_gpio_signals();
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].name, "accel_int");

    let descriptors = config
        .configured_descriptors()
        .expect("configured descriptors");
    assert_eq!(descriptors.len(), 4);

    let logical = &descriptors[0];
    assert_eq!(logical.id.as_str(), "helios.bmi088.bus4.accel0x18.gyro0x68");
    assert_eq!(logical.display_name.as_deref(), Some("board-imu"));
    assert_eq!(
        logical.match_hints.driver_hint.as_deref(),
        Some("helios.sensor.bmi088")
    );
    assert_eq!(
        logical.properties.get("accel_range"),
        Some(&lemnos::core::Value::from("G6"))
    );
    assert_eq!(logical.links.len(), 3);

    let accel = descriptors
        .iter()
        .find(|descriptor| descriptor.id.as_str().ends_with(".endpoint.accel"))
        .expect("accel descriptor");
    assert_eq!(
        accel.address,
        Some(lemnos::core::DeviceAddress::I2cDevice {
            bus: 4,
            address: 0x18
        })
    );

    let accel_int = descriptors
        .iter()
        .find(|descriptor| descriptor.id.as_str().ends_with(".signal.accel_int"))
        .expect("accel interrupt descriptor");
    assert_eq!(accel_int.kind, lemnos::core::DeviceKind::GpioLine);
    assert_eq!(
        accel_int.properties.get("global_line"),
        Some(&lemnos::core::Value::from(311_u64))
    );

    assert_eq!(
        config
            .logical_device_id()
            .expect("logical device id")
            .as_str(),
        "helios.bmi088.bus4.accel0x18.gyro0x68"
    );

    let probe = ExampleImuConfig::configured_probe("macro-generated-probe", vec![config.clone()]);
    assert_eq!(probe.name(), "macro-generated-probe");
    assert_eq!(
        probe.interfaces(),
        &[
            lemnos::core::InterfaceKind::I2c,
            lemnos::core::InterfaceKind::Gpio
        ]
    );
}

#[test]
fn configured_device_derive_generates_spi_helpers() {
    let config = ExampleSpiConfig::builder()
        .bus(2_u32)
        .chip_select(0_u16)
        .label("board-flash")
        .build()
        .expect("spi config builder");

    assert_eq!(
        ExampleSpiConfig::CONFIGURED_DEVICE_INTERFACE,
        lemnos::core::InterfaceKind::Spi
    );
    assert_eq!(
        ExampleSpiConfig::CONFIGURED_DEVICE_INTERFACES,
        &[lemnos::core::InterfaceKind::Spi]
    );
    assert_eq!(
        config.configured_device_id(),
        "example.flash.bus2.flash0x00"
    );
    assert!(config.configured_i2c_endpoints().is_empty());
    assert_eq!(config.configured_spi_endpoints().len(), 1);

    let descriptors = config
        .configured_descriptors()
        .expect("spi configured descriptors");
    assert_eq!(descriptors.len(), 2);

    let endpoint = descriptors
        .iter()
        .find(|descriptor| descriptor.id.as_str().ends_with(".endpoint.flash"))
        .expect("spi endpoint descriptor");
    assert_eq!(
        endpoint.address,
        Some(lemnos::core::DeviceAddress::SpiDevice {
            bus: 2,
            chip_select: 0,
        })
    );
}

#[test]
fn enum_values_attribute_generates_typed_const_accessors() {
    assert_eq!(ExampleRate::Hz10.bits(), 0x00);
    assert_eq!(ExampleRate::Hz30.bits(), 0x07);
    assert_eq!(ExampleRate::Hz10.hertz(), 10.0);
    assert_eq!(ExampleRate::Hz30.hertz(), 30.0);
}
