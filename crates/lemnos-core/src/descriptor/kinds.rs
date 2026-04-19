use crate::InterfaceKind;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::fmt;

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceKind {
    Unspecified(InterfaceKind),
    GpioChip,
    GpioLine,
    PwmChip,
    PwmChannel,
    I2cBus,
    I2cDevice,
    SpiBus,
    SpiDevice,
    UartPort,
    UartDevice,
    UsbBus,
    UsbDevice,
    UsbInterface,
}

impl DeviceKind {
    pub const fn interface(self) -> InterfaceKind {
        match self {
            Self::Unspecified(interface) => interface,
            Self::GpioChip | Self::GpioLine => InterfaceKind::Gpio,
            Self::PwmChip | Self::PwmChannel => InterfaceKind::Pwm,
            Self::I2cBus | Self::I2cDevice => InterfaceKind::I2c,
            Self::SpiBus | Self::SpiDevice => InterfaceKind::Spi,
            Self::UartPort | Self::UartDevice => InterfaceKind::Uart,
            Self::UsbBus | Self::UsbDevice | Self::UsbInterface => InterfaceKind::Usb,
        }
    }
}

impl fmt::Display for DeviceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Unspecified(interface) => return write!(f, "unspecified({interface})"),
            Self::GpioChip => "gpio-chip",
            Self::GpioLine => "gpio-line",
            Self::PwmChip => "pwm-chip",
            Self::PwmChannel => "pwm-channel",
            Self::I2cBus => "i2c-bus",
            Self::I2cDevice => "i2c-device",
            Self::SpiBus => "spi-bus",
            Self::SpiDevice => "spi-device",
            Self::UartPort => "uart-port",
            Self::UartDevice => "uart-device",
            Self::UsbBus => "usb-bus",
            Self::UsbDevice => "usb-device",
            Self::UsbInterface => "usb-interface",
        };
        f.write_str(value)
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceAddress {
    GpioChip {
        chip_name: String,
        base_line: Option<u32>,
    },
    GpioLine {
        chip_name: String,
        offset: u32,
    },
    PwmChip {
        chip_name: String,
    },
    PwmChannel {
        chip_name: String,
        channel: u32,
    },
    I2cBus {
        bus: u32,
    },
    I2cDevice {
        bus: u32,
        address: u16,
    },
    SpiBus {
        bus: u32,
    },
    SpiDevice {
        bus: u32,
        chip_select: u16,
    },
    UartPort {
        port: String,
    },
    UartDevice {
        port: String,
        unit: Option<String>,
    },
    UsbBus {
        bus: u16,
    },
    UsbDevice {
        bus: u16,
        ports: Vec<u8>,
        vendor_id: Option<u16>,
        product_id: Option<u16>,
    },
    UsbInterface {
        bus: u16,
        ports: Vec<u8>,
        interface_number: u8,
        vendor_id: Option<u16>,
        product_id: Option<u16>,
    },
    Custom {
        interface: InterfaceKind,
        scheme: String,
        value: String,
    },
}

impl DeviceAddress {
    pub const fn interface(&self) -> InterfaceKind {
        match self {
            Self::GpioChip { .. } | Self::GpioLine { .. } => InterfaceKind::Gpio,
            Self::PwmChip { .. } | Self::PwmChannel { .. } => InterfaceKind::Pwm,
            Self::I2cBus { .. } | Self::I2cDevice { .. } => InterfaceKind::I2c,
            Self::SpiBus { .. } | Self::SpiDevice { .. } => InterfaceKind::Spi,
            Self::UartPort { .. } | Self::UartDevice { .. } => InterfaceKind::Uart,
            Self::UsbBus { .. } | Self::UsbDevice { .. } | Self::UsbInterface { .. } => {
                InterfaceKind::Usb
            }
            Self::Custom { interface, .. } => *interface,
        }
    }
}

impl fmt::Display for DeviceAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GpioChip {
                chip_name,
                base_line,
            } => {
                write!(f, "gpio-chip:{chip_name}")?;
                if let Some(base_line) = base_line {
                    write!(f, "@{base_line}")?;
                }
                Ok(())
            }
            Self::GpioLine { chip_name, offset } => write!(f, "gpio-line:{chip_name}:{offset}"),
            Self::PwmChip { chip_name } => write!(f, "pwm-chip:{chip_name}"),
            Self::PwmChannel { chip_name, channel } => {
                write!(f, "pwm-channel:{chip_name}:{channel}")
            }
            Self::I2cBus { bus } => write!(f, "i2c-bus:{bus}"),
            Self::I2cDevice { bus, address } => write!(f, "i2c-device:{bus}:0x{address:02x}"),
            Self::SpiBus { bus } => write!(f, "spi-bus:{bus}"),
            Self::SpiDevice { bus, chip_select } => write!(f, "spi-device:{bus}:{chip_select}"),
            Self::UartPort { port } => write!(f, "uart-port:{port}"),
            Self::UartDevice { port, unit } => {
                write!(f, "uart-device:{port}")?;
                if let Some(unit) = unit {
                    write!(f, ":{unit}")?;
                }
                Ok(())
            }
            Self::UsbBus { bus } => write!(f, "usb-bus:{bus}"),
            Self::UsbDevice {
                bus,
                ports,
                vendor_id,
                product_id,
            } => {
                write!(f, "usb-device:{bus}:{ports:?}")?;
                if let (Some(vendor_id), Some(product_id)) = (vendor_id, product_id) {
                    write!(f, "@{vendor_id:04x}:{product_id:04x}")?;
                }
                Ok(())
            }
            Self::UsbInterface {
                bus,
                ports,
                interface_number,
                vendor_id,
                product_id,
            } => {
                write!(f, "usb-interface:{bus}:{ports:?}:{interface_number}")?;
                if let (Some(vendor_id), Some(product_id)) = (vendor_id, product_id) {
                    write!(f, "@{vendor_id:04x}:{product_id:04x}")?;
                }
                Ok(())
            }
            Self::Custom {
                interface,
                scheme,
                value,
            } => write!(f, "{interface}:{scheme}:{value}"),
        }
    }
}
