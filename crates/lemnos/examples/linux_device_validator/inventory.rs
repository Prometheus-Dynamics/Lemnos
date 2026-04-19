#![allow(clippy::print_stdout)]

use crate::support::board_validator::config::{GpioReadTarget, SpiTarget, UsbInterfaceTarget};
use lemnos::core::{DeviceAddress, DeviceDescriptor, DeviceId, DeviceKind, InterfaceKind, Value};
use std::path::Path;

pub fn print_inventory_summary(snapshot: &lemnos::discovery::InventorySnapshot) {
    println!("inventory summary:");
    for interface in [
        InterfaceKind::Gpio,
        InterfaceKind::Pwm,
        InterfaceKind::I2c,
        InterfaceKind::Spi,
        InterfaceKind::Uart,
        InterfaceKind::Usb,
    ] {
        println!("  {interface}: {}", snapshot.count_for(interface));
    }
    println!();

    print_descriptors(
        "GPIO chips",
        snapshot.by_kind(DeviceKind::GpioChip).as_slice(),
    );
    print_descriptors("I2C buses", snapshot.by_kind(DeviceKind::I2cBus).as_slice());
    print_descriptors(
        "I2C devices",
        snapshot.by_kind(DeviceKind::I2cDevice).as_slice(),
    );
    print_descriptors("SPI buses", snapshot.by_kind(DeviceKind::SpiBus).as_slice());
    print_descriptors(
        "SPI devices",
        snapshot.by_kind(DeviceKind::SpiDevice).as_slice(),
    );
    print_descriptors(
        "UART ports",
        snapshot.by_kind(DeviceKind::UartPort).as_slice(),
    );
    print_descriptors(
        "USB devices",
        snapshot.by_kind(DeviceKind::UsbDevice).as_slice(),
    );
    print_descriptors(
        "USB interfaces",
        snapshot.by_kind(DeviceKind::UsbInterface).as_slice(),
    );
    print_descriptors(
        "Configured logical I2C devices",
        snapshot
            .by_kind(DeviceKind::Unspecified(InterfaceKind::I2c))
            .as_slice(),
    );
    print_descriptors(
        "Linux LED class devices",
        snapshot
            .by_kind(DeviceKind::Unspecified(InterfaceKind::Gpio))
            .into_iter()
            .filter(|device| device.properties.get("linux.subsystem") == Some(&Value::from("leds")))
            .collect::<Vec<_>>()
            .as_slice(),
    );
    print_descriptors(
        "Linux hwmon fan devices",
        snapshot
            .by_kind(DeviceKind::Unspecified(InterfaceKind::Pwm))
            .into_iter()
            .filter(|device| {
                device.properties.get("linux.subsystem") == Some(&Value::from("hwmon"))
            })
            .collect::<Vec<_>>()
            .as_slice(),
    );
}

pub fn find_gpio_line(
    snapshot: &lemnos::discovery::InventorySnapshot,
    target: &GpioReadTarget,
) -> Option<DeviceId> {
    snapshot
        .by_kind(DeviceKind::GpioLine)
        .into_iter()
        .find(|device| match device.address.as_ref() {
            Some(DeviceAddress::GpioLine { chip_name, offset }) if *offset == target.offset => {
                if chip_name == &target.chip_name {
                    return true;
                }

                device
                    .properties
                    .get("devnode")
                    .and_then(|value| value.as_str())
                    .and_then(|devnode| Path::new(devnode).file_name())
                    .and_then(|name| name.to_str())
                    == Some(target.chip_name.as_str())
            }
            _ => false,
        })
        .map(|device| device.id.clone())
}

pub fn find_uart_port(
    snapshot: &lemnos::discovery::InventorySnapshot,
    port: &str,
) -> Option<DeviceId> {
    snapshot
        .by_kind(DeviceKind::UartPort)
        .into_iter()
        .find(|device| {
            matches!(
                device.address.as_ref(),
                Some(DeviceAddress::UartPort { port: device_port }) if device_port == port
            )
        })
        .map(|device| device.id.clone())
}

pub fn find_led(snapshot: &lemnos::discovery::InventorySnapshot, name: &str) -> Option<DeviceId> {
    snapshot
        .by_kind(DeviceKind::Unspecified(InterfaceKind::Gpio))
        .into_iter()
        .find(|device| {
            device.properties.get("linux.subsystem") == Some(&Value::from("leds"))
                && device.properties.get("led.name").and_then(Value::as_str) == Some(name)
        })
        .map(|device| device.id.clone())
}

pub fn find_hwmon_fan(
    snapshot: &lemnos::discovery::InventorySnapshot,
    hwmon_name: &str,
) -> Option<DeviceId> {
    snapshot
        .by_kind(DeviceKind::Unspecified(InterfaceKind::Pwm))
        .into_iter()
        .find(|device| {
            device.properties.get("linux.subsystem") == Some(&Value::from("hwmon"))
                && device
                    .properties
                    .get("fan.hwmon_name")
                    .and_then(Value::as_str)
                    == Some(hwmon_name)
        })
        .map(|device| device.id.clone())
}

pub fn find_spi_device(
    snapshot: &lemnos::discovery::InventorySnapshot,
    target: &SpiTarget,
) -> Option<DeviceId> {
    snapshot
        .by_kind(DeviceKind::SpiDevice)
        .into_iter()
        .find(|device| {
            matches!(
                device.address.as_ref(),
                Some(DeviceAddress::SpiDevice { bus, chip_select })
                    if *bus == target.bus && *chip_select == target.chip_select
            )
        })
        .map(|device| device.id.clone())
}

pub fn find_usb_interface(
    snapshot: &lemnos::discovery::InventorySnapshot,
    target: &UsbInterfaceTarget,
) -> Option<DeviceId> {
    snapshot
        .by_kind(DeviceKind::UsbInterface)
        .into_iter()
        .find(|device| {
            matches!(
                device.address.as_ref(),
                Some(DeviceAddress::UsbInterface {
                    bus,
                    ports,
                    interface_number,
                    ..
                }) if *bus == target.bus
                    && ports == &target.ports
                    && *interface_number == target.interface_number
            )
        })
        .map(|device| device.id.clone())
}

fn print_descriptors(title: &str, devices: &[&DeviceDescriptor]) {
    println!("{title}: {}", devices.len());
    for device in devices {
        println!(
            "  - id={} kind={} address={} name={}",
            device.id,
            device.kind,
            device
                .address
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "<none>".to_string()),
            device
                .display_name
                .as_deref()
                .or(device.summary.as_deref())
                .unwrap_or("<unnamed>")
        );
    }
    println!();
}
