//! The small vocabulary every transfer and descriptor uses.

/// Which way data moves: host to device (`Out`) or device to host (`In`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Host to device.
    Out,
    /// Device to host.
    In,
}

/// An endpoint's transfer type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransferType {
    /// Control.
    Control,
    /// Isochronous.
    Isochronous,
    /// Bulk.
    Bulk,
    /// Interrupt.
    Interrupt,
}

/// The class of a control request (`bmRequestType` bits 5 and 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlKind {
    /// A request every device answers (USB 2.0 chapter 9).
    Standard,
    /// A request for a device class.
    Class,
    /// A request the device's own driver defines.
    Vendor,
}

/// What a control request is addressed to (`bmRequestType` bits 0 to 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlRecipient {
    /// The device.
    Device,
    /// An interface (its number goes in `index`).
    Interface,
    /// An endpoint (its address goes in `index`).
    Endpoint,
    /// Something else, defined by the class.
    Other,
}

/// The setup packet of a control transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ControlSetup {
    /// `bmRequestType` bit 7.
    pub direction: Direction,
    /// `bmRequestType` bits 5 and 6.
    pub kind: ControlKind,
    /// `bmRequestType` bits 0 to 4.
    pub recipient: ControlRecipient,
    /// `bRequest`.
    pub request: u8,
    /// `wValue`.
    pub value: u16,
    /// `wIndex`.
    pub index: u16,
}

impl ControlSetup {
    /// A vendor request to the device.
    pub fn vendor(direction: Direction, request: u8, value: u16, index: u16) -> Self {
        Self {
            direction,
            kind: ControlKind::Vendor,
            recipient: ControlRecipient::Device,
            request,
            value,
            index,
        }
    }

    /// The `bmRequestType` byte this setup encodes to.
    pub fn request_type(&self) -> u8 {
        let direction = match self.direction {
            Direction::In => 0x80,
            Direction::Out => 0x00,
        };
        let kind = match self.kind {
            ControlKind::Standard => 0x00,
            ControlKind::Class => 0x20,
            ControlKind::Vendor => 0x40,
        };
        let recipient = match self.recipient {
            ControlRecipient::Device => 0x00,
            ControlRecipient::Interface => 0x01,
            ControlRecipient::Endpoint => 0x02,
            ControlRecipient::Other => 0x03,
        };
        direction | kind | recipient
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_request_type_byte() {
        let vendor_in = ControlSetup::vendor(Direction::In, 0x01, 0, 0);
        assert_eq!(vendor_in.request_type(), 0xc0);
        let standard_out_endpoint = ControlSetup {
            direction: Direction::Out,
            kind: ControlKind::Standard,
            recipient: ControlRecipient::Endpoint,
            request: 1,
            value: 0,
            index: 0x81,
        };
        assert_eq!(standard_out_endpoint.request_type(), 0x02);
        let class_in_interface = ControlSetup {
            direction: Direction::In,
            kind: ControlKind::Class,
            recipient: ControlRecipient::Interface,
            request: 0x81,
            value: 0x0100,
            index: 1,
        };
        assert_eq!(class_in_interface.request_type(), 0xa1);
    }
}
