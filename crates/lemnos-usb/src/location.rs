//! Where a device sits on the bus: a stable place, unlike the device address
//! the bus hands out on each enumeration.

use crate::error::{Error, ErrorKind};
use std::fmt;
use std::str::FromStr;

/// The bus a device is on, and the port numbers from the root hub down to it.
///
/// The form is the sysfs one: `3-1.2` is bus `3`, port 1, then port 2 of the
/// hub on it. The bus is the platform's bus identifier (on Linux the bus
/// number; zero padding is dropped). The location stays the same while the
/// device stays in the same port, unlike the address the bus hands out on
/// each enumeration.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UsbLocation {
    bus: String,
    ports: Vec<u8>,
}

impl UsbLocation {
    /// A location on bus `bus` through `ports` (non-empty for a device, not the root hub).
    pub fn new(bus: impl Into<String>, ports: impl Into<Vec<u8>>) -> Self {
        let bus = bus.into();
        // Linux pads bus numbers to three digits; the sysfs form does not.
        let bus = match bus.parse::<u32>() {
            Ok(number) if bus.bytes().all(|byte| byte.is_ascii_digit()) => number.to_string(),
            _ => bus,
        };
        Self {
            bus,
            ports: ports.into(),
        }
    }

    /// The bus identifier.
    pub fn bus(&self) -> &str {
        &self.bus
    }

    /// The port numbers, from the root hub down.
    pub fn ports(&self) -> &[u8] {
        &self.ports
    }
}

impl fmt::Display for UsbLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-", self.bus)?;
        for (index, port) in self.ports.iter().enumerate() {
            if index > 0 {
                f.write_str(".")?;
            }
            write!(f, "{port}")?;
        }
        Ok(())
    }
}

impl FromStr for UsbLocation {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || {
            Error::new(
                ErrorKind::InvalidInput,
                format!("not a USB location: {text:?}"),
            )
        };
        let (bus, ports) = text.split_once('-').ok_or_else(invalid)?;
        if bus.is_empty() {
            return Err(invalid());
        }
        let ports = ports
            .split('.')
            .map(|port| port.parse::<u8>().ok().filter(|&port| port > 0))
            .collect::<Option<Vec<u8>>>()
            .filter(|ports| !ports.is_empty())
            .ok_or_else(invalid)?;
        Ok(Self::new(bus, ports))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_in_the_sysfs_form() {
        assert_eq!(UsbLocation::new("3", vec![1, 2]).to_string(), "3-1.2");
        assert_eq!(UsbLocation::new("12", vec![2]).to_string(), "12-2");
    }

    #[test]
    fn parses_what_it_formats() {
        let location: UsbLocation = "3-1.2".parse().unwrap();
        assert_eq!(location, UsbLocation::new("3", vec![1, 2]));
        assert_eq!(location.bus(), "3");
        assert_eq!(location.ports(), &[1, 2]);
    }

    #[test]
    fn drops_zero_padding_from_linux_bus_numbers() {
        assert_eq!(
            UsbLocation::new("003", vec![1]),
            UsbLocation::new("3", vec![1])
        );
        assert_eq!(UsbLocation::new("usb-host", vec![1]).bus(), "usb-host");
    }

    #[test]
    fn rejects_malformed_locations() {
        for text in ["", "3", "-1", "3-", "3-0", "3-1..2", "3-1.x"] {
            let error = text.parse::<UsbLocation>().unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidInput, "{text:?}");
        }
    }
}
