use super::*;

#[test]
fn runtime_refreshes_inventory_and_dispatches_uart_requests_through_linux_backend() {
    let _pty_guard = PTY_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let root = TestRoot::new();
    let (mut master, slave) = TTYPort::pair().expect("create PTY pair");
    master
        .set_timeout(Duration::from_millis(100))
        .expect("set PTY master timeout");
    let slave_path = slave.name().expect("PTY slave path");
    let _slave = slave;

    root.create_dir("dev");
    root.create_dir("sys/class/tty/ttyUSB0");
    root.write("sys/class/tty/ttyUSB0/dev", "188:0\n");
    symlink(&slave_path, root.root.join("dev/ttyUSB0")).expect("symlink PTY slave into test /dev");

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

    let configure = UartConfiguration {
        baud_rate: 57_600,
        data_bits: UartDataBits::Eight,
        parity: UartParity::None,
        stop_bits: UartStopBits::One,
        flow_control: UartFlowControl::None,
    };
    let configure_response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Uart(UartRequest::Configure(
                configure.clone(),
            ))),
        ))
        .expect("configure uart");
    assert_eq!(
        configure_response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Uart(
            lemnos_core::UartResponse::Applied
        ))
    );

    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Uart(UartRequest::Write {
                bytes: b"ping".to_vec(),
            })),
        ))
        .expect("write uart");
    let mut master_buffer = [0_u8; 16];
    let bytes_read = master.read(&mut master_buffer).expect("read PTY master");
    assert_eq!(&master_buffer[..bytes_read], b"ping");

    master.write_all(b"pong").expect("write PTY master");
    let read_response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Uart(UartRequest::Read { max_bytes: 8 })),
        ))
        .expect("read uart");
    assert_eq!(
        read_response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Uart(
            lemnos_core::UartResponse::Bytes(b"pong".to_vec())
        ))
    );

    let config_response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Uart(UartRequest::GetConfiguration)),
        ))
        .expect("get uart configuration");
    assert_eq!(
        config_response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Uart(
            lemnos_core::UartResponse::Configuration(configure.clone())
        ))
    );

    assert!(runtime.is_bound(&device_id));
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("uart state")
            .telemetry
            .get("bytes_written"),
        Some(&4_u64.into())
    );
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("uart state")
            .telemetry
            .get("bytes_read"),
        Some(&4_u64.into())
    );
}

#[test]
fn runtime_rebinds_uart_transport_cleanly_after_unbind() {
    let _pty_guard = PTY_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let root = TestRoot::new();
    let (mut master, slave) = TTYPort::pair().expect("create PTY pair");
    master
        .set_timeout(Duration::from_millis(100))
        .expect("set PTY master timeout");
    let slave_path = slave.name().expect("PTY slave path");
    let _slave = slave;

    root.create_dir("dev");
    root.create_dir("sys/class/tty/ttyUSB0");
    root.write("sys/class/tty/ttyUSB0/dev", "188:0\n");
    symlink(&slave_path, root.root.join("dev/ttyUSB0")).expect("symlink PTY slave into test /dev");

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

    let configure = UartConfiguration {
        baud_rate: 57_600,
        data_bits: UartDataBits::Eight,
        parity: UartParity::None,
        stop_bits: UartStopBits::One,
        flow_control: UartFlowControl::None,
    };

    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Uart(UartRequest::Configure(
                configure.clone(),
            ))),
        ))
        .expect("configure uart");
    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Uart(UartRequest::Write {
                bytes: b"first".to_vec(),
            })),
        ))
        .expect("write first uart payload");

    let mut master_buffer = [0_u8; 16];
    let bytes_read = master
        .read(&mut master_buffer)
        .expect("read first PTY payload");
    assert_eq!(&master_buffer[..bytes_read], b"first");
    assert!(runtime.unbind(&device_id));
    assert!(!runtime.is_bound(&device_id));

    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Uart(UartRequest::Configure(configure))),
        ))
        .expect("reconfigure uart after unbind");
    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Uart(UartRequest::Write {
                bytes: b"second".to_vec(),
            })),
        ))
        .expect("write second uart payload");

    let bytes_read = master
        .read(&mut master_buffer)
        .expect("read second PTY payload");
    assert_eq!(&master_buffer[..bytes_read], b"second");
    assert!(runtime.is_bound(&device_id));
}
