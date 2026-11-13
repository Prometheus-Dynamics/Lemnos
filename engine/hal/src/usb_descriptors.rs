//! The descriptor set a [`UsbFunction`](crate::UsbFunction) presents to a host, as one run of
//! bytes that a microcontroller can serve as it stands and a larger system can take apart.
//!
//! # Layout
//!
//! Standard USB descriptors, back to back, each starting with its own `bLength` and
//! `bDescriptorType`, in this order and with nothing else between or after them:
//!
//! 1. The device descriptor: 18 bytes, type 1. `bNumConfigurations` is 1.
//! 2. The configuration at full speed: a configuration descriptor (9 bytes, type 2) followed by
//!    its interface, endpoint and class descriptors, `wTotalLength` bytes in all.
//! 3. Optionally the same configuration at high speed, in the same form. It is there when the
//!    descriptor after the full-speed configuration is of type 2. It differs from the
//!    full-speed one only in what a speed changes (packet sizes, intervals, class descriptors):
//!    the header fields other than `wTotalLength` are the same, and so are the interfaces,
//!    their alternate settings and the endpoint addresses and transfer types, in the same order.
//! 4. The string table: string descriptors (type 3) in index order from 0. Index 0 is the
//!    language list and names exactly one language; the others are UTF-16LE text in that
//!    language. A set that refers to no string may leave the table out.
//!
//! [`UsbDescriptors::parse`] checks all of it, and everything a backend goes on to read is
//! reached through the checked view, so no length or count in the bytes is taken on trust:
//!
//! - every `bLength` is at least 2 and stays inside what holds it, and each `wTotalLength` is
//!   exactly the bytes of its configuration;
//! - `bMaxPacketSize0` is 8, 16, 32 or 64;
//! - `bConfigurationValue` is not 0, and `bmAttributes` has bit 7 set and bits 0 to 4 clear;
//! - interfaces are numbered from 0 without gaps, each starting at alternate setting 0 with its
//!   further settings counting up, and `bNumInterfaces` is how many there are;
//! - each interface setting is followed by exactly `bNumEndpoints` endpoint descriptors before
//!   the next one, each with an endpoint number of 1 to 15, no reserved address bit set, a
//!   transfer type other than control, and an address no other endpoint of that setting has;
//! - no device, configuration or string descriptor sits inside a configuration;
//! - every string index the device, configuration and interface descriptors use is in the
//!   table, and every string is whole UTF-16 without a NUL.
//!
//! Super speed has no place in the layout yet.

use crate::ErrorKind;
use crate::blocks::UsbSpeed;

/// `bDescriptorType` of a device descriptor.
pub const DEVICE: u8 = 1;
/// `bDescriptorType` of a configuration descriptor.
pub const CONFIGURATION: u8 = 2;
/// `bDescriptorType` of a string descriptor.
pub const STRING: u8 = 3;
/// `bDescriptorType` of an interface descriptor.
pub const INTERFACE: u8 = 4;
/// `bDescriptorType` of an endpoint descriptor.
pub const ENDPOINT: u8 = 5;
/// Bytes in a device descriptor.
pub const DEVICE_LEN: usize = 18;
/// Bytes in a configuration descriptor, before what it holds.
pub const CONFIGURATION_LEN: usize = 9;
/// Bytes in an interface descriptor.
pub const INTERFACE_LEN: usize = 9;
/// The fewest bytes in an endpoint descriptor. Audio endpoints carry two more.
pub const ENDPOINT_LEN: usize = 7;

const BAD: ErrorKind = ErrorKind::InvalidInput;

/// One function of a gadget's configuration, in the order the board file lists them, which is
/// the order the configuration links them in. The FunctionFS function is the one whose
/// descriptors [`UsbDescriptors`] carries. The others are kernel functions the board's
/// privileged setup makes in configfs, and nothing the daemon writes to ep0 describes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GadgetFunction {
    /// The FunctionFS function: the link the daemon serves through `ep0` and its endpoint files.
    Ffs,
    /// A CDC network function (`ncm.usb0`). `host` is the host side's address, `device` the
    /// gadget's, each a unicast 6-byte MAC.
    Ncm { host: [u8; 6], device: [u8; 6] },
    /// A CDC ACM serial function (`acm.GS0`, the console).
    Acm,
}

/// What the device descriptor says, in the host's byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbIdentity {
    /// `bcdUSB`.
    pub usb: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    /// `bMaxPacketSize0`.
    pub max_packet: u8,
    pub vendor: u16,
    pub product: u16,
    /// `bcdDevice`.
    pub release: u16,
    /// String indexes, 0 for none.
    pub manufacturer: u8,
    pub product_name: u8,
    pub serial: u8,
}

/// The fields of a configuration descriptor that are the same at every speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbConfiguration {
    pub interfaces: u8,
    /// `bConfigurationValue`, never 0.
    pub value: u8,
    /// String index, 0 for none.
    pub name: u8,
    /// `bmAttributes`.
    pub attributes: u8,
    /// `bMaxPower`, in units of 2 mA.
    pub max_power: u8,
}

/// An endpoint descriptor's fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbEndpoint {
    /// Number, with bit 7 set for IN.
    pub address: u8,
    /// `bmAttributes`: the low two bits are the transfer type (1 isochronous, 2 bulk,
    /// 3 interrupt).
    pub attributes: u8,
    /// `wMaxPacketSize`, as it stands.
    pub max_packet: u16,
    pub interval: u8,
}

/// A descriptor set that passed the checks of the layout in the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbDescriptors<'a> {
    bytes: &'a [u8],
    /// End of the full-speed configuration.
    full_end: usize,
    /// End of the high-speed configuration: `full_end` when there is none.
    high_end: usize,
    /// String descriptors in the table, the language list included.
    strings: usize,
}

impl<'a> UsbDescriptors<'a> {
    /// Checks `bytes` against the layout. Anything that doesn't fit is `InvalidInput`.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ErrorKind> {
        let device = bytes.get(..DEVICE_LEN).ok_or(BAD)?;
        if usize::from(device[0]) != DEVICE_LEN || device[1] != DEVICE {
            return Err(BAD);
        }
        if !matches!(device[7], 8 | 16 | 32 | 64) || device[17] != 1 {
            return Err(BAD);
        }
        let full_end = configuration_end(bytes, DEVICE_LEN)?;
        let full = &bytes[DEVICE_LEN..full_end];
        check_configuration(full)?;
        let high_end = if bytes.get(full_end + 1) == Some(&CONFIGURATION) {
            let end = configuration_end(bytes, full_end)?;
            let high = &bytes[full_end..end];
            check_configuration(high)?;
            // Everything in the header but `wTotalLength`.
            if full[4..CONFIGURATION_LEN] != high[4..CONFIGURATION_LEN]
                || !shape(full).eq(shape(high))
            {
                return Err(BAD);
            }
            end
        } else {
            full_end
        };
        let strings = check_strings(&bytes[high_end..])?;
        let set = Self {
            bytes,
            full_end,
            high_end,
            strings,
        };
        let identity = set.identity();
        let named = [
            identity.manufacturer,
            identity.product_name,
            identity.serial,
            set.configuration().name,
        ];
        let interfaces = descriptors(set.full_speed())
            .chain(descriptors(set.high_speed().unwrap_or(&[])))
            .filter(|d| d[1] == INTERFACE)
            .map(|d| d[8]);
        for index in named.into_iter().chain(interfaces) {
            if index != 0 && usize::from(index) >= strings {
                return Err(BAD);
            }
        }
        Ok(set)
    }

    /// The whole set, as it was given.
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// The device descriptor, 18 bytes.
    pub fn device(&self) -> &'a [u8] {
        &self.bytes[..DEVICE_LEN]
    }

    pub fn identity(&self) -> UsbIdentity {
        let d = self.device();
        let word = |at: usize| u16::from_le_bytes([d[at], d[at + 1]]);
        UsbIdentity {
            usb: word(2),
            class: d[4],
            subclass: d[5],
            protocol: d[6],
            max_packet: d[7],
            vendor: word(8),
            product: word(10),
            release: word(12),
            manufacturer: d[14],
            product_name: d[15],
            serial: d[16],
        }
    }

    /// The configuration's fields, which are the same at both speeds.
    pub fn configuration(&self) -> UsbConfiguration {
        let c = self.full_speed();
        UsbConfiguration {
            interfaces: c[4],
            value: c[5],
            name: c[6],
            attributes: c[7],
            max_power: c[8],
        }
    }

    /// The configuration at full speed, header included.
    pub fn full_speed(&self) -> &'a [u8] {
        &self.bytes[DEVICE_LEN..self.full_end]
    }

    /// The configuration at high speed, header included, if the set has one.
    pub fn high_speed(&self) -> Option<&'a [u8]> {
        (self.high_end > self.full_end).then(|| &self.bytes[self.full_end..self.high_end])
    }

    /// The configuration to serve at `speed`: the high-speed one at high speed when the set has
    /// it, the full-speed one otherwise.
    pub fn at_speed(&self, speed: UsbSpeed) -> &'a [u8] {
        match (speed, self.high_speed()) {
            (UsbSpeed::High, Some(high)) => high,
            _ => self.full_speed(),
        }
    }

    /// How many string descriptors the table holds, the language list at index 0 included.
    pub fn string_count(&self) -> usize {
        self.strings
    }

    /// The language of the strings, if there is a table.
    pub fn language(&self) -> Option<u16> {
        let list = self.string(0)?;
        Some(u16::from_le_bytes([list[2], list[3]]))
    }

    /// The whole string descriptor at `index`, header included.
    pub fn string(&self, index: u8) -> Option<&'a [u8]> {
        descriptors(&self.bytes[self.high_end..]).nth(usize::from(index))
    }

    /// The text of the string at `index` (not 0), as UTF-16 code units.
    pub fn text(&self, index: u8) -> Option<impl Iterator<Item = u16> + 'a> {
        if index == 0 {
            return None;
        }
        Some(units(&self.string(index)?[2..]))
    }
}

/// The endpoints `configuration` names, in order. `configuration` is one a [`UsbDescriptors`]
/// handed out.
pub fn endpoints(configuration: &[u8]) -> impl Iterator<Item = UsbEndpoint> + '_ {
    descriptors(configuration)
        .filter(|d| d[1] == ENDPOINT && d.len() >= ENDPOINT_LEN)
        .map(|d| UsbEndpoint {
            address: d[2],
            attributes: d[3],
            max_packet: u16::from_le_bytes([d[4], d[5]]),
            interval: d[6],
        })
}

/// Each descriptor in `bytes`, whole, in order. Stops at the first that doesn't fit, which
/// there is none of in bytes a [`UsbDescriptors`] handed out.
pub fn descriptors(bytes: &[u8]) -> impl Iterator<Item = &[u8]> + '_ {
    let mut rest = bytes;
    core::iter::from_fn(move || {
        let len = usize::from(*rest.first()?);
        if len < 2 || len > rest.len() {
            return None;
        }
        let (one, after) = rest.split_at(len);
        rest = after;
        Some(one)
    })
}

fn units(text: &[u8]) -> impl Iterator<Item = u16> + '_ {
    text.as_chunks::<2>()
        .0
        .iter()
        .map(|unit| u16::from_le_bytes(*unit))
}

/// Where the configuration that starts at `start` ends, having checked its header.
fn configuration_end(bytes: &[u8], start: usize) -> Result<usize, ErrorKind> {
    let header = bytes
        .get(start..)
        .and_then(|rest| rest.get(..CONFIGURATION_LEN))
        .ok_or(BAD)?;
    if usize::from(header[0]) != CONFIGURATION_LEN || header[1] != CONFIGURATION {
        return Err(BAD);
    }
    let total = usize::from(u16::from_le_bytes([header[2], header[3]]));
    let end = start.checked_add(total).ok_or(BAD)?;
    if total < CONFIGURATION_LEN || end > bytes.len() {
        return Err(BAD);
    }
    if header[5] == 0 || header[7] & 0x80 == 0 || header[7] & 0x1f != 0 {
        return Err(BAD);
    }
    Ok(end)
}

/// Checks what a whole configuration holds after its header.
fn check_configuration(configuration: &[u8]) -> Result<(), ErrorKind> {
    let body = &configuration[CONFIGURATION_LEN..];
    // The walk stops at a descriptor that doesn't fit, so its lengths must add up to all of it.
    if descriptors(body).map(<[u8]>::len).sum::<usize>() != body.len() {
        return Err(BAD);
    }
    // The interface setting being read: its number, its alternate setting, the endpoints it
    // still owes, and the addresses it has named so far (bit n for OUT n, bit 16 + n for IN n).
    let mut current: Option<(u8, u8)> = None;
    let mut owed = 0u8;
    let mut seen = 0u32;
    for d in descriptors(body) {
        match d[1] {
            DEVICE | CONFIGURATION | STRING => return Err(BAD),
            INTERFACE => {
                if d.len() != INTERFACE_LEN || owed != 0 {
                    return Err(BAD);
                }
                let (number, alternate) = (d[2], d[3]);
                let next = match current {
                    None => (0, 0),
                    Some((n, _)) if alternate == 0 => (n.checked_add(1).ok_or(BAD)?, 0),
                    Some((n, a)) => (n, a.checked_add(1).ok_or(BAD)?),
                };
                if (number, alternate) != next {
                    return Err(BAD);
                }
                current = Some(next);
                owed = d[4];
                seen = 0;
            }
            ENDPOINT => {
                if d.len() < ENDPOINT_LEN || owed == 0 {
                    return Err(BAD);
                }
                let (address, attributes) = (d[2], d[3]);
                let number = address & 0x0f;
                if number == 0 || address & 0x70 != 0 || attributes & 3 == 0 {
                    return Err(BAD);
                }
                let bit = 1u32 << (u32::from(number) + u32::from(address >> 7) * 16);
                if seen & bit != 0 {
                    return Err(BAD);
                }
                seen |= bit;
                owed -= 1;
            }
            _ => {}
        }
    }
    let interfaces = current.map_or(0, |(n, _)| usize::from(n) + 1);
    if owed != 0 || interfaces != usize::from(configuration[4]) {
        return Err(BAD);
    }
    Ok(())
}

/// What two speeds of one configuration must share: each interface setting and each endpoint's
/// address and transfer type, in order.
fn shape(configuration: &[u8]) -> impl Iterator<Item = (u8, u8, u8)> + '_ {
    descriptors(&configuration[CONFIGURATION_LEN..]).filter_map(|d| match d[1] {
        INTERFACE => Some((INTERFACE, d[2], d[3])),
        ENDPOINT => Some((ENDPOINT, d[2], d[3] & 3)),
        _ => None,
    })
}

/// Checks the string table and returns how many descriptors it holds.
fn check_strings(table: &[u8]) -> Result<usize, ErrorKind> {
    let mut count = 0usize;
    let mut used = 0usize;
    for d in descriptors(table) {
        if d[1] != STRING || d.len() % 2 != 0 {
            return Err(BAD);
        }
        if count == 0 {
            if d.len() != 4 {
                return Err(BAD);
            }
        } else if !well_formed(units(&d[2..])) {
            return Err(BAD);
        }
        count += 1;
        used += d.len();
    }
    // An index is one byte.
    if used != table.len() || count > 256 {
        return Err(BAD);
    }
    Ok(count)
}

/// Whether `units` is whole UTF-16 (every surrogate paired) with no NUL in it.
fn well_formed(units: impl Iterator<Item = u16>) -> bool {
    let mut high = false;
    for unit in units {
        let paired = match unit {
            0 => return false,
            0xd800..=0xdbff => {
                if high {
                    return false;
                }
                high = true;
                continue;
            }
            0xdc00..=0xdfff => high,
            _ => !high,
        };
        if !paired {
            return false;
        }
        high = false;
    }
    !high
}

#[cfg(test)]
mod tests;
