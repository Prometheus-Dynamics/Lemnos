//! The descriptor model: configurations, interfaces, alternate settings and
//! endpoints, parsed from the raw descriptors the device reports.

use crate::error::{Error, ErrorKind, Result};
use crate::types::{Direction, TransferType};
use nusb::descriptors::ConfigurationDescriptor;

/// One endpoint of an alternate setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointInfo {
    /// `bEndpointAddress`: the number, with bit 7 for IN.
    pub address: u8,
    /// The direction, from the address.
    pub direction: Direction,
    /// The transfer type.
    pub transfer_type: TransferType,
    /// `wMaxPacketSize` (without the high-bandwidth multiplier).
    pub max_packet_size: u16,
    /// Packets per microframe (isochronous at high speed; 1 otherwise).
    pub packets_per_microframe: u8,
    /// `bInterval`.
    pub interval: u8,
}

/// One alternate setting of an interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlternateSetting {
    /// `bAlternateSetting`.
    pub alternate: u8,
    /// `bInterfaceClass`.
    pub class: u8,
    /// `bInterfaceSubClass`.
    pub subclass: u8,
    /// `bInterfaceProtocol`.
    pub protocol: u8,
    /// The endpoints of this setting.
    pub endpoints: Vec<EndpointInfo>,
}

/// An interface and its alternate settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceInfo {
    /// `bInterfaceNumber`.
    pub number: u8,
    /// The alternate settings, in descriptor order (setting 0 first).
    pub alternates: Vec<AlternateSetting>,
}

impl InterfaceInfo {
    /// The alternate setting `alternate`, if the interface has it.
    pub fn alternate(&self, alternate: u8) -> Option<&AlternateSetting> {
        self.alternates
            .iter()
            .find(|setting| setting.alternate == alternate)
    }
}

/// A configuration: its value and interfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationInfo {
    /// `bConfigurationValue`: the value to select with `set_configuration`.
    pub value: u8,
    /// The interfaces of the configuration.
    pub interfaces: Vec<InterfaceInfo>,
}

impl ConfigurationInfo {
    /// The interface numbered `number`, if the configuration has it.
    pub fn interface(&self, number: u8) -> Option<&InterfaceInfo> {
        self.interfaces
            .iter()
            .find(|interface| interface.number == number)
    }

    /// The first endpoint of `interface` (any alternate setting, or only
    /// `alternate` when given) with this direction and transfer type.
    pub fn find_endpoint(
        &self,
        interface: u8,
        alternate: Option<u8>,
        direction: Direction,
        transfer_type: TransferType,
    ) -> Option<EndpointInfo> {
        let interface = self.interface(interface)?;
        interface
            .alternates
            .iter()
            .filter(|setting| alternate.is_none_or(|wanted| setting.alternate == wanted))
            .flat_map(|setting| setting.endpoints.iter())
            .find(|endpoint| {
                endpoint.direction == direction && endpoint.transfer_type == transfer_type
            })
            .copied()
    }

    fn from_nusb(config: &ConfigurationDescriptor<'_>) -> Self {
        let interfaces = config
            .interfaces()
            .map(|interface| InterfaceInfo {
                number: interface.interface_number(),
                alternates: interface
                    .alt_settings()
                    .map(|alt| AlternateSetting {
                        alternate: alt.alternate_setting(),
                        class: alt.class(),
                        subclass: alt.subclass(),
                        protocol: alt.protocol(),
                        endpoints: alt.endpoints().map(endpoint_info).collect(),
                    })
                    .collect(),
            })
            .collect();
        Self {
            value: config.configuration_value(),
            interfaces,
        }
    }
}

fn endpoint_info(endpoint: nusb::descriptors::EndpointDescriptor<'_>) -> EndpointInfo {
    EndpointInfo {
        address: endpoint.address(),
        direction: match endpoint.direction() {
            nusb::transfer::Direction::In => Direction::In,
            nusb::transfer::Direction::Out => Direction::Out,
        },
        transfer_type: match endpoint.transfer_type() {
            nusb::descriptors::TransferType::Control => TransferType::Control,
            nusb::descriptors::TransferType::Isochronous => TransferType::Isochronous,
            nusb::descriptors::TransferType::Bulk => TransferType::Bulk,
            nusb::descriptors::TransferType::Interrupt => TransferType::Interrupt,
        },
        max_packet_size: endpoint.max_packet_size() as u16,
        packets_per_microframe: endpoint.packets_per_microframe(),
        interval: endpoint.interval(),
    }
}

/// Parses one configuration from its own descriptor bytes.
pub(crate) fn configuration_from_bytes(bytes: &[u8]) -> Result<ConfigurationInfo> {
    ConfigurationDescriptor::new(bytes)
        .map(|config| ConfigurationInfo::from_nusb(&config))
        .ok_or_else(|| Error::new(ErrorKind::Other, "malformed configuration descriptor"))
}

/// Splits the raw descriptor blob (a device descriptor, then one configuration
/// descriptor after another, each with its total length) into configurations.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn configurations_from_blob(blob: &[u8]) -> Result<Vec<ConfigurationInfo>> {
    const DEVICE_DESCRIPTOR_TYPE: u8 = 1;
    const CONFIGURATION_DESCRIPTOR_TYPE: u8 = 2;

    let mut rest = blob;
    if rest.len() >= 2 && rest[1] == DEVICE_DESCRIPTOR_TYPE {
        rest = &rest[usize::from(rest[0]).min(rest.len())..];
    }
    let mut configurations = Vec::new();
    while rest.len() >= 4 {
        if rest[1] != CONFIGURATION_DESCRIPTOR_TYPE {
            break;
        }
        let total = usize::from(u16::from_le_bytes([rest[2], rest[3]]));
        if total < 9 || total > rest.len() {
            return Err(Error::new(
                ErrorKind::Other,
                "truncated configuration descriptor",
            ));
        }
        configurations.push(configuration_from_bytes(&rest[..total])?);
        rest = &rest[total..];
    }
    Ok(configurations)
}
