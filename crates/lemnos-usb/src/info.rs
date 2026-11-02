//! What a listing says about a device, before it is opened.

use crate::location::UsbLocation;

/// The negotiated bus speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Speed {
    /// 1.5 Mbit/s.
    Low,
    /// 12 Mbit/s.
    Full,
    /// 480 Mbit/s.
    High,
    /// 5 Gbit/s.
    Super,
    /// 10 Gbit/s.
    SuperPlus,
}

impl Speed {
    fn from_nusb(speed: nusb::Speed) -> Option<Self> {
        Some(match speed {
            nusb::Speed::Low => Self::Low,
            nusb::Speed::Full => Self::Full,
            nusb::Speed::High => Self::High,
            nusb::Speed::Super => Self::Super,
            nusb::Speed::SuperPlus => Self::SuperPlus,
            _ => return None,
        })
    }
}

/// An interface as a listing shows it: its number, class and name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceSummary {
    /// `bInterfaceNumber`.
    pub number: u8,
    /// `bInterfaceClass`.
    pub class: u8,
    /// `bInterfaceSubClass`.
    pub subclass: u8,
    /// `bInterfaceProtocol`.
    pub protocol: u8,
    /// The interface's string descriptor, when the device has one.
    pub name: Option<String>,
}

/// One device as a listing shows it. Open it with [`crate::UsbDevice::open`].
#[derive(Clone)]
pub struct UsbDeviceInfo {
    location: UsbLocation,
    vendor_id: u16,
    product_id: u16,
    class: u8,
    subclass: u8,
    protocol: u8,
    manufacturer: Option<String>,
    product: Option<String>,
    serial_number: Option<String>,
    speed: Option<Speed>,
    interfaces: Vec<InterfaceSummary>,
    driver: Option<String>,
    native: nusb::DeviceInfo,
}

impl std::fmt::Debug for UsbDeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsbDeviceInfo")
            .field("location", &self.location.to_string())
            .field("vendor_id", &format_args!("{:04x}", self.vendor_id))
            .field("product_id", &format_args!("{:04x}", self.product_id))
            .field("product", &self.product)
            .field("serial_number", &self.serial_number)
            .finish_non_exhaustive()
    }
}

impl UsbDeviceInfo {
    pub(crate) fn from_nusb(info: &nusb::DeviceInfo) -> Self {
        let interfaces = info
            .interfaces()
            .map(|interface| InterfaceSummary {
                number: interface.interface_number(),
                class: interface.class(),
                subclass: interface.subclass(),
                protocol: interface.protocol(),
                name: interface.interface_string().map(str::to_owned),
            })
            .collect();
        #[cfg(windows)]
        let driver = info.driver().map(str::to_owned);
        #[cfg(not(windows))]
        let driver = None;
        Self {
            location: UsbLocation::new(info.bus_id(), info.port_chain().to_vec()),
            vendor_id: info.vendor_id(),
            product_id: info.product_id(),
            class: info.class(),
            subclass: info.subclass(),
            protocol: info.protocol(),
            manufacturer: info.manufacturer_string().map(str::to_owned),
            product: info.product_string().map(str::to_owned),
            serial_number: info.serial_number().map(str::to_owned),
            speed: info.speed().and_then(Speed::from_nusb),
            interfaces,
            driver,
            native: info.clone(),
        }
    }

    /// Where the device is on the bus (stable while it stays in its port).
    pub fn location(&self) -> &UsbLocation {
        &self.location
    }

    /// `idVendor`.
    pub fn vendor_id(&self) -> u16 {
        self.vendor_id
    }

    /// `idProduct`.
    pub fn product_id(&self) -> u16 {
        self.product_id
    }

    /// `bDeviceClass`.
    pub fn class(&self) -> u8 {
        self.class
    }

    /// `bDeviceSubClass`.
    pub fn subclass(&self) -> u8 {
        self.subclass
    }

    /// `bDeviceProtocol`.
    pub fn protocol(&self) -> u8 {
        self.protocol
    }

    /// The manufacturer string, when the device reports one.
    pub fn manufacturer(&self) -> Option<&str> {
        self.manufacturer.as_deref()
    }

    /// The product string, when the device reports one.
    pub fn product(&self) -> Option<&str> {
        self.product.as_deref()
    }

    /// The serial number string, when the device reports one.
    pub fn serial_number(&self) -> Option<&str> {
        self.serial_number.as_deref()
    }

    /// The negotiated speed, when the platform reports it.
    pub fn speed(&self) -> Option<Speed> {
        self.speed
    }

    /// The interfaces the listing reports for the active configuration.
    pub fn interfaces(&self) -> &[InterfaceSummary] {
        &self.interfaces
    }

    /// The driver bound to the device as a whole, as the platform names it
    /// (Windows only; `None` elsewhere).
    pub fn driver(&self) -> Option<&str> {
        self.driver.as_deref()
    }

    /// On Windows, whether WinUSB is the driver bound to the device, so a tool
    /// can offer to install it. `None` on other platforms, and when Windows
    /// has no driver to report. A composite device binds WinUSB per interface,
    /// which this listing does not show.
    pub fn winusb_bound(&self) -> Option<bool> {
        if !cfg!(windows) {
            return None;
        }
        self.driver
            .as_deref()
            .map(|driver| driver.eq_ignore_ascii_case("winusb"))
    }

    /// The native handle the open path uses.
    pub(crate) fn native(&self) -> &nusb::DeviceInfo {
        &self.native
    }
}
