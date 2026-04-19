use super::*;

#[test]
fn runtime_refreshes_inventory_and_dispatches_gpio_requests_through_linux_backend() {
    let root = TestRoot::new();
    root.create_dir("sys/class/gpio/gpiochip0");
    root.write("sys/class/gpio/gpiochip0/label", "soc-gpio\n");
    root.write("sys/class/gpio/gpiochip0/base", "32\n");
    root.write("sys/class/gpio/gpiochip0/ngpio", "1\n");
    root.touch("dev/gpiochip0");
    root.create_dir("sys/class/gpio/gpio32");
    root.write("sys/class/gpio/gpio32/direction", "out\n");
    root.write("sys/class/gpio/gpio32/value", "0\n");
    root.write("sys/class/gpio/gpio32/active_low", "0\n");
    root.write("sys/class/gpio/gpio32/edge", "none\n");

    let backend = LinuxBackend::with_paths(root.paths());
    let gpio_probe = backend.gpio_probe();
    let device_id = gpio_probe
        .discover(&DiscoveryContext::new())
        .expect("discover gpio devices")
        .devices
        .into_iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::GpioLine)
        .expect("gpio line device")
        .id;

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(backend.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");

    let report = runtime
        .refresh(&DiscoveryContext::new(), &[&gpio_probe])
        .expect("refresh");
    assert_eq!(report.discovery.snapshot.len(), 2);
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
        lemnos_core::InteractionResponse::Standard(StandardResponse::Gpio(
            lemnos_core::GpioResponse::Applied
        ))
    );
    assert!(runtime.is_bound(&device_id));
    assert_eq!(
        fs::read_to_string(root.root.join("sys/class/gpio/gpio32/value")).expect("value file"),
        "1"
    );
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
fn runtime_unbind_unexports_gpio_sysfs_line_when_session_exported_it() {
    let root = TestRoot::new();
    root.create_dir("sys/class/gpio/gpiochip0");
    root.write("sys/class/gpio/gpiochip0/label", "soc-gpio\n");
    root.write("sys/class/gpio/gpiochip0/base", "32\n");
    root.write("sys/class/gpio/gpiochip0/ngpio", "1\n");
    root.touch("sys/class/gpio/export");
    root.touch("sys/class/gpio/unexport");
    root.touch("dev/gpiochip0");

    let _harness = SysfsGpioExportHarness::new(&root, 32, "out", "0");
    let backend = LinuxBackend::with_paths_and_config(
        root.paths(),
        lemnos_linux::LinuxTransportConfig::new()
            .with_sysfs_export_retries(40)
            .with_sysfs_export_delay_ms(5),
    );
    let gpio_probe = backend.gpio_probe();
    let device_id = gpio_probe
        .discover(&DiscoveryContext::new())
        .expect("discover gpio devices")
        .devices
        .into_iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::GpioLine)
        .expect("gpio line device")
        .id;

    let line_root = root.root.join("sys/class/gpio/gpio32");

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(backend);
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&gpio_probe])
        .expect("refresh");

    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect("write request");
    wait_for_path_state(&line_root, true, "exported gpio line");
    assert!(runtime.is_bound(&device_id));

    assert!(runtime.unbind(&device_id));
    wait_for_path_state(&line_root, false, "unexported gpio line");
    assert!(!runtime.is_bound(&device_id));
}

#[test]
fn runtime_refreshes_inventory_and_dispatches_pwm_requests_through_linux_backend() {
    let root = TestRoot::new();
    root.create_dir("sys/class/pwm/pwmchip0/pwm0");
    root.write("sys/class/pwm/pwmchip0/npwm", "1\n");
    root.write("sys/class/pwm/pwmchip0/pwm0/period", "20000000\n");
    root.write("sys/class/pwm/pwmchip0/pwm0/duty_cycle", "5000000\n");
    root.write("sys/class/pwm/pwmchip0/pwm0/enable", "0\n");
    root.write("sys/class/pwm/pwmchip0/pwm0/polarity", "normal\n");

    let backend = LinuxBackend::with_paths(root.paths());
    let pwm_probe = backend.pwm_probe();
    let device_id = pwm_probe
        .discover(&DiscoveryContext::new())
        .expect("discover pwm devices")
        .devices
        .into_iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::PwmChannel)
        .expect("pwm channel device")
        .id;

    let mut runtime = Runtime::new();
    runtime.set_pwm_backend(backend.clone());
    runtime.register_driver(PwmDriver).expect("register driver");

    let report = runtime
        .refresh(&DiscoveryContext::new(), &[&pwm_probe])
        .expect("refresh");
    assert_eq!(report.discovery.snapshot.len(), 2);
    assert!(runtime.inventory().contains(&device_id));

    let response = runtime
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
        .expect("configure request");

    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Pwm(
            lemnos_core::PwmResponse::Applied
        ))
    );
    assert!(runtime.is_bound(&device_id));
    assert_eq!(
        fs::read_to_string(root.root.join("sys/class/pwm/pwmchip0/pwm0/enable"))
            .expect("enable file"),
        "1"
    );
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("state")
            .telemetry
            .get("duty_cycle_ratio"),
        Some(&0.5_f64.into())
    );
}

#[test]
fn runtime_shutdown_unexports_pwm_sysfs_channel_when_session_exported_it() {
    let root = TestRoot::new();
    root.create_dir("sys/class/pwm/pwmchip0");
    root.write("sys/class/pwm/pwmchip0/npwm", "1\n");
    root.touch("sys/class/pwm/pwmchip0/export");
    root.touch("sys/class/pwm/pwmchip0/unexport");

    let _harness = SysfsPwmExportHarness::new(&root, "pwmchip0", 0, 20_000_000, 5_000_000);
    let backend = LinuxBackend::with_paths_and_config(
        root.paths(),
        lemnos_linux::LinuxTransportConfig::new()
            .with_sysfs_export_retries(40)
            .with_sysfs_export_delay_ms(5),
    );
    let pwm_probe = backend.pwm_probe();
    let device_id = pwm_probe
        .discover(&DiscoveryContext::new())
        .expect("discover pwm devices")
        .devices
        .into_iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::PwmChannel)
        .expect("pwm channel device")
        .id;

    let channel_root = root.root.join("sys/class/pwm/pwmchip0/pwm0");

    let mut runtime = Runtime::new();
    runtime.set_pwm_backend(backend);
    runtime.register_driver(PwmDriver).expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&pwm_probe])
        .expect("refresh");

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
        .expect("configure request");
    wait_for_path_state(&channel_root, true, "exported pwm channel");
    assert!(runtime.is_bound(&device_id));

    runtime.shutdown();
    wait_for_path_state(&channel_root, false, "unexported pwm channel");
    assert!(!runtime.is_running());
}
