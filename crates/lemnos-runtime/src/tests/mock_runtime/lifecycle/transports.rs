use super::*;
use lemnos_core::{
    DeviceRequest, GpioRequest, I2cRequest, InteractionResponse, PwmConfiguration, PwmRequest,
    SpiRequest, StandardResponse, UartRequest,
};
use lemnos_drivers_gpio::GpioDriver;
use lemnos_drivers_i2c::I2cDriver;
use lemnos_drivers_pwm::PwmDriver;
use lemnos_drivers_spi::SpiDriver;
use lemnos_drivers_uart::UartDriver;
use lemnos_mock::{
    MockGpioLine, MockHardware, MockI2cDevice, MockPwmChannel, MockSpiDevice, MockUartPort,
};

#[test]
fn runtime_dispatches_pwm_requests_and_tracks_state() {
    let hardware = MockHardware::builder()
        .with_pwm_channel(MockPwmChannel::new("pwmchip0", 0).with_configuration(pwm_config()))
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::new();
    runtime.set_pwm_backend(hardware.clone());
    runtime.register_driver(PwmDriver).expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Pwm(PwmRequest::Configure(
                PwmConfiguration {
                    period_ns: 25_000_000,
                    duty_cycle_ns: 10_000_000,
                    enabled: true,
                    polarity: PwmPolarity::Inversed,
                },
            ))),
        ))
        .expect("configure request");

    assert_eq!(
        response.interaction,
        InteractionResponse::Standard(StandardResponse::Pwm(lemnos_core::PwmResponse::Applied))
    );
    assert!(runtime.is_bound(&device_id));
    assert_eq!(
        hardware.pwm_configuration(&device_id),
        Some(PwmConfiguration {
            period_ns: 25_000_000,
            duty_cycle_ns: 10_000_000,
            enabled: true,
            polarity: PwmPolarity::Inversed,
        })
    );
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("state")
            .realized_config
            .get("polarity"),
        Some(&"inversed".into())
    );
}

#[test]
fn runtime_dispatches_i2c_requests_and_tracks_state() {
    let hardware = MockHardware::builder()
        .with_i2c_device(MockI2cDevice::new(1, 0x48).with_bytes(0x10, [0xAA, 0xBB, 0xCC]))
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::new();
    runtime.set_i2c_backend(hardware.clone());
    runtime.register_driver(I2cDriver).expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::I2c(I2cRequest::WriteRead {
                write: vec![0x10],
                read_length: 2,
            })),
        ))
        .expect("write_read request");

    assert_eq!(
        response.interaction,
        InteractionResponse::Standard(StandardResponse::I2c(lemnos_core::I2cResponse::Bytes(
            vec![0xAA, 0xBB]
        )))
    );
    assert!(runtime.is_bound(&device_id));
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("state")
            .telemetry
            .get("write_read_ops"),
        Some(&1_u64.into())
    );
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("state")
            .realized_config
            .get("address"),
        Some(&0x48_u64.into())
    );
}

#[test]
fn runtime_dispatches_spi_requests_and_tracks_state() {
    let hardware = MockHardware::builder()
        .with_spi_device(
            MockSpiDevice::new(0, 1)
                .with_configuration(spi_config())
                .with_transfer_response([0x9F], [0x12, 0x34, 0x56]),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::new();
    runtime.set_spi_backend(hardware.clone());
    runtime.register_driver(SpiDriver).expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Spi(SpiRequest::Transfer {
                write: vec![0x9F],
            })),
        ))
        .expect("transfer request");

    assert_eq!(
        response.interaction,
        InteractionResponse::Standard(StandardResponse::Spi(lemnos_core::SpiResponse::Bytes(
            vec![0x12, 0x34, 0x56]
        )))
    );
    assert!(runtime.is_bound(&device_id));
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("state")
            .telemetry
            .get("transfer_ops"),
        Some(&1_u64.into())
    );
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("state")
            .realized_config
            .get("mode"),
        Some(&"mode0".into())
    );
}

#[test]
fn runtime_dispatches_uart_requests_and_tracks_state() {
    let hardware = MockHardware::builder()
        .with_uart_port(
            MockUartPort::new("ttyUSB0")
                .with_configuration(uart_config())
                .with_rx_bytes([0x48, 0x69]),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::new();
    runtime.set_uart_backend(hardware.clone());
    runtime
        .register_driver(UartDriver)
        .expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Uart(UartRequest::Read { max_bytes: 2 })),
        ))
        .expect("read request");

    assert_eq!(
        response.interaction,
        InteractionResponse::Standard(StandardResponse::Uart(lemnos_core::UartResponse::Bytes(
            vec![0x48, 0x69]
        )))
    );
    assert!(runtime.is_bound(&device_id));
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("state")
            .telemetry
            .get("read_ops"),
        Some(&1_u64.into())
    );
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("state")
            .realized_config
            .get("port"),
        Some(&"ttyUSB0".into())
    );
}

#[test]
fn runtime_tracks_invalid_request_failures_and_clears_them_after_success() {
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
        .expect("refresh");

    let err = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Pwm(PwmRequest::Configure(pwm_config()))),
        ))
        .expect_err("mismatched interface should fail request validation");
    assert!(matches!(err, crate::RuntimeError::InvalidRequest { .. }));

    let failure = runtime
        .failure(&device_id)
        .expect("failure should be tracked");
    assert_eq!(failure.operation, RuntimeFailureOperation::Request);
    assert_eq!(failure.category, RuntimeFailureCategory::InvalidRequest);
    assert_eq!(failure.driver_id, None);

    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect("valid request");

    assert!(runtime.failure(&device_id).is_none());
}
