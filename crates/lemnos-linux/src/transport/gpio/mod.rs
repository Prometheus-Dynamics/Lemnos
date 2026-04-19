use crate::LinuxPaths;
use crate::backend::BACKEND_NAME;
use crate::backend::LinuxTransportConfig;
#[cfg(feature = "gpio-cdev")]
use crate::metadata::descriptor_devnode;
#[cfg(feature = "gpio-sysfs")]
use crate::util::read_u32;
use lemnos_bus::{BusError, BusResult, GpioSession, SessionAccess};
#[cfg(any(feature = "gpio-cdev", feature = "gpio-sysfs"))]
use lemnos_core::DeviceAddress;
use lemnos_core::{DeviceDescriptor, DeviceKind, InterfaceKind};

#[cfg(feature = "gpio-cdev")]
mod cdev;
#[cfg(feature = "gpio-sysfs")]
mod sysfs;

pub(crate) fn supports_descriptor(device: &DeviceDescriptor) -> bool {
    if device.interface != InterfaceKind::Gpio || device.kind != DeviceKind::GpioLine {
        return false;
    }

    #[cfg(feature = "gpio-cdev")]
    if cdev::supports_descriptor(device) {
        return true;
    }

    #[cfg(feature = "gpio-sysfs")]
    if sysfs::supports_descriptor(device) {
        return true;
    }

    false
}

pub(crate) fn open_session(
    paths: &LinuxPaths,
    _transport_config: &LinuxTransportConfig,
    device: &DeviceDescriptor,
    access: SessionAccess,
) -> BusResult<Box<dyn GpioSession>> {
    #[cfg(not(feature = "gpio-sysfs"))]
    let _ = paths;
    #[cfg(not(any(feature = "gpio-cdev", feature = "gpio-sysfs")))]
    let _ = access;

    if !supports_descriptor(device) {
        return Err(BusError::UnsupportedDevice {
            backend: BACKEND_NAME.to_string(),
            device_id: device.id.clone(),
        });
    }

    #[cfg(feature = "gpio-cdev")]
    if cdev::can_use_transport(paths, device) {
        return cdev::open_session(paths, device, access);
    }

    #[cfg(feature = "gpio-sysfs")]
    if sysfs::can_use_transport(paths, device) {
        return sysfs::open_session(paths, _transport_config, device, access);
    }

    Err(BusError::SessionUnavailable {
        device_id: device.id.clone(),
        reason: "no enabled Linux GPIO transport can service this device".into(),
    })
}

#[cfg(any(feature = "gpio-cdev", feature = "gpio-sysfs"))]
fn gpio_line_address(device: &DeviceDescriptor) -> Option<(&str, u32)> {
    match &device.address {
        Some(DeviceAddress::GpioLine { chip_name, offset }) => Some((chip_name.as_str(), *offset)),
        _ => None,
    }
}

#[cfg(feature = "gpio-cdev")]
fn resolve_chip_devnode(paths: &LinuxPaths, device: &DeviceDescriptor) -> Option<String> {
    if let Some(devnode) = descriptor_devnode(device) {
        return Some(devnode.to_string());
    }

    let (chip_name, _) = gpio_line_address(device)?;
    Some(paths.gpio_devnode(chip_name).display().to_string())
}

#[cfg(feature = "gpio-sysfs")]
fn resolve_global_line(paths: &LinuxPaths, device: &DeviceDescriptor) -> Option<u32> {
    let (chip_name, offset) = gpio_line_address(device)?;
    let base = read_u32(&paths.gpio_class_root().join(chip_name).join("base"))
        .ok()
        .flatten()?;
    base.checked_add(offset)
}
