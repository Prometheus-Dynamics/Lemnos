use super::support::TestRoot;
use crate::{
    GpioDiscoveryProbe, HwmonDiscoveryProbe, I2cDiscoveryProbe, LedDiscoveryProbe, LinuxBackend,
    PwmDiscoveryProbe, SpiDiscoveryProbe, UartDiscoveryProbe, UsbDiscoveryProbe,
    metadata::descriptor_devnode,
};
use lemnos_core::{
    DeviceAddress, DeviceControlSurface, DeviceKind, DeviceRelation, InterfaceKind, Value,
};
use lemnos_discovery::{DiscoveryContext, DiscoveryProbe};

#[test]
fn hwmon_probe_discovers_linux_pwm_fan_device() {
    let root = TestRoot::new();
    root.create_dir("sys/class/hwmon/hwmon3");
    root.write("sys/class/hwmon/hwmon3/name", "pwmfan\n");
    root.write("sys/class/hwmon/hwmon3/pwm1", "120\n");
    root.write("sys/class/hwmon/hwmon3/pwm1_enable", "1\n");
    root.write("sys/class/hwmon/hwmon3/fan1_input", "4321\n");
    root.create_dir("sys/devices/platform/pwmfan");
    root.create_dir("sys/bus/platform/drivers/pwm-fan");
    std::os::unix::fs::symlink(
        root.root.join("sys/devices/platform/pwmfan"),
        root.root.join("sys/class/hwmon/hwmon3/device"),
    )
    .expect("device symlink");
    std::os::unix::fs::symlink(
        root.root.join("sys/bus/platform/drivers/pwm-fan"),
        root.root.join("sys/devices/platform/pwmfan/driver"),
    )
    .expect("driver symlink");

    let probe = HwmonDiscoveryProbe::new(root.paths());
    let discovery = probe
        .discover(&DiscoveryContext::new())
        .expect("discover hwmon devices");

    assert_eq!(discovery.devices.len(), 1);
    let device = discovery.devices.first().expect("hwmon fan descriptor");
    assert_eq!(device.kind, DeviceKind::Unspecified(InterfaceKind::Pwm));
    assert_eq!(
        device.address,
        Some(DeviceAddress::Custom {
            interface: InterfaceKind::Pwm,
            scheme: "linux-hwmon-fan".into(),
            value: "hwmon3".into(),
        })
    );
    assert_eq!(
        device.properties.get("linux.driver"),
        Some(&Value::from("pwm-fan"))
    );
    assert_eq!(
        device.properties.get("fan.pwm"),
        Some(&Value::from(120_u64))
    );
    assert_eq!(
        device.properties.get("fan.rpm"),
        Some(&Value::from(4321_u64))
    );
    assert_eq!(
        device.control_surface,
        Some(DeviceControlSurface::LinuxClass {
            root: root
                .root
                .join("sys/class/hwmon/hwmon3")
                .display()
                .to_string(),
        })
    );
}

#[test]
fn led_probe_discovers_linux_led_class_device() {
    let root = TestRoot::new();
    root.create_dir("sys/class/leds/ACT");
    root.write("sys/class/leds/ACT/brightness", "1\n");
    root.write("sys/class/leds/ACT/max_brightness", "255\n");
    root.write(
        "sys/class/leds/ACT/trigger",
        "none timer [default-on] heartbeat\n",
    );
    root.create_dir("sys/devices/platform");
    root.create_dir("sys/devices/platform/leds");
    root.create_dir("sys/bus/platform/drivers/leds-gpio");
    std::os::unix::fs::symlink(
        root.root.join("sys/devices/platform/leds"),
        root.root.join("sys/class/leds/ACT/device"),
    )
    .expect("device symlink");
    std::os::unix::fs::symlink(
        root.root.join("sys/bus/platform/drivers/leds-gpio"),
        root.root.join("sys/devices/platform/leds/driver"),
    )
    .expect("driver symlink");

    let probe = LedDiscoveryProbe::new(root.paths());
    let discovery = probe
        .discover(&DiscoveryContext::new())
        .expect("discover led devices");

    assert_eq!(discovery.devices.len(), 1);
    assert!(discovery.notes.is_empty());

    let device = discovery.devices.first().expect("led descriptor");
    assert_eq!(device.kind, DeviceKind::Unspecified(InterfaceKind::Gpio));
    assert_eq!(
        device.address,
        Some(DeviceAddress::Custom {
            interface: InterfaceKind::Gpio,
            scheme: "linux-led-class".into(),
            value: "ACT".into(),
        })
    );
    assert_eq!(
        device.properties.get("linux.subsystem"),
        Some(&Value::from("leds"))
    );
    assert_eq!(
        device.properties.get("linux.driver"),
        Some(&Value::from("leds-gpio"))
    );
    assert_eq!(
        device.properties.get("led.active_trigger"),
        Some(&Value::from("default-on"))
    );
    assert_eq!(
        device.control_surface,
        Some(DeviceControlSurface::LinuxClass {
            root: root.root.join("sys/class/leds/ACT").display().to_string(),
        })
    );
    assert_eq!(device.capabilities.len(), 4);
}

#[test]
fn gpio_probe_discovers_linux_chip_and_lines() {
    let root = TestRoot::new();
    root.create_dir("sys/class/gpio/gpiochip0");
    root.write("sys/class/gpio/gpiochip0/label", "soc-gpio\n");
    root.write("sys/class/gpio/gpiochip0/base", "32\n");
    root.write("sys/class/gpio/gpiochip0/ngpio", "2\n");
    root.touch("dev/gpiochip0");

    let probe = GpioDiscoveryProbe::new(root.paths());
    let discovery = probe
        .discover(&DiscoveryContext::new())
        .expect("discover gpio devices");

    assert_eq!(discovery.devices.len(), 3);
    assert!(discovery.notes.is_empty());

    let chip = discovery
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::GpioChip)
        .expect("gpio chip descriptor");
    assert_eq!(
        chip.address,
        Some(DeviceAddress::GpioChip {
            chip_name: "gpiochip0".into(),
            base_line: Some(32),
        })
    );
    assert_eq!(chip.properties.get("line_count"), Some(&Value::from(2_u64)));
    let chip_devnode = root.paths().gpio_devnode("gpiochip0").display().to_string();
    assert_eq!(descriptor_devnode(chip), Some(chip_devnode.as_str()));

    let line = discovery
        .devices
        .iter()
        .find(|device| {
            device.kind == DeviceKind::GpioLine
                && device.address
                    == Some(DeviceAddress::GpioLine {
                        chip_name: "gpiochip0".into(),
                        offset: 1,
                    })
        })
        .expect("gpio line descriptor");
    assert_eq!(
        line.match_hints.driver_hint.as_deref(),
        Some("lemnos.gpio.generic")
    );
    assert_eq!(line.capabilities.len(), 4);
    assert!(
        line.links
            .iter()
            .any(|link| link.target == chip.id && link.relation == DeviceRelation::Parent)
    );
    assert_eq!(
        line.properties.get("global_line"),
        Some(&Value::from(33_u64))
    );
}

#[test]
fn i2c_probe_discovers_linux_bus_and_device() {
    let root = TestRoot::new();
    root.create_dir("sys/class/i2c-dev/i2c-10");
    root.write("sys/class/i2c-dev/i2c-10/name", "DesignWare I2C adapter\n");
    root.touch("dev/i2c-10");
    root.create_dir("sys/bus/i2c/devices/10-0060");
    root.write("sys/bus/i2c/devices/10-0060/name", "ina219\n");
    root.write("sys/bus/i2c/devices/10-0060/modalias", "i2c:ina219\n");

    let probe = I2cDiscoveryProbe::new(root.paths());
    let discovery = probe
        .discover(&DiscoveryContext::new())
        .expect("discover i2c devices");

    assert_eq!(discovery.devices.len(), 2);
    assert!(discovery.notes.is_empty());

    let bus = discovery
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::I2cBus)
        .expect("i2c bus descriptor");
    assert_eq!(bus.address, Some(DeviceAddress::I2cBus { bus: 10 }));
    assert_eq!(
        bus.properties.get("adapter_name"),
        Some(&Value::from("DesignWare I2C adapter"))
    );

    let device = discovery
        .devices
        .iter()
        .find(|entry| entry.kind == DeviceKind::I2cDevice)
        .expect("i2c device descriptor");
    assert_eq!(
        device.address,
        Some(DeviceAddress::I2cDevice {
            bus: 10,
            address: 0x60,
        })
    );
    assert_eq!(
        device.match_hints.driver_hint.as_deref(),
        Some("lemnos.i2c.generic")
    );
    assert_eq!(device.match_hints.modalias.as_deref(), Some("i2c:ina219"));
    assert_eq!(device.capabilities.len(), 4);
    assert!(
        device
            .links
            .iter()
            .any(|link| link.target == bus.id && link.relation == DeviceRelation::Parent)
    );
    let i2c_devnode = root.paths().i2c_devnode(10).display().to_string();
    assert_eq!(descriptor_devnode(device), Some(i2c_devnode.as_str()));
}

#[test]
fn pwm_probe_discovers_linux_chip_and_channels() {
    let root = TestRoot::new();
    root.create_dir("sys/class/pwm/pwmchip2");
    root.write("sys/class/pwm/pwmchip2/npwm", "2\n");
    root.create_dir("sys/class/pwm/pwmchip2/pwm1");

    let probe = PwmDiscoveryProbe::new(root.paths());
    let discovery = probe
        .discover(&DiscoveryContext::new())
        .expect("discover pwm devices");

    assert_eq!(discovery.devices.len(), 3);
    assert!(discovery.notes.is_empty());

    let chip = discovery
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::PwmChip)
        .expect("pwm chip descriptor");
    assert_eq!(
        chip.address,
        Some(DeviceAddress::PwmChip {
            chip_name: "pwmchip2".into(),
        })
    );
    assert_eq!(
        chip.properties.get("channel_count"),
        Some(&Value::from(2_u64))
    );

    let channel = discovery
        .devices
        .iter()
        .find(|device| {
            device.kind == DeviceKind::PwmChannel
                && device.address
                    == Some(DeviceAddress::PwmChannel {
                        chip_name: "pwmchip2".into(),
                        channel: 1,
                    })
        })
        .expect("pwm channel descriptor");
    assert_eq!(
        channel.match_hints.driver_hint.as_deref(),
        Some("lemnos.pwm.generic")
    );
    assert_eq!(channel.capabilities.len(), 5);
    assert!(
        channel
            .links
            .iter()
            .any(|link| link.target == chip.id && link.relation == DeviceRelation::Parent)
    );
    assert_eq!(channel.properties.get("exported"), Some(&Value::from(true)));
}

#[test]
fn spi_probe_discovers_linux_bus_and_device() {
    let root = TestRoot::new();
    root.create_dir("sys/bus/spi/devices/spi0.1");
    root.write("sys/bus/spi/devices/spi0.1/modalias", "spi:st7735r\n");
    root.touch("dev/spidev0.1");

    let probe = SpiDiscoveryProbe::new(root.paths());
    let discovery = probe
        .discover(&DiscoveryContext::new())
        .expect("discover spi devices");

    assert_eq!(discovery.devices.len(), 2);
    assert!(discovery.notes.is_empty());

    let bus = discovery
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::SpiBus)
        .expect("spi bus descriptor");
    assert_eq!(bus.address, Some(DeviceAddress::SpiBus { bus: 0 }));

    let device = discovery
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::SpiDevice)
        .expect("spi device descriptor");
    assert_eq!(
        device.address,
        Some(DeviceAddress::SpiDevice {
            bus: 0,
            chip_select: 1,
        })
    );
    assert_eq!(
        device.match_hints.driver_hint.as_deref(),
        Some("lemnos.spi.generic")
    );
    assert_eq!(device.match_hints.modalias.as_deref(), Some("spi:st7735r"));
    assert_eq!(device.capabilities.len(), 4);
    assert!(
        device
            .links
            .iter()
            .any(|link| link.target == bus.id && link.relation == DeviceRelation::Parent)
    );
    let spi_devnode = root.paths().spi_devnode(0, 1).display().to_string();
    assert_eq!(descriptor_devnode(device), Some(spi_devnode.as_str()));
}

#[test]
fn uart_probe_discovers_linux_port() {
    let root = TestRoot::new();
    root.create_dir("sys/class/tty/ttyUSB0/device");
    root.write("sys/class/tty/ttyUSB0/dev", "188:0\n");
    root.write("sys/class/tty/ttyUSB0/device/modalias", "usb:v067Bp2303\n");
    root.touch("dev/ttyUSB0");

    let probe = UartDiscoveryProbe::new(root.paths());
    let discovery = probe
        .discover(&DiscoveryContext::new())
        .expect("discover uart devices");

    assert_eq!(discovery.devices.len(), 1);
    assert!(discovery.notes.is_empty());

    let device = discovery.devices.first().expect("uart port descriptor");
    assert_eq!(device.kind, DeviceKind::UartPort);
    assert_eq!(
        device.address,
        Some(DeviceAddress::UartPort {
            port: "ttyUSB0".into(),
        })
    );
    assert_eq!(
        device.match_hints.driver_hint.as_deref(),
        Some("lemnos.uart.generic")
    );
    assert_eq!(
        device.match_hints.modalias.as_deref(),
        Some("usb:v067Bp2303")
    );
    assert_eq!(device.capabilities.len(), 5);
    let uart_devnode = root.paths().tty_devnode("ttyUSB0").display().to_string();
    assert_eq!(descriptor_devnode(device), Some(uart_devnode.as_str()));
}

#[test]
fn usb_probe_discovers_linux_bus_device_and_interface() {
    let root = TestRoot::new();
    root.create_dir("sys/bus/usb/devices/usb1");
    root.write("sys/bus/usb/devices/usb1/product", "Root Hub\n");
    root.create_dir("sys/bus/usb/devices/1-2");
    root.write("sys/bus/usb/devices/1-2/idVendor", "1209\n");
    root.write("sys/bus/usb/devices/1-2/idProduct", "0001\n");
    root.write("sys/bus/usb/devices/1-2/devnum", "5\n");
    root.write("sys/bus/usb/devices/1-2/modalias", "usb:v1209p0001\n");
    root.write("sys/bus/usb/devices/1-2/manufacturer", "Test Vendor\n");
    root.write("sys/bus/usb/devices/1-2/product", "Test Gadget\n");
    root.touch("dev/bus/usb/001/005");
    root.create_dir("sys/bus/usb/devices/1-2:1.0");
    root.write("sys/bus/usb/devices/1-2:1.0/bInterfaceNumber", "00\n");
    root.write("sys/bus/usb/devices/1-2:1.0/bAlternateSetting", "00\n");
    root.write("sys/bus/usb/devices/1-2:1.0/bInterfaceClass", "ff\n");
    root.write("sys/bus/usb/devices/1-2:1.0/bInterfaceSubClass", "00\n");
    root.write("sys/bus/usb/devices/1-2:1.0/bInterfaceProtocol", "00\n");

    let probe = UsbDiscoveryProbe::new(root.paths());
    let discovery = probe
        .discover(&DiscoveryContext::new())
        .expect("discover usb devices");

    assert_eq!(discovery.devices.len(), 3);
    assert!(discovery.notes.is_empty());

    let bus = discovery
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::UsbBus)
        .expect("usb bus descriptor");
    assert_eq!(bus.address, Some(DeviceAddress::UsbBus { bus: 1 }));

    let device = discovery
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::UsbDevice)
        .expect("usb device descriptor");
    assert_eq!(
        device.address,
        Some(DeviceAddress::UsbDevice {
            bus: 1,
            ports: vec![2],
            vendor_id: Some(0x1209),
            product_id: Some(0x0001),
        })
    );
    assert_eq!(
        device.match_hints.driver_hint.as_deref(),
        Some("lemnos.usb.generic")
    );
    let usb_devnode = root.paths().usb_bus_devnode(1, 5).display().to_string();
    assert_eq!(descriptor_devnode(device), Some(usb_devnode.as_str()));
    assert!(
        device
            .links
            .iter()
            .any(|link| link.target == bus.id && link.relation == DeviceRelation::Parent)
    );

    let interface = discovery
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::UsbInterface)
        .expect("usb interface descriptor");
    assert_eq!(
        interface.address,
        Some(DeviceAddress::UsbInterface {
            bus: 1,
            ports: vec![2],
            interface_number: 0,
            vendor_id: Some(0x1209),
            product_id: Some(0x0001),
        })
    );
    assert_eq!(interface.capabilities.len(), 7);
    assert!(
        interface
            .links
            .iter()
            .any(|link| link.target == device.id && link.relation == DeviceRelation::Parent)
    );
}

#[test]
fn linux_backend_discovers_requested_interfaces_only() {
    let root = TestRoot::new();
    root.create_dir("sys/class/gpio/gpiochip0");
    root.write("sys/class/gpio/gpiochip0/base", "0\n");
    root.write("sys/class/gpio/gpiochip0/ngpio", "1\n");
    root.touch("dev/gpiochip0");
    root.create_dir("sys/class/i2c-dev/i2c-1");
    root.touch("dev/i2c-1");
    root.create_dir("sys/bus/i2c/devices/1-0048");

    let backend = LinuxBackend::with_paths(root.paths());
    let context = DiscoveryContext::new().with_requested_interface(InterfaceKind::I2c);
    let report = backend
        .discover(&context)
        .expect("discover requested interface");

    assert_eq!(report.probe_reports.len(), 1);
    assert_eq!(report.snapshot.count_for(InterfaceKind::I2c), 2);
    assert_eq!(report.snapshot.count_for(InterfaceKind::Gpio), 0);
}
