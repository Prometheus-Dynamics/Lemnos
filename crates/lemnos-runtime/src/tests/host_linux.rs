use super::support::{HostGpioFixture, HostI2cFixture, HostSpiFixture, HostUsbFixture};
use crate::Runtime;
use lemnos_core::{
    DeviceAddress, DeviceRequest, GpioDirection, GpioLevel, GpioRequest, I2cRequest,
    InteractionRequest, SpiRequest, StandardRequest, StandardResponse, UsbControlSetup,
    UsbControlTransfer, UsbDirection, UsbRecipient, UsbRequest, UsbRequestType, UsbResponse,
};
use lemnos_discovery::{DiscoveryContext, DiscoveryProbe};
use lemnos_drivers_gpio::GpioDriver;
use lemnos_drivers_i2c::I2cDriver;
use lemnos_drivers_spi::SpiDriver;
use lemnos_drivers_usb::UsbDriver;
use lemnos_linux::LinuxBackend;

#[test]
#[ignore = "requires a real Linux GPIO target; set LEMNOS_TEST_GPIO_CHIP and LEMNOS_TEST_GPIO_OFFSET. Optional: LEMNOS_TEST_GPIO_EXPECT_LEVEL=0|1|low|high"]
fn runtime_refreshes_inventory_and_dispatches_gpio_requests_through_host_linux_backend() {
    let fixture = HostGpioFixture::from_env().expect("load host GPIO fixture from env");
    let backend = LinuxBackend::new();
    let gpio_probe = backend.gpio_probe();
    let context = DiscoveryContext::new();
    let device = gpio_probe
        .discover(&context)
        .expect("discover host gpio devices")
        .devices
        .into_iter()
        .find(|device| {
            matches!(
                device.address.as_ref(),
                Some(DeviceAddress::GpioLine { chip_name, offset })
                    if chip_name == &fixture.chip_name && *offset == fixture.offset
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "failed to find Linux GPIO line '{}:{}'",
                fixture.chip_name, fixture.offset
            )
        });
    let device_id = device.id.clone();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(backend);
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    runtime.refresh(&context, &[&gpio_probe]).expect("refresh");

    let configuration_response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::GetConfiguration)),
        ))
        .expect("dispatch host-backed gpio get_configuration request");
    let configuration = match configuration_response.interaction {
        lemnos_core::InteractionResponse::Standard(StandardResponse::Gpio(
            lemnos_core::GpioResponse::Configuration(configuration),
        )) => configuration,
        other => panic!("unexpected GPIO configuration response: {other:?}"),
    };

    let read_response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Read)),
        ))
        .expect("dispatch host-backed gpio read request");
    let level = match read_response.interaction {
        lemnos_core::InteractionResponse::Standard(StandardResponse::Gpio(
            lemnos_core::GpioResponse::Level(level),
        )) => level,
        other => panic!("unexpected GPIO read response: {other:?}"),
    };

    if let Some(expected_level) = fixture.expected_level {
        assert_eq!(level, expected_level);
    }
    assert!(runtime.is_bound(&device_id));

    let state = runtime.state(&device_id).expect("gpio state");
    assert_eq!(
        state.realized_config.get("direction"),
        Some(
            &match configuration.direction {
                GpioDirection::Input => "input",
                GpioDirection::Output => "output",
            }
            .into()
        )
    );
    assert_eq!(
        state.telemetry.get("level"),
        Some(
            &match level {
                GpioLevel::Low => "low",
                GpioLevel::High => "high",
            }
            .into()
        )
    );
}

#[test]
#[ignore = "requires a real Linux I2C target; set LEMNOS_TEST_I2C_BUS, LEMNOS_TEST_I2C_ADDRESS, LEMNOS_TEST_I2C_WRITE_HEX, and LEMNOS_TEST_I2C_EXPECT_READ_HEX"]
fn runtime_refreshes_inventory_and_dispatches_i2c_requests_through_host_linux_backend() {
    let fixture = HostI2cFixture::from_env().expect("load host I2C fixture from env");
    let backend = LinuxBackend::new();
    let i2c_probe = backend.i2c_probe();
    let context = DiscoveryContext::new();
    let device_id = i2c_probe
        .discover(&context)
        .expect("discover host i2c devices")
        .devices
        .into_iter()
        .find(|device| {
            matches!(
                device.address.as_ref(),
                Some(DeviceAddress::I2cDevice { bus, address })
                    if *bus == fixture.bus && *address == fixture.address
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "failed to find Linux I2C device on bus {} address 0x{:02x}",
                fixture.bus, fixture.address
            )
        })
        .id;

    let mut runtime = Runtime::new();
    runtime.set_i2c_backend(backend);
    runtime.register_driver(I2cDriver).expect("register driver");
    runtime.refresh(&context, &[&i2c_probe]).expect("refresh");

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::I2c(I2cRequest::WriteRead {
                write: fixture.write.clone(),
                read_length: fixture.expected_read.len() as u32,
            })),
        ))
        .expect("dispatch host-backed i2c write_read request");

    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::I2c(
            lemnos_core::I2cResponse::Bytes(fixture.expected_read.clone())
        ))
    );
    assert!(runtime.is_bound(&device_id));
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("i2c state")
            .telemetry
            .get("write_read_ops"),
        Some(&1_u64.into())
    );
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("i2c state")
            .telemetry
            .get("bytes_written"),
        Some(&(fixture.write.len() as u64).into())
    );
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("i2c state")
            .telemetry
            .get("bytes_read"),
        Some(&(fixture.expected_read.len() as u64).into())
    );
}

#[test]
#[ignore = "requires a real Linux SPI target; set LEMNOS_TEST_SPI_BUS, LEMNOS_TEST_SPI_CHIP_SELECT, LEMNOS_TEST_SPI_TRANSFER_HEX, and LEMNOS_TEST_SPI_EXPECT_READ_HEX"]
fn runtime_refreshes_inventory_and_dispatches_spi_requests_through_host_linux_backend() {
    let fixture = HostSpiFixture::from_env().expect("load host SPI fixture from env");
    let backend = LinuxBackend::new();
    let spi_probe = backend.spi_probe();
    let context = DiscoveryContext::new();
    let device_id = spi_probe
        .discover(&context)
        .expect("discover host spi devices")
        .devices
        .into_iter()
        .find(|device| {
            matches!(
                device.address.as_ref(),
                Some(DeviceAddress::SpiDevice { bus, chip_select })
                    if *bus == fixture.bus && *chip_select == fixture.chip_select
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "failed to find Linux SPI device on bus {} chip-select {}",
                fixture.bus, fixture.chip_select
            )
        })
        .id;

    let mut runtime = Runtime::new();
    runtime.set_spi_backend(backend);
    runtime.register_driver(SpiDriver).expect("register driver");
    runtime.refresh(&context, &[&spi_probe]).expect("refresh");

    if let Some(configuration) = fixture.configuration.clone() {
        let configure_response = runtime
            .request(DeviceRequest::new(
                device_id.clone(),
                InteractionRequest::Standard(StandardRequest::Spi(SpiRequest::Configure(
                    configuration.clone(),
                ))),
            ))
            .expect("configure host-backed spi target");
        assert_eq!(
            configure_response.interaction,
            lemnos_core::InteractionResponse::Standard(StandardResponse::Spi(
                lemnos_core::SpiResponse::Applied
            ))
        );
    }

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Spi(SpiRequest::Transfer {
                write: fixture.write.clone(),
            })),
        ))
        .expect("dispatch host-backed spi transfer");

    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Spi(
            lemnos_core::SpiResponse::Bytes(fixture.expected_read.clone())
        ))
    );
    assert!(runtime.is_bound(&device_id));
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("spi state")
            .telemetry
            .get("transfer_ops"),
        Some(&1_u64.into())
    );
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("spi state")
            .telemetry
            .get("bytes_written"),
        Some(&(fixture.write.len() as u64).into())
    );
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("spi state")
            .telemetry
            .get("bytes_read"),
        Some(&(fixture.expected_read.len() as u64).into())
    );
}

#[test]
#[ignore = "requires a real Linux USB target; set LEMNOS_TEST_USB_BUS, LEMNOS_TEST_USB_PORTS, and LEMNOS_TEST_USB_INTERFACE"]
fn runtime_refreshes_inventory_and_dispatches_usb_requests_through_host_linux_backend() {
    let fixture = HostUsbFixture::from_env().expect("load host USB fixture from env");
    let backend = LinuxBackend::new();
    let usb_probe = backend.usb_probe();
    let context = DiscoveryContext::new();
    let device = usb_probe
        .discover(&context)
        .expect("discover host usb devices")
        .devices
        .into_iter()
        .find(|device| {
            matches!(
                device.address.as_ref(),
                Some(DeviceAddress::UsbInterface {
                    bus,
                    ports,
                    interface_number,
                    ..
                }) if *bus == fixture.bus
                    && *ports == fixture.ports
                    && *interface_number == fixture.interface_number
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "failed to find Linux USB interface on bus {} ports {:?} interface {}",
                fixture.bus, fixture.ports, fixture.interface_number
            )
        });
    let device_id = device.id.clone();
    let (vendor_id, product_id) = match device.address.as_ref() {
        Some(DeviceAddress::UsbInterface {
            vendor_id,
            product_id,
            ..
        }) => (*vendor_id, *product_id),
        _ => (None, None),
    };

    let mut runtime = Runtime::new();
    runtime.set_usb_backend(backend);
    runtime.register_driver(UsbDriver).expect("register driver");
    runtime.refresh(&context, &[&usb_probe]).expect("refresh");

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::Control(
                UsbControlTransfer {
                    setup: UsbControlSetup {
                        direction: UsbDirection::In,
                        request_type: UsbRequestType::Standard,
                        recipient: UsbRecipient::Device,
                        request: 0x06,
                        value: 0x0100,
                        index: 0,
                    },
                    data: vec![0; 18],
                    timeout_ms: Some(250),
                },
            ))),
        ))
        .expect("dispatch host-backed usb control transfer");

    let bytes = match response.interaction {
        lemnos_core::InteractionResponse::Standard(StandardResponse::Usb(UsbResponse::Bytes(
            bytes,
        ))) => bytes,
        other => panic!("unexpected USB response: {other:?}"),
    };
    assert_eq!(bytes.len(), 18);
    assert_eq!(bytes[0], 18);
    assert_eq!(bytes[1], 0x01);
    if let Some(vendor_id) = vendor_id {
        assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), vendor_id);
    }
    if let Some(product_id) = product_id {
        assert_eq!(u16::from_le_bytes([bytes[10], bytes[11]]), product_id);
    }
    assert!(runtime.is_bound(&device_id));
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("usb state")
            .telemetry
            .get("control_ops"),
        Some(&1_u64.into())
    );
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("usb state")
            .telemetry
            .get("bytes_read"),
        Some(&18_u64.into())
    );
}
