//! The device descriptor: the 18 bytes that identify a device (its versions,
//! class, IDs and string indices). Parsed once, when the device is opened.

use crate::error::{Error, ErrorKind, Result};

/// `bLength` of a device descriptor.
const LENGTH: usize = 18;
/// `bDescriptorType` of a device descriptor.
const DEVICE_TYPE: u8 = 1;

/// A device's descriptor (USB 2.0 section 9.6.1), as the device reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceDescriptor {
    /// `bcdUSB`: the USB specification the device follows (`0x0200` for USB 2.0,
    /// `0x0300` for USB 3.x). A boot ROM and its loader can differ here.
    pub usb_version: u16,
    /// `bDeviceClass`.
    pub class: u8,
    /// `bDeviceSubClass`.
    pub subclass: u8,
    /// `bDeviceProtocol`.
    pub protocol: u8,
    /// `bMaxPacketSize0`: the control endpoint's packet size.
    pub max_packet_size_0: u8,
    /// `idVendor`.
    pub vendor_id: u16,
    /// `idProduct`.
    pub product_id: u16,
    /// `bcdDevice`: the device's release number.
    pub device_version: u16,
    /// `iManufacturer`: the string index of the manufacturer (0: none).
    pub manufacturer_index: u8,
    /// `iProduct`: the string index of the product name (0: none).
    pub product_index: u8,
    /// `iSerialNumber`: the string index of the serial number (0: none).
    pub serial_number_index: u8,
    /// `bNumConfigurations`.
    pub num_configurations: u8,
}

impl DeviceDescriptor {
    /// Parses the device descriptor at the start of `bytes`. Extra bytes after
    /// it are ignored: a raw descriptor blob starts with this one.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < LENGTH || bytes[0] as usize != LENGTH || bytes[1] != DEVICE_TYPE {
            return Err(Error::new(
                ErrorKind::Other,
                "malformed device descriptor: expected 18 bytes of type 1",
            ));
        }
        let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        Ok(Self {
            usb_version: u16_at(2),
            class: bytes[4],
            subclass: bytes[5],
            protocol: bytes[6],
            max_packet_size_0: bytes[7],
            vendor_id: u16_at(8),
            product_id: u16_at(10),
            device_version: u16_at(12),
            manufacturer_index: bytes[14],
            product_index: bytes[15],
            serial_number_index: bytes[16],
            num_configurations: bytes[17],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A USB 2.0 device descriptor, recorded from a boot ROM's device: vendor
    /// 0x2e8a, product 0x0003, release 1.00, serial string at index 3.
    const RECORDED: [u8; 18] = [
        0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x8a, 0x2e, 0x03, 0x00, 0x00, 0x01, 0x01,
        0x02, 0x03, 0x01,
    ];

    #[test]
    fn parses_the_recorded_device_descriptor() {
        let d = DeviceDescriptor::from_bytes(&RECORDED).unwrap();
        assert_eq!(d.usb_version, 0x0200);
        assert_eq!((d.class, d.subclass, d.protocol), (0, 0, 0));
        assert_eq!(d.max_packet_size_0, 64);
        assert_eq!((d.vendor_id, d.product_id), (0x2e8a, 0x0003));
        assert_eq!(d.device_version, 0x0100);
        assert_eq!(
            (d.manufacturer_index, d.product_index, d.serial_number_index),
            (1, 2, 3)
        );
        assert_eq!(d.num_configurations, 1);
    }

    #[test]
    fn a_blob_with_configurations_after_it_parses_the_device_part() {
        let mut blob = RECORDED.to_vec();
        blob.extend_from_slice(&[9, 2, 9, 0, 1, 1, 0, 0x80, 50]);
        assert_eq!(
            DeviceDescriptor::from_bytes(&blob).unwrap(),
            DeviceDescriptor::from_bytes(&RECORDED).unwrap()
        );
    }

    #[test]
    fn a_short_or_mistyped_descriptor_is_refused() {
        assert!(
            DeviceDescriptor::from_bytes(&RECORDED[..17]).is_err(),
            "truncated"
        );
        let mut wrong_type = RECORDED;
        wrong_type[1] = 2;
        assert!(
            DeviceDescriptor::from_bytes(&wrong_type).is_err(),
            "configuration type"
        );
        let mut wrong_length = RECORDED;
        wrong_length[0] = 9;
        assert!(
            DeviceDescriptor::from_bytes(&wrong_length).is_err(),
            "bLength"
        );
    }
}
