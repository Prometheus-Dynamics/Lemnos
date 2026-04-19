use super::super::support::{output_config, pwm_config, spi_config, uart_config};
use crate::{Runtime, RuntimeConfig, RuntimeFailureCategory, RuntimeFailureOperation};
use lemnos_bus::BusError;
use lemnos_core::{
    DeviceRequest, GpioLevel, GpioRequest, I2cRequest, InteractionRequest, PwmConfiguration,
    PwmPolarity, PwmRequest, SpiRequest, StandardRequest, StandardResponse, UartRequest,
};
use lemnos_discovery::DiscoveryContext;
use lemnos_driver_sdk::DriverError;
use lemnos_drivers_gpio::GpioDriver;
use lemnos_drivers_i2c::I2cDriver;
use lemnos_drivers_pwm::PwmDriver;
use lemnos_drivers_spi::SpiDriver;
use lemnos_drivers_uart::UartDriver;
use lemnos_mock::{
    MockFaultScript, MockGpioLine, MockHardware, MockI2cDevice, MockPwmChannel, MockSpiDevice,
    MockUartPort,
};

#[test]
fn runtime_rebinds_after_mock_hotplug_cycle() {
    let hardware = MockHardware::builder().build();
    let line = MockGpioLine::new("gpiochip0", 11)
        .with_line_name("hotplug")
        .with_configuration(output_config());
    let device_id = line.descriptor().id.clone();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");

    let report = runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh empty inventory");
    assert!(report.discovery.snapshot.is_empty());
    assert!(!runtime.inventory().contains(&device_id));

    hardware.attach_gpio_line(line.clone());
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after attach");
    assert!(runtime.inventory().contains(&device_id));

    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect("write request before removal");
    assert!(runtime.is_bound(&device_id));
    assert_eq!(hardware.gpio_level(&device_id), Some(GpioLevel::High));

    assert!(hardware.remove_device(&device_id));
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after removal");
    assert!(!runtime.inventory().contains(&device_id));
    assert!(!runtime.is_bound(&device_id));
    assert!(runtime.state(&device_id).is_none());

    hardware.attach_gpio_line(line);
    let report = runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after reattach");
    assert_eq!(report.rebinds.attempted, vec![device_id.clone()]);
    assert_eq!(report.rebinds.rebound, vec![device_id.clone()]);
    assert!(runtime.inventory().contains(&device_id));
    assert!(runtime.is_bound(&device_id));

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Read)),
        ))
        .expect("read request after reattach");
    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Gpio(
            lemnos_core::GpioResponse::Level(GpioLevel::Low)
        ))
    );
    assert!(runtime.is_bound(&device_id));
}

#[test]
fn runtime_can_disable_auto_rebind_on_refresh() {
    let hardware = MockHardware::builder().build();
    let line = MockGpioLine::new("gpiochip0", 15)
        .with_line_name("no-auto-rebind")
        .with_configuration(output_config());
    let device_id = line.descriptor().id.clone();

    let mut runtime = Runtime::with_config(RuntimeConfig::new().with_auto_rebind_on_refresh(false));
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");

    hardware.attach_gpio_line(line.clone());
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("initial refresh");
    runtime.bind(&device_id).expect("bind");
    assert!(runtime.is_bound(&device_id));

    assert!(hardware.remove_device(&device_id));
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after removal");
    assert!(runtime.wants_binding(&device_id));
    assert!(!runtime.is_bound(&device_id));

    hardware.attach_gpio_line(line);
    let report = runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after reattach");

    assert!(report.rebinds.attempted.is_empty());
    assert!(runtime.inventory().contains(&device_id));
    assert!(runtime.wants_binding(&device_id));
    assert!(!runtime.is_bound(&device_id));
}

#[test]
fn runtime_rebinds_pwm_after_mock_hotplug_cycle() {
    let hardware = MockHardware::builder().build();
    let channel = MockPwmChannel::new("pwmchip0", 2).with_configuration(pwm_config());
    let device_id = channel.descriptor().id.clone();
    let context = DiscoveryContext::new();

    let mut runtime = Runtime::new();
    runtime.set_pwm_backend(hardware.clone());
    runtime.register_driver(PwmDriver).expect("register driver");

    runtime
        .refresh(&context, &[&hardware])
        .expect("refresh empty inventory");
    assert!(!runtime.inventory().contains(&device_id));

    hardware.attach_pwm_channel(channel.clone());
    runtime
        .refresh(&context, &[&hardware])
        .expect("refresh after attach");

    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Pwm(PwmRequest::Configure(
                PwmConfiguration {
                    period_ns: 25_000_000,
                    duty_cycle_ns: 12_500_000,
                    enabled: true,
                    polarity: PwmPolarity::Inversed,
                },
            ))),
        ))
        .expect("configure request before removal");
    assert!(runtime.is_bound(&device_id));
    assert_eq!(
        hardware.pwm_configuration(&device_id),
        Some(PwmConfiguration {
            period_ns: 25_000_000,
            duty_cycle_ns: 12_500_000,
            enabled: true,
            polarity: PwmPolarity::Inversed,
        })
    );

    assert!(hardware.remove_device(&device_id));
    runtime
        .refresh(&context, &[&hardware])
        .expect("refresh after removal");
    assert!(!runtime.inventory().contains(&device_id));
    assert!(!runtime.is_bound(&device_id));
    assert!(runtime.state(&device_id).is_none());

    hardware.attach_pwm_channel(channel);
    let report = runtime
        .refresh(&context, &[&hardware])
        .expect("refresh after reattach");
    assert_eq!(report.rebinds.attempted, vec![device_id.clone()]);
    assert_eq!(report.rebinds.rebound, vec![device_id.clone()]);
    assert!(runtime.inventory().contains(&device_id));
    assert!(runtime.is_bound(&device_id));

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Pwm(PwmRequest::Configure(
                PwmConfiguration {
                    period_ns: 20_000_000,
                    duty_cycle_ns: 5_000_000,
                    enabled: false,
                    polarity: PwmPolarity::Normal,
                },
            ))),
        ))
        .expect("configure request after reattach");
    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Pwm(
            lemnos_core::PwmResponse::Applied
        ))
    );
}

#[test]
fn runtime_rebinds_i2c_after_mock_hotplug_cycle() {
    let hardware = MockHardware::builder().build();
    let device = MockI2cDevice::new(1, 0x48).with_bytes(0x10, [0xAA, 0xBB, 0xCC]);
    let device_id = device.descriptor().id.clone();
    let context = DiscoveryContext::new();

    let mut runtime = Runtime::new();
    runtime.set_i2c_backend(hardware.clone());
    runtime.register_driver(I2cDriver).expect("register driver");

    runtime
        .refresh(&context, &[&hardware])
        .expect("refresh empty inventory");
    assert!(!runtime.inventory().contains(&device_id));

    hardware.attach_i2c_device(device.clone());
    runtime
        .refresh(&context, &[&hardware])
        .expect("refresh after attach");

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::I2c(I2cRequest::WriteRead {
                write: vec![0x10],
                read_length: 2,
            })),
        ))
        .expect("write_read request before removal");
    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::I2c(
            lemnos_core::I2cResponse::Bytes(vec![0xAA, 0xBB])
        ))
    );
    assert!(runtime.is_bound(&device_id));

    assert!(hardware.remove_device(&device_id));
    runtime
        .refresh(&context, &[&hardware])
        .expect("refresh after removal");
    assert!(!runtime.inventory().contains(&device_id));
    assert!(!runtime.is_bound(&device_id));
    assert!(runtime.state(&device_id).is_none());

    hardware.attach_i2c_device(device);
    let report = runtime
        .refresh(&context, &[&hardware])
        .expect("refresh after reattach");
    assert_eq!(report.rebinds.attempted, vec![device_id.clone()]);
    assert_eq!(report.rebinds.rebound, vec![device_id.clone()]);
    assert!(runtime.inventory().contains(&device_id));
    assert!(runtime.is_bound(&device_id));

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::I2c(I2cRequest::WriteRead {
                write: vec![0x10],
                read_length: 3,
            })),
        ))
        .expect("write_read request after reattach");
    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::I2c(
            lemnos_core::I2cResponse::Bytes(vec![0xAA, 0xBB, 0xCC])
        ))
    );
}

#[test]
fn runtime_rebinds_spi_after_mock_hotplug_cycle() {
    let hardware = MockHardware::builder().build();
    let device = MockSpiDevice::new(0, 1)
        .with_configuration(spi_config())
        .with_transfer_response([0x9F], [0x12, 0x34, 0x56]);
    let device_id = device.descriptor().id.clone();
    let context = DiscoveryContext::new();

    let mut runtime = Runtime::new();
    runtime.set_spi_backend(hardware.clone());
    runtime.register_driver(SpiDriver).expect("register driver");

    runtime
        .refresh(&context, &[&hardware])
        .expect("refresh empty inventory");
    assert!(!runtime.inventory().contains(&device_id));

    hardware.attach_spi_device(device.clone());
    runtime
        .refresh(&context, &[&hardware])
        .expect("refresh after attach");

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Spi(SpiRequest::Transfer {
                write: vec![0x9F],
            })),
        ))
        .expect("transfer request before removal");
    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Spi(
            lemnos_core::SpiResponse::Bytes(vec![0x12, 0x34, 0x56])
        ))
    );
    assert!(runtime.is_bound(&device_id));
    assert_eq!(hardware.spi_last_write(&device_id), Some(vec![0x9F]));

    assert!(hardware.remove_device(&device_id));
    runtime
        .refresh(&context, &[&hardware])
        .expect("refresh after removal");
    assert!(!runtime.inventory().contains(&device_id));
    assert!(!runtime.is_bound(&device_id));
    assert!(runtime.state(&device_id).is_none());

    hardware.attach_spi_device(device);
    let report = runtime
        .refresh(&context, &[&hardware])
        .expect("refresh after reattach");
    assert_eq!(report.rebinds.attempted, vec![device_id.clone()]);
    assert_eq!(report.rebinds.rebound, vec![device_id.clone()]);
    assert!(runtime.inventory().contains(&device_id));
    assert!(runtime.is_bound(&device_id));

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Spi(SpiRequest::Transfer {
                write: vec![0x9F],
            })),
        ))
        .expect("transfer request after reattach");
    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Spi(
            lemnos_core::SpiResponse::Bytes(vec![0x12, 0x34, 0x56])
        ))
    );
}

#[test]
fn runtime_rebinds_uart_after_mock_hotplug_cycle() {
    let hardware = MockHardware::builder().build();
    let port = MockUartPort::new("ttyUSB0")
        .with_configuration(uart_config())
        .with_rx_bytes([0x48, 0x69]);
    let device_id = port.descriptor().id.clone();
    let context = DiscoveryContext::new();

    let mut runtime = Runtime::new();
    runtime.set_uart_backend(hardware.clone());
    runtime
        .register_driver(UartDriver)
        .expect("register driver");

    runtime
        .refresh(&context, &[&hardware])
        .expect("refresh empty inventory");
    assert!(!runtime.inventory().contains(&device_id));

    hardware.attach_uart_port(port.clone());
    runtime
        .refresh(&context, &[&hardware])
        .expect("refresh after attach");

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Uart(UartRequest::Read { max_bytes: 2 })),
        ))
        .expect("read request before removal");
    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Uart(
            lemnos_core::UartResponse::Bytes(vec![0x48, 0x69])
        ))
    );
    assert!(runtime.is_bound(&device_id));

    assert!(hardware.remove_device(&device_id));
    runtime
        .refresh(&context, &[&hardware])
        .expect("refresh after removal");
    assert!(!runtime.inventory().contains(&device_id));
    assert!(!runtime.is_bound(&device_id));
    assert!(runtime.state(&device_id).is_none());

    hardware.attach_uart_port(port);
    let report = runtime
        .refresh(&context, &[&hardware])
        .expect("refresh after reattach");
    assert_eq!(report.rebinds.attempted, vec![device_id.clone()]);
    assert_eq!(report.rebinds.rebound, vec![device_id.clone()]);
    assert!(runtime.inventory().contains(&device_id));
    assert!(runtime.is_bound(&device_id));

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Uart(UartRequest::Read { max_bytes: 2 })),
        ))
        .expect("read request after reattach");
    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Uart(
            lemnos_core::UartResponse::Bytes(vec![0x48, 0x69])
        ))
    );
}

#[test]
fn runtime_tracks_and_clears_mock_request_failures() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 12)
                .with_line_name("faulty")
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

    hardware.queue_timeout(&device_id, "gpio.read");

    let err = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Read)),
        ))
        .expect_err("injected request failure should surface");
    assert!(matches!(
        err,
        crate::RuntimeError::Driver {
            device_id: failing_device_id,
            source,
        } if failing_device_id == device_id
            && matches!(
                source.as_ref(),
                DriverError::Transport {
                    source: BusError::Timeout {
                        operation: "gpio.read",
                        ..
                    },
                    ..
                }
            )
    ));

    let failure = runtime
        .failure(&device_id)
        .expect("failure should be tracked");
    assert_eq!(failure.operation, RuntimeFailureOperation::Request);
    assert_eq!(failure.category, RuntimeFailureCategory::Driver);
    assert!(failure.driver_id.is_some());

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Read)),
        ))
        .expect("retry should succeed after one-shot fault");
    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Gpio(
            lemnos_core::GpioResponse::Level(GpioLevel::Low)
        ))
    );
    assert!(runtime.failure(&device_id).is_none());
}

#[test]
fn runtime_tracks_and_clears_mock_bind_failures() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 13)
                .with_line_name("bind-fault")
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

    hardware.queue_timeout(&device_id, "open");

    let err = runtime
        .bind(&device_id)
        .expect_err("first bind should fail");
    assert!(matches!(
        err,
        crate::RuntimeError::Driver {
            device_id: failing_device_id,
            source,
        } if failing_device_id == device_id
            && matches!(
                source.as_ref(),
                DriverError::Transport {
                    source: BusError::Timeout {
                        operation: "open",
                        ..
                    },
                    ..
                }
            )
    ));

    let failure = runtime
        .failure(&device_id)
        .expect("bind failure should be tracked");
    assert_eq!(failure.operation, RuntimeFailureOperation::Bind);
    assert_eq!(failure.category, RuntimeFailureCategory::Driver);
    assert!(failure.driver_id.is_some());

    runtime
        .bind(&device_id)
        .expect("second bind should succeed");
    assert!(runtime.is_bound(&device_id));
    assert!(runtime.failure(&device_id).is_none());
}

#[test]
fn runtime_retries_through_scripted_mock_fault_sequence() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 14)
                .with_line_name("scripted")
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
            .timeout("open")
            .disconnect("open")
            .timeout("gpio.write"),
    );

    let first = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect_err("first scripted request should time out while opening");
    assert!(matches!(
        first,
        crate::RuntimeError::Driver {
            device_id: failing_device_id,
            source,
        } if failing_device_id == device_id
            && matches!(
                source.as_ref(),
                DriverError::Transport {
                    source: BusError::Timeout {
                        operation: "open",
                        ..
                    },
                    ..
                }
            )
    ));
    assert!(!runtime.is_bound(&device_id));

    let second = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect_err("second scripted request should disconnect while opening");
    assert!(matches!(
        second,
        crate::RuntimeError::Driver {
            device_id: failing_device_id,
            source,
        } if failing_device_id == device_id
            && matches!(
                source.as_ref(),
                DriverError::Transport {
                    source: BusError::Disconnected { .. },
                    ..
                }
            )
    ));
    assert!(!runtime.is_bound(&device_id));

    let third = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect_err("third scripted request should fail after binding");
    assert!(matches!(
        third,
        crate::RuntimeError::Driver {
            device_id: failing_device_id,
            source,
        } if failing_device_id == device_id
            && matches!(
                source.as_ref(),
                DriverError::Transport {
                    source: BusError::Timeout {
                        operation: "gpio.write",
                        ..
                    },
                    ..
                }
            )
    ));
    assert!(runtime.is_bound(&device_id));
    assert_eq!(
        runtime
            .failure(&device_id)
            .expect("failure should still be tracked")
            .operation,
        RuntimeFailureOperation::Request
    );

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect("scripted faults should be exhausted");
    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Gpio(
            lemnos_core::GpioResponse::Applied
        ))
    );
    assert!(runtime.failure(&device_id).is_none());
}
