use super::*;
use crate::{
    CapabilityAccess, CapabilityDescriptor, CoreError, DeviceControlSurface, DeviceId,
    InterfaceKind, Value,
};

#[test]
fn device_kind_maps_to_expected_interface() {
    assert_eq!(DeviceKind::GpioLine.interface(), InterfaceKind::Gpio);
    assert_eq!(DeviceKind::UsbInterface.interface(), InterfaceKind::Usb);
}

#[test]
fn descriptor_builder_collects_core_metadata() {
    let capability =
        CapabilityDescriptor::new("gpio.write", CapabilityAccess::WRITE).expect("capability");
    let parent = DeviceId::new("gpiochip0").expect("device id");

    let descriptor = DeviceDescriptor::builder_for_kind("gpiochip0-line1", DeviceKind::GpioLine)
        .expect("builder")
        .local_id("line1")
        .expect("local id")
        .display_name("GPIO line 1")
        .summary("Digital output line")
        .address(DeviceAddress::GpioLine {
            chip_name: "gpiochip0".into(),
            offset: 1,
        })
        .driver_hint("generic-gpio")
        .label("chip_name", "gpiochip0")
        .property("offset", 1_u64)
        .control_surface(DeviceControlSurface::LinuxClass {
            root: "/sys/class/gpio/gpiochip0".into(),
        })
        .capability(capability)
        .link(DeviceLink::new(parent, DeviceRelation::Parent))
        .build()
        .expect("descriptor");

    assert_eq!(descriptor.interface, InterfaceKind::Gpio);
    assert_eq!(descriptor.kind, DeviceKind::GpioLine);
    assert_eq!(descriptor.capabilities.len(), 1);
    assert_eq!(
        descriptor.properties.get("offset"),
        Some(&Value::from(1_u64))
    );
    assert_eq!(
        descriptor.control_surface,
        Some(DeviceControlSurface::LinuxClass {
            root: "/sys/class/gpio/gpiochip0".into(),
        })
    );
}

#[test]
fn descriptor_validation_rejects_mismatched_address() {
    let descriptor = DeviceDescriptor::builder_for_kind("uart0", DeviceKind::UartPort)
        .expect("builder")
        .address(DeviceAddress::I2cBus { bus: 1 })
        .build()
        .expect_err("mismatched address should fail");

    assert!(matches!(
        descriptor,
        CoreError::AddressInterfaceMismatch { .. }
    ));
}
