//! What a board name can say a USB device is, rather than where it is plugged in: its vendor and
//! product, or the class codes of the device or of one of its interfaces.
//!
//! | Name | Fits a device |
//! |---|---|
//! | `usb:VVVV:PPPP` | whose `idVendor` and `idProduct` are these, four hex digits each |
//! | `usb:class:CC` | whose device descriptor or one of whose interfaces has class `CC` |
//! | `usb:class:CC:SS` | the same, with subclass `SS` |
//! | `usb:class:CC:SS:PP` | the same, with protocol `PP` |
//!
//! Class codes are two hex digits each. A device whose class is given per interface (device
//! class 0, or 0xEF with interface association) is matched by its interfaces, so a video camera
//! fits `usb:class:0e` whatever its device descriptor says. Subclass and protocol are matched
//! against the same descriptor as the class: a device can't fit by the class of one interface and
//! the subclass of another.
//!
//! Nothing here reads a device. A backend reads the codes the way its platform shows them and
//! asks [`UsbPattern::fits`].

use crate::usb_descriptors::{self, INTERFACE};

const PREFIX: &str = "usb:";
const CLASS: &str = "class:";

/// The codes of a device descriptor a name can match on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UsbCodes {
    pub vendor: u16,
    pub product: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
}

/// A class, subclass and protocol, as a device or an interface descriptor gives them.
pub type ClassCodes = [u8; 3];

/// A board name that says what a USB device is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbPattern {
    /// `usb:VVVV:PPPP`.
    Id { vendor: u16, product: u16 },
    /// `usb:class:CC[:SS[:PP]]`. A part left out fits anything.
    Class {
        class: u8,
        subclass: Option<u8>,
        protocol: Option<u8>,
    },
}

impl UsbPattern {
    /// The pattern `name` spells, or `None` for any other name, a port among them.
    pub fn parse(name: &str) -> Option<Self> {
        let rest = name.strip_prefix(PREFIX)?;
        if let Some(codes) = rest.strip_prefix(CLASS) {
            let mut parts = codes.split(':');
            let class = hex2(parts.next()?)?;
            let subclass = parts.next().map(hex2);
            let protocol = parts.next().map(hex2);
            if parts.next().is_some() || subclass == Some(None) || protocol == Some(None) {
                return None;
            }
            return Some(Self::Class {
                class,
                subclass: subclass.flatten(),
                protocol: protocol.flatten(),
            });
        }
        let (vendor, product) = rest.split_once(':')?;
        Some(Self::Id {
            vendor: hex4(vendor)?,
            product: hex4(product)?,
        })
    }

    /// Whether a device with the device descriptor `device` and interfaces of `interfaces` fits.
    pub fn fits(
        &self,
        device: &UsbCodes,
        interfaces: impl IntoIterator<Item = ClassCodes>,
    ) -> bool {
        match *self {
            Self::Id { vendor, product } => (device.vendor, device.product) == (vendor, product),
            Self::Class { .. } => {
                let own = [device.class, device.subclass, device.protocol];
                self.fits_class(own) || interfaces.into_iter().any(|codes| self.fits_class(codes))
            }
        }
    }

    fn fits_class(&self, [class, subclass, protocol]: ClassCodes) -> bool {
        match *self {
            Self::Class {
                class: want,
                subclass: want_sub,
                protocol: want_protocol,
            } => {
                class == want
                    && want_sub.is_none_or(|want| want == subclass)
                    && want_protocol.is_none_or(|want| want == protocol)
            }
            Self::Id { .. } => false,
        }
    }
}

/// The class codes of each interface setting in a configuration descriptor, header included,
/// in order. Stops at the first descriptor that doesn't fit, so bytes from a device can be given
/// as they came.
pub fn interface_classes(configuration: &[u8]) -> impl Iterator<Item = ClassCodes> + '_ {
    let body = configuration
        .get(usb_descriptors::CONFIGURATION_LEN..)
        .unwrap_or(&[]);
    usb_descriptors::descriptors(body)
        .filter(|d| d[1] == INTERFACE && d.len() >= usb_descriptors::INTERFACE_LEN)
        .map(|d| [d[5], d[6], d[7]])
}

fn hex2(text: &str) -> Option<u8> {
    let valid = text.len() == 2 && text.bytes().all(|b| b.is_ascii_hexdigit());
    valid.then(|| u8::from_str_radix(text, 16).ok())?
}

fn hex4(text: &str) -> Option<u16> {
    let valid = text.len() == 4 && text.bytes().all(|b| b.is_ascii_hexdigit());
    valid.then(|| u16::from_str_radix(text, 16).ok())?
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAMERA: UsbCodes = UsbCodes {
        vendor: 0x1a2b,
        product: 0x3c4d,
        class: 0xef,
        subclass: 2,
        protocol: 1,
    };
    /// A video control and a video streaming interface.
    const VIDEO: [ClassCodes; 2] = [[0x0e, 1, 0], [0x0e, 2, 0]];

    #[test]
    fn names_parse_as_ids_or_classes() {
        assert_eq!(
            UsbPattern::parse("usb:1A2b:00ff"),
            Some(UsbPattern::Id {
                vendor: 0x1a2b,
                product: 0x00ff
            })
        );
        assert_eq!(
            UsbPattern::parse("usb:class:0e"),
            Some(UsbPattern::Class {
                class: 0x0e,
                subclass: None,
                protocol: None
            })
        );
        assert_eq!(
            UsbPattern::parse("usb:class:0E:02:01"),
            Some(UsbPattern::Class {
                class: 0x0e,
                subclass: Some(2),
                protocol: Some(1)
            })
        );
        for name in [
            "usb:1-1.4",
            "usb:class:",
            "usb:class:e",
            "usb:class:0e:",
            "usb:class:0e:1",
            "usb:class:0e:01:02:03",
            "usb:class:zz",
            "usb:1a2b",
            "usb:1a2b:3c4d:1",
            "usb:+a2b:3c4d",
            "class:0e",
            "i2c-1",
        ] {
            assert_eq!(UsbPattern::parse(name), None, "{name}");
        }
    }

    #[test]
    fn an_id_fits_only_its_vendor_and_product() {
        let fits = |name: &str| UsbPattern::parse(name).unwrap().fits(&CAMERA, VIDEO);
        assert!(fits("usb:1a2b:3c4d"));
        assert!(!fits("usb:1a2b:3c4e"));
        assert!(!fits("usb:1a2c:3c4d"));
    }

    #[test]
    fn a_class_fits_the_device_or_any_one_interface() {
        let fits = |name: &str| UsbPattern::parse(name).unwrap().fits(&CAMERA, VIDEO);
        assert!(fits("usb:class:0e"));
        assert!(fits("usb:class:0e:01"));
        assert!(fits("usb:class:0e:02:00"));
        assert!(fits("usb:class:ef:02:01"));
        assert!(!fits("usb:class:0e:03"));
        assert!(!fits("usb:class:0e:01:01"));
        assert!(!fits("usb:class:08"));
        // Subclass and protocol come from the same descriptor as the class.
        assert!(!fits("usb:class:ef:01"));
        assert!(!UsbPattern::parse("usb:class:0e").unwrap().fits(&CAMERA, []));
    }

    #[test]
    fn interface_classes_come_from_a_configuration_and_stop_at_damage() {
        let configuration = [
            9, 2, 32, 0, 2, 1, 0, 0x80, 50, // configuration
            9, 4, 0, 0, 0, 0x0e, 1, 0, 0, // interface 0
            5, 0x24, 1, 0, 1, // a class descriptor
            9, 4, 1, 0, 0, 0x0e, 2, 0, 0, // interface 1
        ];
        let found: [ClassCodes; 2] = [[0x0e, 1, 0], [0x0e, 2, 0]];
        assert!(interface_classes(&configuration).eq(found));
        // A length that runs past the end ends the walk.
        let mut damaged = configuration;
        damaged[23] = 200;
        assert!(interface_classes(&damaged).eq(found[..1].iter().copied()));
        assert_eq!(interface_classes(&[]).count(), 0);
    }
}
