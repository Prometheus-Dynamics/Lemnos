use lemnos_core::{DeviceDescriptor, DeviceDescriptorBuilder};

pub(crate) const DEVNODE_PROPERTY: &str = "devnode";
pub(crate) const DRIVER_PROPERTY: &str = "driver";
pub(crate) const LINUX_DRIVER_PROPERTY: &str = "linux.driver";

pub(crate) fn descriptor_devnode(device: &DeviceDescriptor) -> Option<&str> {
    descriptor_string_property(device, DEVNODE_PROPERTY)
}

pub(crate) fn descriptor_driver(device: &DeviceDescriptor) -> Option<&str> {
    descriptor_string_property(device, DRIVER_PROPERTY)
}

pub(crate) fn with_devnode(
    builder: DeviceDescriptorBuilder,
    devnode: impl Into<String>,
) -> DeviceDescriptorBuilder {
    builder.property(DEVNODE_PROPERTY, devnode.into())
}

pub(crate) fn with_driver(
    builder: DeviceDescriptorBuilder,
    driver: impl Into<String>,
) -> DeviceDescriptorBuilder {
    builder.property(DRIVER_PROPERTY, driver.into())
}

pub(crate) fn with_linux_driver(
    builder: DeviceDescriptorBuilder,
    driver: impl Into<String>,
) -> DeviceDescriptorBuilder {
    builder.property(LINUX_DRIVER_PROPERTY, driver.into())
}

fn descriptor_string_property<'a>(device: &'a DeviceDescriptor, key: &str) -> Option<&'a str> {
    device.properties.get(key).and_then(|value| value.as_str())
}
