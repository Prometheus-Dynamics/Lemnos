use super::*;

#[test]
fn runtime_surfaces_linux_spi_transport_failures_during_bind() {
    let root = TestRoot::new();
    root.create_dir("sys/bus/spi/devices/spi0.1");
    root.write("sys/bus/spi/devices/spi0.1/modalias", "spi:fake-display\n");
    root.touch("dev/spidev0.1");

    let backend = LinuxBackend::with_paths(root.paths());
    let spi_probe = backend.spi_probe();
    let device_id = spi_probe
        .discover(&DiscoveryContext::new())
        .expect("discover spi devices")
        .devices
        .into_iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::SpiDevice)
        .expect("spi device")
        .id;

    let mut runtime = Runtime::new();
    runtime.set_spi_backend(backend.clone());
    runtime.register_driver(SpiDriver).expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&spi_probe])
        .expect("refresh");

    let err = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Spi(SpiRequest::Transfer {
                write: vec![0x9F],
            })),
        ))
        .expect_err("fake devnode should not bind as a real spi transport");

    assert!(matches!(
        err,
        crate::RuntimeError::Driver {
            device_id: failing_device_id,
            source,
        } if failing_device_id == device_id
            && matches!(
                source.as_ref(),
                DriverError::Transport {
                    source: BusError::TransportFailure {
                        operation: "open",
                        ..
                    },
                    ..
                }
            )
    ));
    assert!(!runtime.is_bound(&device_id));
    assert!(runtime.inventory().contains(&device_id));
}

#[test]
fn runtime_surfaces_linux_i2c_transport_failures_during_bind() {
    let root = TestRoot::new();
    root.create_dir("sys/class/i2c-dev/i2c-1");
    root.write("sys/class/i2c-dev/i2c-1/name", "DesignWare I2C adapter\n");
    root.touch("dev/i2c-1");
    root.create_dir("sys/bus/i2c/devices/1-0050");
    root.write("sys/bus/i2c/devices/1-0050/name", "fake-sensor\n");
    root.write("sys/bus/i2c/devices/1-0050/modalias", "i2c:fake-sensor\n");

    let backend = LinuxBackend::with_paths(root.paths());
    let i2c_probe = backend.i2c_probe();
    let device_id = i2c_probe
        .discover(&DiscoveryContext::new())
        .expect("discover i2c devices")
        .devices
        .into_iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::I2cDevice)
        .expect("i2c device")
        .id;

    let mut runtime = Runtime::new();
    runtime.set_i2c_backend(backend.clone());
    runtime.register_driver(I2cDriver).expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&i2c_probe])
        .expect("refresh");

    let err = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::I2c(I2cRequest::WriteRead {
                write: vec![0x10],
                read_length: 2,
            })),
        ))
        .expect_err("fake devnode should not bind as a real i2c transport");

    assert!(matches!(
        err,
        crate::RuntimeError::Driver {
            device_id: failing_device_id,
            source,
        } if failing_device_id == device_id
            && matches!(
                source.as_ref(),
                DriverError::Transport {
                    source: BusError::TransportFailure {
                        operation: "open",
                        ..
                    },
                    ..
                }
            )
    ));
    assert!(!runtime.is_bound(&device_id));
    assert!(runtime.inventory().contains(&device_id));
}

#[test]
fn runtime_surfaces_linux_uart_transport_failures_during_bind() {
    let root = TestRoot::new();
    root.create_dir("sys/class/tty/ttyUSB0/device");
    root.write("sys/class/tty/ttyUSB0/dev", "188:0\n");
    root.write("sys/class/tty/ttyUSB0/device/modalias", "usb:v1D50p6018\n");
    root.touch("dev/ttyUSB0");

    let backend = LinuxBackend::with_paths(root.paths());
    let uart_probe = backend.uart_probe();
    let device_id = uart_probe
        .discover(&DiscoveryContext::new())
        .expect("discover uart devices")
        .devices
        .into_iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::UartPort)
        .expect("uart port")
        .id;

    let mut runtime = Runtime::new();
    runtime.set_uart_backend(backend.clone());
    runtime
        .register_driver(UartDriver)
        .expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&uart_probe])
        .expect("refresh");

    let err = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Uart(UartRequest::Read { max_bytes: 4 })),
        ))
        .expect_err("fake devnode should not bind as a real uart transport");

    assert!(matches!(
        err,
        crate::RuntimeError::Driver {
            device_id: failing_device_id,
            source,
        } if failing_device_id == device_id
            && matches!(
                source.as_ref(),
                DriverError::Transport {
                    source: BusError::TransportFailure {
                        operation: "open",
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
    assert!(failure.message.contains("failed to open Linux UART device"));
    assert!(!runtime.is_bound(&device_id));
    assert!(runtime.inventory().contains(&device_id));
}

#[test]
fn runtime_surfaces_linux_usb_transport_failures_during_bind() {
    let root = TestRoot::new();
    root.create_dir("sys/bus/usb/devices/usb99");
    root.write("sys/bus/usb/devices/usb99/product", "Test USB Bus\n");
    root.create_dir("sys/bus/usb/devices/99-9");
    root.write("sys/bus/usb/devices/99-9/idVendor", "1209\n");
    root.write("sys/bus/usb/devices/99-9/idProduct", "0001\n");
    root.write("sys/bus/usb/devices/99-9/devnum", "1\n");
    root.write("sys/bus/usb/devices/99-9/modalias", "usb:v1209p0001\n");
    root.create_dir("sys/bus/usb/devices/99-9:1.0");
    root.write("sys/bus/usb/devices/99-9:1.0/bInterfaceNumber", "00\n");
    root.write("sys/bus/usb/devices/99-9:1.0/bAlternateSetting", "00\n");
    root.write("sys/bus/usb/devices/99-9:1.0/bInterfaceClass", "ff\n");
    root.write("sys/bus/usb/devices/99-9:1.0/bInterfaceSubClass", "00\n");
    root.write("sys/bus/usb/devices/99-9:1.0/bInterfaceProtocol", "00\n");

    let backend = LinuxBackend::with_paths(root.paths());
    let usb_probe = backend.usb_probe();
    let device_id = usb_probe
        .discover(&DiscoveryContext::new())
        .expect("discover usb devices")
        .devices
        .into_iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::UsbInterface)
        .expect("usb interface")
        .id;

    let mut runtime = Runtime::new();
    runtime.set_usb_backend(backend.clone());
    runtime.register_driver(UsbDriver).expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&usb_probe])
        .expect("refresh");

    let err = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::Control(
                usb_vendor_request(),
            ))),
        ))
        .expect_err("fake sysfs device should not bind as a real usb transport");

    assert!(matches!(
        err,
        crate::RuntimeError::Driver {
            device_id: failing_device_id,
            source,
        } if failing_device_id == device_id
            && matches!(
                source.as_ref(),
                DriverError::Transport {
                    source: BusError::SessionUnavailable { .. },
                    ..
                }
            )
    ));
    assert!(!runtime.is_bound(&device_id));
    assert!(runtime.inventory().contains(&device_id));
}

#[test]
fn runtime_tracks_bind_failure_diagnostics() {
    let root = TestRoot::new();
    root.create_dir("sys/class/tty/ttyUSB1/device");
    root.write("sys/class/tty/ttyUSB1/dev", "188:1\n");
    root.write("sys/class/tty/ttyUSB1/device/modalias", "usb:v1D50p6018\n");
    root.touch("dev/ttyUSB1");

    let backend = LinuxBackend::with_paths(root.paths());
    let uart_probe = backend.uart_probe();
    let device_id = uart_probe
        .discover(&DiscoveryContext::new())
        .expect("discover uart devices")
        .devices
        .into_iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::UartPort)
        .expect("uart port")
        .id;

    let mut runtime = Runtime::new();
    runtime.set_uart_backend(backend);
    runtime
        .register_driver(UartDriver)
        .expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&uart_probe])
        .expect("refresh");

    let err = runtime.bind(&device_id).expect_err("bind should fail");
    assert!(matches!(err, crate::RuntimeError::Driver { .. }));

    let failure = runtime
        .failure(&device_id)
        .expect("failure should be tracked");
    assert_eq!(failure.operation, RuntimeFailureOperation::Bind);
    assert_eq!(failure.category, RuntimeFailureCategory::Driver);
    assert!(failure.driver_id.is_some());
}
