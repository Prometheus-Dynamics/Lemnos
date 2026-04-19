use super::support::TestRoot;
use crate::{LinuxBackend, LinuxTransportConfig};
use lemnos_bus::{
    BusBackend, I2cBusBackend, PwmBusBackend, SessionAccess, SpiBusBackend, UartBusBackend,
    UsbBusBackend,
    contract::{assert_close_contract, assert_pwm_configuration_contract, assert_session_contract},
};
#[cfg(feature = "gpio-sysfs")]
use lemnos_bus::{GpioBusBackend, contract::assert_gpio_round_trip_contract};
#[cfg(feature = "gpio-sysfs")]
use lemnos_core::{
    DeviceAddress, DeviceDescriptor, GpioDirection, GpioLevel, InterfaceKind as CoreInterfaceKind,
};
use lemnos_core::{DeviceKind, InterfaceKind, PwmConfiguration, PwmPolarity};
use lemnos_discovery::{DiscoveryContext, DiscoveryProbe};
use std::fs;

#[test]
fn linux_backend_exposes_transport_config_overrides() {
    let transport_config = LinuxTransportConfig::new()
        .with_uart_default_baud_rate(57_600)
        .with_uart_timeout_ms(125)
        .with_usb_timeout_ms(250)
        .with_sysfs_export_retries(12)
        .with_sysfs_export_delay_ms(15);
    let backend = LinuxBackend::with_config(transport_config);

    assert_eq!(backend.transport_config(), &transport_config);
}

#[cfg(feature = "gpio-sysfs")]
#[test]
fn linux_backend_gpio_session_round_trips_sysfs_state() {
    let root = TestRoot::new();
    root.create_dir("sys/class/gpio/gpiochip0");
    root.write("sys/class/gpio/gpiochip0/base", "32\n");
    root.write("sys/class/gpio/gpiochip0/ngpio", "1\n");
    root.touch("dev/gpiochip0");
    root.create_dir("sys/class/gpio/gpio32");
    root.write("sys/class/gpio/gpio32/direction", "out\n");
    root.write("sys/class/gpio/gpio32/value", "0\n");
    root.write("sys/class/gpio/gpio32/active_low", "0\n");
    root.write("sys/class/gpio/gpio32/edge", "none\n");

    let backend = LinuxBackend::with_paths(root.paths());
    let device = backend
        .gpio_probe()
        .discover(&DiscoveryContext::new())
        .expect("discover gpio")
        .devices
        .into_iter()
        .find(|device| device.kind == DeviceKind::GpioLine)
        .expect("gpio line");

    assert!(backend.supports_device(&device));

    let mut session = backend
        .open_gpio(&device, SessionAccess::Exclusive)
        .expect("open gpio");
    assert_session_contract(
        session.as_ref(),
        InterfaceKind::Gpio,
        &device,
        backend.name(),
        SessionAccess::Exclusive,
    );
    assert_gpio_round_trip_contract(
        session.as_mut(),
        GpioLevel::Low,
        GpioLevel::High,
        GpioDirection::Output,
    );
    assert_eq!(
        fs::read_to_string(root.root.join("sys/class/gpio/gpio32/value")).expect("value file"),
        "1"
    );
    assert_close_contract(session.as_mut());
    assert!(matches!(
        session.read_level(),
        Err(lemnos_bus::BusError::SessionUnavailable { .. })
    ));

    let reopened = backend
        .open_gpio(&device, SessionAccess::Exclusive)
        .expect("reopen gpio");
    assert_eq!(reopened.metadata().state, lemnos_bus::SessionState::Idle);
}

#[cfg(feature = "gpio-sysfs")]
#[test]
fn linux_backend_gpio_open_supports_typed_address_without_property_fallbacks() {
    let root = TestRoot::new();
    root.create_dir("sys/class/gpio/gpiochip0");
    root.write("sys/class/gpio/gpiochip0/base", "32\n");
    root.write("sys/class/gpio/gpiochip0/ngpio", "1\n");
    root.create_dir("sys/class/gpio/gpio32");
    root.write("sys/class/gpio/gpio32/direction", "out\n");
    root.write("sys/class/gpio/gpio32/value", "0\n");
    root.write("sys/class/gpio/gpio32/active_low", "0\n");
    root.write("sys/class/gpio/gpio32/edge", "none\n");

    let backend = LinuxBackend::with_paths(root.paths());
    let device =
        DeviceDescriptor::builder_for_kind("linux.gpio.line.gpiochip0.0", DeviceKind::GpioLine)
            .expect("builder")
            .address(DeviceAddress::GpioLine {
                chip_name: "gpiochip0".into(),
                offset: 0,
            })
            .build()
            .expect("descriptor");

    assert!(backend.supports_device(&device));

    let mut session = backend
        .open_gpio(&device, SessionAccess::Exclusive)
        .expect("open gpio with typed address");
    assert_eq!(session.read_level().expect("read level"), GpioLevel::Low);
}

#[cfg(feature = "gpio-sysfs")]
#[test]
fn linux_backend_gpio_open_rejects_property_only_identity() {
    let root = TestRoot::new();
    root.create_dir("sys/class/gpio/gpiochip0");
    root.write("sys/class/gpio/gpiochip0/base", "32\n");
    root.write("sys/class/gpio/gpiochip0/ngpio", "1\n");
    root.create_dir("sys/class/gpio/gpio32");
    root.write("sys/class/gpio/gpio32/direction", "out\n");
    root.write("sys/class/gpio/gpio32/value", "0\n");
    root.write("sys/class/gpio/gpio32/active_low", "0\n");
    root.write("sys/class/gpio/gpio32/edge", "none\n");

    let device = DeviceDescriptor::builder("linux.gpio.line.gpiochip0.0", CoreInterfaceKind::Gpio)
        .expect("builder")
        .kind(DeviceKind::GpioLine)
        .property("chip_name", "gpiochip0")
        .property("offset", 0_u64)
        .property("global_line", 32_u64)
        .build()
        .expect("descriptor");

    let backend = LinuxBackend::with_paths(root.paths());
    assert!(!backend.supports_device(&device));
    assert!(matches!(
        backend.open_gpio(&device, SessionAccess::Exclusive),
        Err(lemnos_bus::BusError::UnsupportedDevice { .. })
    ));
}

#[test]
fn linux_backend_pwm_session_round_trips_sysfs_state() {
    let root = TestRoot::new();
    root.create_dir("sys/class/pwm/pwmchip0/pwm0");
    root.write("sys/class/pwm/pwmchip0/npwm", "1\n");
    root.write("sys/class/pwm/pwmchip0/pwm0/period", "20000000\n");
    root.write("sys/class/pwm/pwmchip0/pwm0/duty_cycle", "5000000\n");
    root.write("sys/class/pwm/pwmchip0/pwm0/enable", "0\n");
    root.write("sys/class/pwm/pwmchip0/pwm0/polarity", "normal\n");

    let backend = LinuxBackend::with_paths(root.paths());
    let device = backend
        .pwm_probe()
        .discover(&DiscoveryContext::new())
        .expect("discover pwm")
        .devices
        .into_iter()
        .find(|device| device.kind == DeviceKind::PwmChannel)
        .expect("pwm channel");

    assert!(backend.supports_device(&device));

    let mut session = backend
        .open_pwm(&device, SessionAccess::Exclusive)
        .expect("open pwm");
    let initial = PwmConfiguration {
        period_ns: 20_000_000,
        duty_cycle_ns: 5_000_000,
        enabled: false,
        polarity: PwmPolarity::Normal,
    };
    let updated = PwmConfiguration {
        period_ns: 25_000_000,
        duty_cycle_ns: 10_000_000,
        enabled: true,
        polarity: PwmPolarity::Inversed,
    };

    assert_session_contract(
        session.as_ref(),
        InterfaceKind::Pwm,
        &device,
        backend.name(),
        SessionAccess::Exclusive,
    );
    assert_pwm_configuration_contract(session.as_mut(), &initial, &updated);
    assert_eq!(
        fs::read_to_string(root.root.join("sys/class/pwm/pwmchip0/pwm0/period"))
            .expect("period file"),
        "25000000"
    );
    assert_eq!(
        fs::read_to_string(root.root.join("sys/class/pwm/pwmchip0/pwm0/duty_cycle"))
            .expect("duty_cycle file"),
        "10000000"
    );
    assert_eq!(
        fs::read_to_string(root.root.join("sys/class/pwm/pwmchip0/pwm0/enable"))
            .expect("enable file"),
        "1"
    );
    assert_eq!(
        fs::read_to_string(root.root.join("sys/class/pwm/pwmchip0/pwm0/polarity"))
            .expect("polarity file"),
        "inversed"
    );
    assert_close_contract(session.as_mut());
    assert!(matches!(
        session.configuration(),
        Err(lemnos_bus::BusError::SessionUnavailable { .. })
    ));

    let reopened = backend
        .open_pwm(&device, SessionAccess::Exclusive)
        .expect("reopen pwm");
    assert_eq!(reopened.metadata().state, lemnos_bus::SessionState::Idle);
}

#[test]
fn linux_backend_spi_open_reports_transport_failure_for_non_device_node() {
    let root = TestRoot::new();
    root.create_dir("sys/bus/spi/devices/spi0.1");
    root.write("sys/bus/spi/devices/spi0.1/modalias", "spi:fake-display\n");
    root.touch("dev/spidev0.1");

    let backend = LinuxBackend::with_paths(root.paths());
    let device = backend
        .spi_probe()
        .discover(&DiscoveryContext::new())
        .expect("discover spi")
        .devices
        .into_iter()
        .find(|device| device.kind == DeviceKind::SpiDevice)
        .expect("spi device");

    assert!(backend.supports_device(&device));

    let err = match backend.open_spi(&device, SessionAccess::Exclusive) {
        Ok(_) => panic!("fake file should not open as spi device"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        lemnos_bus::BusError::TransportFailure {
            operation: "open",
            ..
        }
    ));
}

#[test]
fn linux_backend_spi_open_reports_session_unavailable_for_missing_devnode() {
    let root = TestRoot::new();
    root.create_dir("sys/bus/spi/devices/spi0.1");
    root.write(
        "sys/bus/spi/devices/spi0.1/modalias",
        "spi:missing-device\n",
    );

    let backend = LinuxBackend::with_paths(root.paths());
    let device = backend
        .spi_probe()
        .discover(&DiscoveryContext::new())
        .expect("discover spi")
        .devices
        .into_iter()
        .find(|device| device.kind == DeviceKind::SpiDevice)
        .expect("spi device");

    let err = match backend.open_spi(&device, SessionAccess::Exclusive) {
        Ok(_) => panic!("missing devnode should be unavailable"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        lemnos_bus::BusError::SessionUnavailable { .. }
    ));
}

#[test]
fn linux_backend_uart_open_reports_transport_failure_for_non_device_node() {
    let root = TestRoot::new();
    root.create_dir("sys/class/tty/ttyUSB0/device");
    root.write("sys/class/tty/ttyUSB0/dev", "188:0\n");
    root.write("sys/class/tty/ttyUSB0/device/modalias", "usb:v067Bp2303\n");
    root.touch("dev/ttyUSB0");

    let backend = LinuxBackend::with_paths(root.paths());
    let device = backend
        .uart_probe()
        .discover(&DiscoveryContext::new())
        .expect("discover uart")
        .devices
        .into_iter()
        .find(|device| device.kind == DeviceKind::UartPort)
        .expect("uart port");

    assert!(backend.supports_device(&device));

    let err = match backend.open_uart(&device, SessionAccess::Exclusive) {
        Ok(_) => panic!("fake file should not open as uart device"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        lemnos_bus::BusError::TransportFailure {
            operation: "open",
            ..
        }
    ));
}

#[test]
fn linux_backend_uart_open_reports_session_unavailable_for_missing_devnode() {
    let root = TestRoot::new();
    root.create_dir("sys/class/tty/ttyUSB0/device");
    root.write("sys/class/tty/ttyUSB0/dev", "188:0\n");
    root.write("sys/class/tty/ttyUSB0/device/modalias", "usb:v067Bp2303\n");

    let backend = LinuxBackend::with_paths(root.paths());
    let device = backend
        .uart_probe()
        .discover(&DiscoveryContext::new())
        .expect("discover uart")
        .devices
        .into_iter()
        .find(|device| device.kind == DeviceKind::UartPort)
        .expect("uart port");

    let err = match backend.open_uart(&device, SessionAccess::Exclusive) {
        Ok(_) => panic!("missing devnode should be unavailable"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        lemnos_bus::BusError::SessionUnavailable { .. }
    ));
}

#[test]
fn linux_backend_usb_open_reports_session_unavailable_for_missing_device() {
    let root = TestRoot::new();
    root.create_dir("sys/bus/usb/devices/usb99");
    root.write("sys/bus/usb/devices/usb99/product", "Test Bus\n");
    root.create_dir("sys/bus/usb/devices/99-9");
    root.write("sys/bus/usb/devices/99-9/idVendor", "1209\n");
    root.write("sys/bus/usb/devices/99-9/idProduct", "0001\n");
    root.write("sys/bus/usb/devices/99-9/devnum", "1\n");
    root.create_dir("sys/bus/usb/devices/99-9:1.0");
    root.write("sys/bus/usb/devices/99-9:1.0/bInterfaceNumber", "00\n");
    root.write("sys/bus/usb/devices/99-9:1.0/bAlternateSetting", "00\n");
    root.write("sys/bus/usb/devices/99-9:1.0/bInterfaceClass", "ff\n");
    root.write("sys/bus/usb/devices/99-9:1.0/bInterfaceSubClass", "00\n");
    root.write("sys/bus/usb/devices/99-9:1.0/bInterfaceProtocol", "00\n");

    let backend = LinuxBackend::with_paths(root.paths());
    let device = backend
        .usb_probe()
        .discover(&DiscoveryContext::new())
        .expect("discover usb")
        .devices
        .into_iter()
        .find(|device| device.kind == DeviceKind::UsbInterface)
        .expect("usb interface");

    assert!(backend.supports_device(&device));

    let err = match backend.open_usb(&device, SessionAccess::ExclusiveController) {
        Ok(_) => panic!("fake sysfs entry should not open as usb device"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        lemnos_bus::BusError::SessionUnavailable { .. }
            | lemnos_bus::BusError::TransportFailure {
                operation: "open",
                ..
            }
    ));
}

#[test]
fn linux_backend_i2c_open_reports_transport_failure_for_non_device_node() {
    let root = TestRoot::new();
    root.create_dir("sys/class/i2c-dev/i2c-1");
    root.touch("dev/i2c-1");
    root.create_dir("sys/bus/i2c/devices/1-0050");

    let backend = LinuxBackend::with_paths(root.paths());
    let device = backend
        .i2c_probe()
        .discover(&DiscoveryContext::new())
        .expect("discover i2c")
        .devices
        .into_iter()
        .find(|device| device.kind == DeviceKind::I2cDevice)
        .expect("i2c device");

    assert!(backend.supports_device(&device));

    let err = match backend.open_i2c(&device, SessionAccess::Exclusive) {
        Ok(_) => panic!("fake file should not open as i2c device"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        lemnos_bus::BusError::TransportFailure {
            operation: "open",
            ..
        }
    ));
}
