use crate::{
    MockFaultScript, MockGpioLine, MockHardware, MockI2cDevice, MockPwmChannel, MockSpiDevice,
    MockUartPort, MockUsbDevice,
};
use lemnos_bus::{
    BusBackend, BusError, GpioBusBackend, I2cBusBackend, PwmBusBackend, SessionAccess,
    SpiBusBackend, UartBusBackend, UsbBusBackend,
    contract::{
        assert_close_contract, assert_gpio_round_trip_contract, assert_pwm_configuration_contract,
        assert_session_contract,
    },
};
use lemnos_core::{
    DeviceDescriptor, GpioDirection, GpioLevel, GpioLineConfiguration, I2cOperation, InterfaceKind,
    PwmConfiguration, PwmPolarity, SpiBitOrder, SpiConfiguration, SpiMode, UartConfiguration,
    UartDataBits, UartFlowControl, UartParity, UartStopBits, UsbControlSetup, UsbControlTransfer,
    UsbDirection, UsbInterruptTransfer, UsbRecipient, UsbRequestType,
};
use lemnos_discovery::{DiscoveryContext, DiscoveryProbe};
use std::thread;

fn runtime_gpio_config() -> GpioLineConfiguration {
    GpioLineConfiguration {
        direction: GpioDirection::Output,
        active_low: false,
        bias: None,
        drive: None,
        edge: None,
        debounce_us: None,
        initial_level: Some(GpioLevel::Low),
    }
}

fn runtime_pwm_config() -> PwmConfiguration {
    PwmConfiguration {
        period_ns: 20_000_000,
        duty_cycle_ns: 5_000_000,
        enabled: false,
        polarity: PwmPolarity::Normal,
    }
}

fn runtime_spi_config() -> SpiConfiguration {
    SpiConfiguration {
        mode: SpiMode::Mode3,
        max_frequency_hz: Some(4_000_000),
        bits_per_word: Some(8),
        bit_order: SpiBitOrder::MsbFirst,
    }
}

fn runtime_uart_config() -> UartConfiguration {
    UartConfiguration {
        baud_rate: 115_200,
        data_bits: UartDataBits::Eight,
        parity: UartParity::None,
        stop_bits: UartStopBits::One,
        flow_control: UartFlowControl::None,
    }
}

#[test]
fn discovery_returns_built_inventory_for_multiple_interfaces() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 4).with_line_name("status"))
        .with_pwm_channel(
            MockPwmChannel::new("pwmchip0", 0)
                .with_display_name("cooling-fan")
                .with_configuration(runtime_pwm_config()),
        )
        .with_i2c_device(MockI2cDevice::new(1, 0x50))
        .with_spi_device(
            MockSpiDevice::new(0, 1)
                .with_display_name("display-controller")
                .with_configuration(runtime_spi_config()),
        )
        .with_uart_port(
            MockUartPort::new("ttyUSB0")
                .with_display_name("debug-console")
                .with_configuration(runtime_uart_config()),
        )
        .with_usb_device(
            MockUsbDevice::new(1, [2])
                .with_display_name("imu")
                .with_vendor_product(0x1209, 0x0001)
                .with_interface(0),
        )
        .build();

    let discovery = hardware
        .discover(&DiscoveryContext::new())
        .expect("discover");

    assert_eq!(discovery.devices.len(), 7);
    assert_eq!(
        discovery
            .devices
            .iter()
            .filter(|device| device.interface == InterfaceKind::Gpio)
            .count(),
        1
    );
    assert_eq!(
        discovery
            .devices
            .iter()
            .filter(|device| device.interface == InterfaceKind::Pwm)
            .count(),
        1
    );
    assert_eq!(
        discovery
            .devices
            .iter()
            .filter(|device| device.interface == InterfaceKind::I2c)
            .count(),
        1
    );
    assert_eq!(
        discovery
            .devices
            .iter()
            .filter(|device| device.interface == InterfaceKind::Spi)
            .count(),
        1
    );
    assert_eq!(
        discovery
            .devices
            .iter()
            .filter(|device| device.interface == InterfaceKind::Uart)
            .count(),
        1
    );
    assert_eq!(
        discovery
            .devices
            .iter()
            .filter(|device| device.interface == InterfaceKind::Usb)
            .count(),
        2
    );
}

#[test]
fn gpio_backend_round_trips_configuration_and_level() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 4).with_configuration(runtime_gpio_config()))
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == InterfaceKind::Gpio)
        .expect("gpio device");
    let mut session = hardware
        .open_gpio(&device, SessionAccess::Exclusive)
        .expect("open gpio");

    assert_session_contract(
        session.as_ref(),
        InterfaceKind::Gpio,
        &device,
        BusBackend::name(&hardware),
        SessionAccess::Exclusive,
    );
    assert_gpio_round_trip_contract(
        session.as_mut(),
        GpioLevel::Low,
        GpioLevel::High,
        GpioDirection::Output,
    );
    assert_close_contract(session.as_mut());
}

#[test]
fn i2c_backend_round_trips_register_reads_and_writes() {
    let hardware = MockHardware::builder()
        .with_i2c_device(MockI2cDevice::new(1, 0x50).with_bytes(0x10, [0xAA, 0xBB, 0xCC]))
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == InterfaceKind::I2c)
        .expect("i2c device");
    let mut session = hardware
        .open_i2c(&device, SessionAccess::Exclusive)
        .expect("open i2c");

    let bytes = session.write_read(&[0x10], 2).expect("write_read");
    assert_eq!(bytes, vec![0xAA, 0xBB]);

    session.write(&[0x12, 0x11, 0x22]).expect("write");
    assert_eq!(
        hardware.i2c_bytes(&device.id, 0x12, 2),
        Some(vec![0x11, 0x22])
    );

    let transaction = session
        .transaction(&[
            I2cOperation::Write { bytes: vec![0x10] },
            I2cOperation::Read { length: 3 },
        ])
        .expect("transaction");
    assert_eq!(transaction.len(), 2);
    assert_eq!(transaction[1], vec![0xAA, 0xBB, 0x11]);
}

#[test]
fn i2c_device_helpers_encode_common_register_values() {
    let hardware = MockHardware::builder()
        .with_i2c_device(
            MockI2cDevice::new(2, 0x40)
                .with_u8(0x00, 0xAB)
                .with_be_u16(0x01, 0x1234)
                .with_le_u16(0x03, 0x5678)
                .with_be_i16(0x05, -2)
                .with_le_i16(0x07, -3),
        )
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == InterfaceKind::I2c)
        .expect("i2c device");

    assert_eq!(hardware.i2c_bytes(&device.id, 0x00, 1), Some(vec![0xAB]));
    assert_eq!(
        hardware.i2c_bytes(&device.id, 0x01, 2),
        Some(vec![0x12, 0x34])
    );
    assert_eq!(
        hardware.i2c_bytes(&device.id, 0x03, 2),
        Some(vec![0x78, 0x56])
    );
    assert_eq!(
        hardware.i2c_bytes(&device.id, 0x05, 2),
        Some(vec![0xFF, 0xFE])
    );
    assert_eq!(
        hardware.i2c_bytes(&device.id, 0x07, 2),
        Some(vec![0xFD, 0xFF])
    );
}

#[test]
fn i2c_controller_session_can_access_multiple_addresses_on_one_bus() {
    let hardware = MockHardware::builder()
        .with_i2c_device(MockI2cDevice::new(4, 0x18).with_u8(0x00, 0x1E))
        .with_i2c_device(MockI2cDevice::new(4, 0x68).with_u8(0x00, 0x0F))
        .build();
    let owner = DeviceDescriptor::new("mock.bmi088", InterfaceKind::I2c).expect("owner");
    let mut controller = hardware
        .open_i2c_controller(&owner, 4, SessionAccess::ExclusiveController)
        .expect("open i2c controller");

    let accel = controller
        .write_read(0x18, &[0x00], 1)
        .expect("read accel chip id");
    let gyro = controller
        .write_read(0x68, &[0x00], 1)
        .expect("read gyro chip id");

    assert_eq!(controller.bus(), 4);
    assert_eq!(accel, vec![0x1E]);
    assert_eq!(gyro, vec![0x0F]);
}

#[test]
fn pwm_backend_round_trips_configuration_updates() {
    let hardware = MockHardware::builder()
        .with_pwm_channel(
            MockPwmChannel::new("pwmchip0", 0).with_configuration(runtime_pwm_config()),
        )
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == InterfaceKind::Pwm)
        .expect("pwm device");
    let mut session = hardware
        .open_pwm(&device, SessionAccess::Exclusive)
        .expect("open pwm");
    let updated = PwmConfiguration {
        period_ns: 25_000_000,
        duty_cycle_ns: 7_500_000,
        enabled: true,
        polarity: PwmPolarity::Normal,
    };

    assert_session_contract(
        session.as_ref(),
        InterfaceKind::Pwm,
        &device,
        BusBackend::name(&hardware),
        SessionAccess::Exclusive,
    );
    assert_pwm_configuration_contract(session.as_mut(), &runtime_pwm_config(), &updated);
    assert_eq!(
        hardware.pwm_configuration(&device.id),
        Some(updated.clone())
    );
    assert_close_contract(session.as_mut());
}

#[test]
fn spi_backend_round_trips_transfers_and_configuration() {
    let hardware = MockHardware::builder()
        .with_spi_device(
            MockSpiDevice::new(0, 1)
                .with_configuration(runtime_spi_config())
                .with_transfer_response([0x9F], [0x12, 0x34, 0x56]),
        )
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == InterfaceKind::Spi)
        .expect("spi device");
    let mut session = hardware
        .open_spi(&device, SessionAccess::Exclusive)
        .expect("open spi");

    let bytes = session.transfer(&[0x9F]).expect("transfer");
    assert_eq!(bytes, vec![0x12, 0x34, 0x56]);

    session.write(&[0xAA, 0x55]).expect("write");
    assert_eq!(hardware.spi_last_write(&device.id), Some(vec![0xAA, 0x55]));

    session
        .configure(&SpiConfiguration {
            mode: SpiMode::Mode1,
            max_frequency_hz: Some(8_000_000),
            bits_per_word: Some(16),
            bit_order: SpiBitOrder::LsbFirst,
        })
        .expect("configure");
    assert_eq!(
        hardware.spi_configuration(&device.id),
        Some(SpiConfiguration {
            mode: SpiMode::Mode1,
            max_frequency_hz: Some(8_000_000),
            bits_per_word: Some(16),
            bit_order: SpiBitOrder::LsbFirst,
        })
    );
}

#[test]
fn uart_backend_round_trips_reads_writes_and_configuration() {
    let hardware = MockHardware::builder()
        .with_uart_port(
            MockUartPort::new("ttyUSB0")
                .with_configuration(runtime_uart_config())
                .with_rx_bytes([0x48, 0x65, 0x79]),
        )
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == InterfaceKind::Uart)
        .expect("uart device");
    let mut session = hardware
        .open_uart(&device, SessionAccess::Exclusive)
        .expect("open uart");

    let bytes = session.read(2).expect("read");
    assert_eq!(bytes, vec![0x48, 0x65]);
    assert_eq!(hardware.uart_rx_bytes(&device.id), Some(vec![0x79]));

    session.write(&[0xAA, 0x55]).expect("write");
    assert_eq!(hardware.uart_tx_bytes(&device.id), Some(vec![0xAA, 0x55]));

    session
        .configure(&UartConfiguration {
            baud_rate: 57_600,
            data_bits: UartDataBits::Seven,
            parity: UartParity::Even,
            stop_bits: UartStopBits::Two,
            flow_control: UartFlowControl::Hardware,
        })
        .expect("configure");
    assert_eq!(
        hardware.uart_configuration(&device.id),
        Some(UartConfiguration {
            baud_rate: 57_600,
            data_bits: UartDataBits::Seven,
            parity: UartParity::Even,
            stop_bits: UartStopBits::Two,
            flow_control: UartFlowControl::Hardware,
        })
    );
    session.flush().expect("flush");
}

#[test]
fn usb_backend_round_trips_claims_and_transfers() {
    let hardware = MockHardware::builder()
        .with_usb_device(
            MockUsbDevice::new(1, [2])
                .with_vendor_product(0x1209, 0x0001)
                .with_interface(0)
                .with_control_response(
                    UsbControlTransfer {
                        setup: UsbControlSetup {
                            direction: UsbDirection::In,
                            request_type: UsbRequestType::Vendor,
                            recipient: UsbRecipient::Interface,
                            request: 0x01,
                            value: 0,
                            index: 0,
                        },
                        data: vec![0; 4],
                        timeout_ms: Some(100),
                    },
                    [0x10, 0x20, 0x30, 0x40],
                )
                .with_bulk_in_response(0x81, [0xAA, 0xBB, 0xCC])
                .with_interrupt_in_response(0x82, [0x01, 0x02]),
        )
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::UsbInterface)
        .expect("usb interface");
    let owner_id = hardware
        .descriptors()
        .into_iter()
        .find(|entry| entry.kind == lemnos_core::DeviceKind::UsbDevice)
        .expect("usb device")
        .id;
    let mut session = hardware
        .open_usb(&device, SessionAccess::ExclusiveController)
        .expect("open usb");

    session
        .claim_interface(0, Some(1))
        .expect("claim interface");
    assert_eq!(hardware.usb_claimed_interfaces(&owner_id), Some(vec![0]));

    let control = session
        .control_transfer(&UsbControlTransfer {
            setup: UsbControlSetup {
                direction: UsbDirection::In,
                request_type: UsbRequestType::Vendor,
                recipient: UsbRecipient::Interface,
                request: 0x01,
                value: 0,
                index: 0,
            },
            data: vec![0; 4],
            timeout_ms: Some(100),
        })
        .expect("control transfer");
    assert_eq!(control, vec![0x10, 0x20, 0x30, 0x40]);

    let bulk = session.bulk_read(0x81, 3, Some(100)).expect("bulk read");
    assert_eq!(bulk, vec![0xAA, 0xBB, 0xCC]);

    session
        .bulk_write(0x01, &[0x55, 0x66], Some(100))
        .expect("bulk write");
    assert_eq!(
        hardware.usb_last_bulk_write(&owner_id, 0x01),
        Some(vec![0x55, 0x66])
    );

    let interrupt = session
        .interrupt_read(0x82, 2, Some(100))
        .expect("interrupt read");
    assert_eq!(interrupt, vec![0x01, 0x02]);

    session
        .interrupt_write(&UsbInterruptTransfer {
            endpoint: 0x02,
            bytes: vec![0x90, 0x91],
            timeout_ms: Some(100),
        })
        .expect("interrupt write");
    assert_eq!(
        hardware.usb_last_interrupt_write(&owner_id, 0x02),
        Some(vec![0x90, 0x91])
    );

    session.release_interface(0).expect("release interface");
    assert_eq!(hardware.usb_claimed_interfaces(&owner_id), Some(Vec::new()));
}

#[test]
fn hotplug_attach_and_remove_updates_discovered_inventory() {
    let hardware = MockHardware::builder().build();

    let discovery = hardware
        .discover(&DiscoveryContext::new())
        .expect("discover empty inventory");
    assert!(discovery.devices.is_empty());

    let line = MockGpioLine::new("gpiochip0", 9)
        .with_line_name("hotplug")
        .with_configuration(runtime_gpio_config());
    let device_id = line.descriptor().id.clone();
    let attached_id = hardware.attach_gpio_line(line);
    assert_eq!(attached_id, device_id);

    let discovery = hardware
        .discover(&DiscoveryContext::new())
        .expect("discover attached inventory");
    assert_eq!(discovery.devices.len(), 1);
    assert_eq!(discovery.devices[0].id, device_id);

    assert!(hardware.remove_device(&device_id));
    assert!(!hardware.remove_device(&device_id));

    let discovery = hardware
        .discover(&DiscoveryContext::new())
        .expect("discover after removal");
    assert!(discovery.devices.is_empty());
}

#[test]
fn queued_fault_is_returned_once() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 5).with_configuration(runtime_gpio_config()))
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == InterfaceKind::Gpio)
        .expect("gpio device");
    let mut session = hardware
        .open_gpio(&device, SessionAccess::Exclusive)
        .expect("open gpio");

    hardware.queue_timeout(&device.id, "gpio.read");

    assert_eq!(
        session
            .read_level()
            .expect_err("first read should use injected timeout"),
        BusError::Timeout {
            device_id: device.id.clone(),
            operation: "gpio.read",
        }
    );
    assert_eq!(
        session.read_level().expect("second read should succeed"),
        GpioLevel::Low
    );
}

#[test]
fn fault_script_replays_ordered_sequence_across_operations() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 6).with_configuration(runtime_gpio_config()))
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == InterfaceKind::Gpio)
        .expect("gpio device");
    let mut session = hardware
        .open_gpio(&device, SessionAccess::Exclusive)
        .expect("open gpio");

    hardware.queue_script(
        &device.id,
        MockFaultScript::new()
            .timeout("gpio.read")
            .disconnect("gpio.read")
            .transport_failure("gpio.write", "scripted write failure"),
    );

    assert_eq!(
        session
            .read_level()
            .expect_err("first scripted read should time out"),
        BusError::Timeout {
            device_id: device.id.clone(),
            operation: "gpio.read",
        }
    );
    assert_eq!(
        session
            .read_level()
            .expect_err("second scripted read should disconnect"),
        BusError::Disconnected {
            device_id: device.id.clone(),
        }
    );
    assert_eq!(
        session
            .write_level(GpioLevel::High)
            .expect_err("scripted write should transport-fail"),
        BusError::TransportFailure {
            device_id: device.id.clone(),
            operation: "gpio.write",
            reason: "scripted write failure".into(),
        }
    );
    assert_eq!(
        session.read_level().expect("script should be exhausted"),
        GpioLevel::Low
    );
}

#[test]
fn removing_usb_interface_clears_pending_faults_for_reattached_device() {
    let usb = MockUsbDevice::new(1, [3])
        .with_vendor_product(0x1209, 0x0002)
        .with_interface(0);
    let hardware = MockHardware::builder().with_usb_device(usb.clone()).build();
    let interface = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::UsbInterface)
        .expect("usb interface");

    hardware.queue_timeout(&interface.id, "open");
    assert!(hardware.remove_device(&interface.id));

    hardware.attach_usb_device(usb);
    let interface = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::UsbInterface)
        .expect("reattached usb interface");

    let mut session = hardware
        .open_usb(&interface, SessionAccess::ExclusiveController)
        .expect("open usb after reattach");
    session
        .claim_interface(0, None)
        .expect("claim interface after reattach");
}

#[test]
fn concurrent_sessions_on_unrelated_mock_devices_do_not_deadlock() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 7).with_configuration(runtime_gpio_config()))
        .with_uart_port(
            MockUartPort::new("ttyUSB1")
                .with_configuration(runtime_uart_config())
                .with_rx_bytes([0x10, 0x11, 0x12, 0x13]),
        )
        .build();
    let gpio = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == InterfaceKind::Gpio)
        .expect("gpio device");
    let uart = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.interface == InterfaceKind::Uart)
        .expect("uart device");

    thread::scope(|scope| {
        let gpio_hardware = hardware.clone();
        let gpio_device = gpio.clone();
        let gpio_worker = scope.spawn(move || {
            let mut session = gpio_hardware
                .open_gpio(&gpio_device, SessionAccess::Exclusive)
                .expect("open gpio");
            for _ in 0..32 {
                session.write_level(GpioLevel::High).expect("gpio high");
                session.write_level(GpioLevel::Low).expect("gpio low");
                assert_eq!(session.read_level().expect("gpio read"), GpioLevel::Low);
            }
        });

        let uart_hardware = hardware.clone();
        let uart_device = uart.clone();
        let uart_worker = scope.spawn(move || {
            let mut session = uart_hardware
                .open_uart(&uart_device, SessionAccess::Exclusive)
                .expect("open uart");
            for expected in [[0x10_u8], [0x11_u8], [0x12_u8], [0x13_u8]] {
                assert_eq!(session.read(1).expect("uart read"), expected);
                session.write(&expected).expect("uart write");
            }
            session.flush().expect("uart flush");
        });

        gpio_worker.join().expect("gpio worker");
        uart_worker.join().expect("uart worker");
    });

    assert_eq!(hardware.gpio_level(&gpio.id), Some(GpioLevel::Low));
    assert_eq!(
        hardware.uart_tx_bytes(&uart.id),
        Some(vec![0x10, 0x11, 0x12, 0x13])
    );
}
