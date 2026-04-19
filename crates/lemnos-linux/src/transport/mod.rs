#[cfg(any(
    feature = "i2c",
    feature = "pwm",
    feature = "spi",
    feature = "uart",
    feature = "usb"
))]
use lemnos_core::{DeviceAddress, DeviceDescriptor};

pub(crate) mod gpio;
#[cfg(feature = "i2c")]
pub(crate) mod i2c;
#[cfg(feature = "pwm")]
pub(crate) mod pwm;
#[cfg(any(feature = "i2c", feature = "spi", feature = "uart", feature = "usb"))]
pub(crate) mod session;
#[cfg(feature = "spi")]
pub(crate) mod spi;
#[cfg(feature = "uart")]
pub(crate) mod uart;
#[cfg(feature = "usb")]
pub(crate) mod usb;

#[cfg(feature = "pwm")]
pub(crate) fn pwm_channel_address(device: &DeviceDescriptor) -> Option<(String, u32)> {
    match &device.address {
        Some(DeviceAddress::PwmChannel { chip_name, channel }) => {
            Some((chip_name.clone(), *channel))
        }
        _ => None,
    }
}

#[cfg(feature = "i2c")]
pub(crate) fn i2c_bus_address(device: &DeviceDescriptor) -> Option<(u32, u16)> {
    match &device.address {
        Some(DeviceAddress::I2cDevice { bus, address }) => Some((*bus, *address)),
        _ => None,
    }
}

#[cfg(feature = "spi")]
pub(crate) fn spi_bus_chip_select(device: &DeviceDescriptor) -> Option<(u32, u16)> {
    match &device.address {
        Some(DeviceAddress::SpiDevice { bus, chip_select }) => Some((*bus, *chip_select)),
        _ => None,
    }
}

#[cfg(feature = "uart")]
pub(crate) fn uart_port_name(device: &DeviceDescriptor) -> Option<String> {
    match &device.address {
        Some(DeviceAddress::UartPort { port }) => Some(port.clone()),
        _ => None,
    }
}

#[cfg(feature = "usb")]
pub(crate) fn usb_bus_ports(device: &DeviceDescriptor) -> Option<(u16, Vec<u8>)> {
    match &device.address {
        Some(DeviceAddress::UsbDevice { bus, ports, .. })
        | Some(DeviceAddress::UsbInterface { bus, ports, .. }) => Some((*bus, ports.clone())),
        _ => None,
    }
}
