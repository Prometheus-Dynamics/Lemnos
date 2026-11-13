//! The gadget a board file describes, built into the descriptor set a
//! [`UsbFunction`](lemnos_hal::UsbFunction) takes
//! ([`lemnos_hal::usb_descriptors`]). It is built once, at bind, into a fixed buffer.
//!
//! The device's identity comes from scalar settings (see the crate docs). Its interfaces come
//! from the `interfaces` setting, which is text. Lines are `key words...`, `#` starts a
//! comment, blank lines are skipped, and a number is decimal or `0x` hex:
//!
//! - `interface <class> <subclass> <protocol> [name...]`: the next interface, numbered from 0
//!   in file order. The rest of the line, if any, is its name, which the host can read.
//! - `alternate [name...]`: another setting of the interface above, numbered from 1, with its
//!   class, subclass and protocol. The endpoints after it are the setting's. The host picks a
//!   setting with `SET_INTERFACE`; setting 0, the `interface` line's own, is in force until
//!   it does. An isochronous endpoint goes in a setting other than 0, so a host that has not
//!   asked for it reserves no bandwidth for it.
//! - `endpoint <in|out> bulk`: a bulk endpoint of the interface above, with the packet size of
//!   each speed (64 at full speed, 512 at high speed).
//! - `endpoint <in|out> interrupt <max-packet> <interval-ms>`: an interrupt endpoint, with a
//!   packet of 1 to 64 bytes polled every 1 to 255 ms (rounded down to a power of two at high
//!   speed, where intervals are powers of two).
//! - `endpoint <in|out> isochronous <bytes> <interval-ms>`: an isochronous endpoint moving up
//!   to 1 to 1023 bytes every `interval-ms`, a power of two from 1 to 4096 ms, at either speed.
//!   Its synchronisation type is asynchronous. Each send is one interval's bytes, and a
//!   receive needs room for them.
//!
//! Endpoints are numbered from 1 in file order, whatever their direction and setting, so there
//! are at most 15, and no address is in two settings. What the classes, subclasses and protocols mean, and what the endpoints carry, is the
//! business of whoever talks over them: nothing here looks.
//!
//! The set has the configuration at full speed and at high speed, one language (US English,
//! 0x0409) and USB 2.0 as its version. Class descriptors have no line here.

use crate::derive::Macs;
use lemnos_hal::usb_descriptors::{GadgetFunction, UsbDescriptors};

/// The most bytes a built descriptor set may come to.
pub const MAX_SET: usize = 1024;
/// The most interface settings a description may have, alternate settings included.
pub const MAX_INTERFACES: usize = 8;
/// The most bytes an isochronous endpoint moves in an interval: a full-speed packet's most.
pub const MAX_ISO_BYTES: u16 = 1023;
/// The longest interval of an isochronous endpoint, in ms: `bInterval` 16 at high speed.
pub const MAX_ISO_INTERVAL_MS: u16 = 4096;
/// The most endpoints: a device has 15 endpoint numbers besides 0.
pub const MAX_ENDPOINTS: usize = 15;
/// The most functions a gadget's configuration has: FunctionFS, NCM and ACM, one each.
pub const MAX_FUNCTIONS: usize = 3;
/// The most bytes of UTF-8 a name may have, so its descriptor fits one length byte whatever it
/// holds.
pub const MAX_NAME: usize = 63;

const LANGUAGE: u16 = 0x0409;

/// The device's own facts, from the scalar settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity<'a> {
    pub vendor: u16,
    pub product: u16,
    /// `bcdDevice`.
    pub release: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub manufacturer: Option<&'a str>,
    pub product_name: Option<&'a str>,
    pub serial: Option<&'a str>,
    /// The most current the device draws from the bus, in mA: at most 500.
    pub max_power_ma: u16,
    pub self_powered: bool,
}

/// Why a description can't be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// A line that is not one of the forms above, by its number from 1.
    Line(usize),
    /// An `endpoint` or `alternate` line before any `interface` line, by its number.
    NoInterface(usize),
    /// No `interface` line at all.
    Empty,
    TooManyInterfaces,
    TooManyEndpoints,
    /// A name, or one of the identity's strings, over [`MAX_NAME`] bytes or holding a NUL.
    Name,
    /// More current than a bus gives.
    Power,
    /// The set is over [`MAX_SET`] bytes.
    TooLong,
    /// A `function` line that is not `ffs`, `ncm host=MAC device=MAC`, `ncm derive` or `acm`, that
    /// lists a function twice, or that makes more than [`MAX_FUNCTIONS`]; by its number from 1.
    Function(usize),
    /// A `function ncm derive` line with no board identity to derive the addresses from (see
    /// [`crate::derive`]); by its number from 1.
    NoIdentity(usize),
}

/// A descriptor set, built.
#[derive(Clone)]
pub struct Set {
    bytes: [u8; MAX_SET],
    len: usize,
}

impl Set {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// Appends to a fixed buffer, remembering whether it ran out.
struct Writer {
    bytes: [u8; MAX_SET],
    len: usize,
    full: bool,
}

impl Writer {
    fn put(&mut self, bytes: &[u8]) {
        match self.bytes.get_mut(self.len..self.len + bytes.len()) {
            Some(room) => {
                room.copy_from_slice(bytes);
                self.len += bytes.len();
            }
            None => self.full = true,
        }
    }

    /// A string descriptor holding `text`, which is at most [`MAX_NAME`] bytes.
    fn string(&mut self, text: &str) {
        let units = text.encode_utf16().count();
        // 2 + 2 * 63 at the most.
        self.put(&[(2 + 2 * units) as u8, 3]);
        for unit in text.encode_utf16() {
            self.put(&unit.to_le_bytes());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transfer {
    Bulk,
    Interrupt {
        max_packet: u8,
        interval_ms: u8,
    },
    /// `interval_log2` is the interval in ms as a power of two.
    Isochronous {
        bytes: u16,
        interval_log2: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Endpoint {
    address: u8,
    transfer: Transfer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Interface<'a> {
    /// The interface's number, and the setting's.
    number: u8,
    alternate: u8,
    class: u8,
    subclass: u8,
    protocol: u8,
    name: Option<&'a str>,
    /// Where its endpoints start in the endpoint list, and how many there are.
    first: usize,
    endpoints: usize,
}

/// A function as its line lists it. `derive` takes its addresses from the board identity, which
/// [`functions`] is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Listed {
    Fixed(GadgetFunction),
    Derive { line: usize },
}

impl Listed {
    /// Which function it is, for the rule that each is listed once: an NCM with either address
    /// form counts as the NCM.
    fn kind(&self) -> u8 {
        match self {
            Self::Fixed(GadgetFunction::Ffs) => 0,
            Self::Fixed(GadgetFunction::Acm) => 1,
            Self::Fixed(GadgetFunction::Ncm { .. }) | Self::Derive { .. } => 2,
        }
    }
}

/// The lines of an `interfaces` text, read.
struct Layout<'a> {
    interfaces: [Option<Interface<'a>>; MAX_INTERFACES],
    interface_count: usize,
    endpoints: [Option<Endpoint>; MAX_ENDPOINTS],
    endpoint_count: usize,
    functions: [Option<Listed>; MAX_FUNCTIONS],
    function_count: usize,
}

/// The functions of a gadget's configuration, in the order its `function` lines list them.
/// Without a `function ffs` line the FunctionFS function comes first, as the one every gadget
/// has. Every NCM address is concrete: a `derive` line has its addresses from the identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Functions {
    items: [GadgetFunction; MAX_FUNCTIONS],
    len: usize,
}

impl Functions {
    pub fn as_slice(&self) -> &[GadgetFunction] {
        &self.items[..self.len]
    }
}

/// A MAC address from `aa:bb:cc:dd:ee:ff`, unicast (the low bit of the first octet clear).
fn mac(word: &str) -> Option<[u8; 6]> {
    let mut octets = [0u8; 6];
    let mut parts = word.split(':');
    for octet in &mut octets {
        let part = parts.next()?;
        if part.len() != 2 {
            return None;
        }
        *octet = u8::from_str_radix(part, 16).ok()?;
    }
    (parts.next().is_none() && octets[0] & 1 == 0).then_some(octets)
}

fn number(word: Option<&str>) -> Option<u32> {
    let word = word?;
    match word.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => word.parse().ok(),
    }
}

fn byte(word: Option<&str>) -> Option<u8> {
    u8::try_from(number(word)?).ok()
}

fn name_fits(name: &str) -> bool {
    name.len() <= MAX_NAME && !name.contains('\0')
}

impl<'a> Layout<'a> {
    fn parse(text: &'a str) -> Result<Self, Fault> {
        let mut layout = Self {
            interfaces: [None; MAX_INTERFACES],
            interface_count: 0,
            endpoints: [None; MAX_ENDPOINTS],
            endpoint_count: 0,
            functions: [None; MAX_FUNCTIONS],
            function_count: 0,
        };
        for (at, line) in text.lines().enumerate() {
            let line_number = at + 1;
            let line = line.split('#').next().unwrap_or("").trim();
            let (key, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            match key {
                "" => {}
                "interface" => layout.interface(rest.trim(), line_number)?,
                "alternate" => layout.alternate(rest.trim(), line_number)?,
                "endpoint" => layout.endpoint(rest, line_number)?,
                "function" => layout.function(rest.trim(), line_number)?,
                _ => return Err(Fault::Line(line_number)),
            }
        }
        if layout.interface_count == 0 {
            return Err(Fault::Empty);
        }
        Ok(layout)
    }

    fn interface(&mut self, rest: &'a str, line: usize) -> Result<(), Fault> {
        let bad = Fault::Line(line);
        // Three numbers, then whatever is left is the name.
        let mut rest = rest;
        let mut numbers = [0u8; 3];
        for slot in &mut numbers {
            let (word, after) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            *slot = byte(Some(word)).ok_or(bad)?;
            rest = after.trim_start();
        }
        let name = (!rest.is_empty()).then_some(rest);
        if name.is_some_and(|name| !name_fits(name)) {
            return Err(Fault::Name);
        }
        let number = self.interfaces().filter(|(_, i)| i.alternate == 0).count();
        self.add(Interface {
            // At most `MAX_INTERFACES`, which fits.
            number: number as u8,
            alternate: 0,
            class: numbers[0],
            subclass: numbers[1],
            protocol: numbers[2],
            name,
            first: self.endpoint_count,
            endpoints: 0,
        })
    }

    fn alternate(&mut self, rest: &'a str, line: usize) -> Result<(), Fault> {
        let name = (!rest.is_empty()).then_some(rest);
        if name.is_some_and(|name| !name_fits(name)) {
            return Err(Fault::Name);
        }
        let above = self.interface_count.checked_sub(1);
        let above = above.and_then(|at| self.interfaces[at]);
        let above = above.ok_or(Fault::NoInterface(line))?;
        self.add(Interface {
            alternate: above.alternate + 1,
            name,
            first: self.endpoint_count,
            endpoints: 0,
            ..above
        })
    }

    fn add(&mut self, interface: Interface<'a>) -> Result<(), Fault> {
        let slot = self
            .interfaces
            .get_mut(self.interface_count)
            .ok_or(Fault::TooManyInterfaces)?;
        *slot = Some(interface);
        self.interface_count += 1;
        Ok(())
    }

    fn endpoint(&mut self, rest: &str, line: usize) -> Result<(), Fault> {
        let bad = Fault::Line(line);
        let mut words = rest.split_whitespace();
        let direction = match words.next() {
            Some("in") => 0x80,
            Some("out") => 0,
            _ => return Err(bad),
        };
        let transfer = match words.next() {
            Some("bulk") => Transfer::Bulk,
            Some("interrupt") => {
                let max_packet = byte(words.next()).filter(|n| (1..=64).contains(n));
                let interval_ms = byte(words.next()).filter(|n| *n != 0);
                Transfer::Interrupt {
                    max_packet: max_packet.ok_or(bad)?,
                    interval_ms: interval_ms.ok_or(bad)?,
                }
            }
            Some("isochronous") => {
                let bytes =
                    number(words.next()).filter(|n| (1..=u32::from(MAX_ISO_BYTES)).contains(n));
                let interval = number(words.next())
                    .filter(|n| n.is_power_of_two() && *n <= u32::from(MAX_ISO_INTERVAL_MS));
                Transfer::Isochronous {
                    // Both checked to fit above.
                    bytes: bytes.ok_or(bad)? as u16,
                    interval_log2: interval.ok_or(bad)?.ilog2() as u8,
                }
            }
            _ => return Err(bad),
        };
        if words.next().is_some() {
            return Err(bad);
        }
        let owner = self.interface_count.checked_sub(1);
        let owner = owner.and_then(|at| self.interfaces[at].as_mut());
        let owner = owner.ok_or(Fault::NoInterface(line))?;
        let slot = self
            .endpoints
            .get_mut(self.endpoint_count)
            .ok_or(Fault::TooManyEndpoints)?;
        self.endpoint_count += 1;
        // At most 15, so the number fits the low four bits.
        *slot = Some(Endpoint {
            address: direction | self.endpoint_count as u8,
            transfer,
        });
        owner.endpoints += 1;
        Ok(())
    }

    fn function(&mut self, rest: &str, line: usize) -> Result<(), Fault> {
        let bad = Fault::Function(line);
        let (kind, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let mut words = args.split_whitespace();
        let function = match kind {
            "ffs" => GadgetFunction::Ffs,
            "acm" => GadgetFunction::Acm,
            "ncm" => {
                // `derive` alone: the addresses come from the board identity.
                if args.trim() == "derive" {
                    return self.push(Listed::Derive { line }, line);
                }
                // `host=MAC device=MAC`, in either order, each once.
                let (mut host, mut device) = (None, None);
                for word in words.by_ref() {
                    match word.split_once('=') {
                        Some(("host", value)) if host.is_none() => host = mac(value),
                        Some(("device", value)) if device.is_none() => device = mac(value),
                        _ => return Err(bad),
                    }
                }
                let (host, device) = (host.ok_or(bad)?, device.ok_or(bad)?);
                if host == device {
                    return Err(bad);
                }
                return self.push(Listed::Fixed(GadgetFunction::Ncm { host, device }), line);
            }
            _ => return Err(bad),
        };
        if words.next().is_some() {
            return Err(bad);
        }
        self.push(Listed::Fixed(function), line)
    }

    fn push(&mut self, function: Listed, line: usize) -> Result<(), Fault> {
        let bad = Fault::Function(line);
        let listed = self.listed().any(|f| f.kind() == function.kind());
        if listed {
            return Err(bad);
        }
        let slot = self.functions.get_mut(self.function_count).ok_or(bad)?;
        *slot = Some(function);
        self.function_count += 1;
        Ok(())
    }

    fn listed(&self) -> impl Iterator<Item = Listed> + '_ {
        self.functions.iter().flatten().copied()
    }

    fn interfaces(&self) -> impl Iterator<Item = (usize, Interface<'a>)> + '_ {
        self.interfaces.iter().flatten().copied().enumerate()
    }
}

/// `bInterval` at high speed for an interrupt endpoint polled every `ms`: the interval is
/// 2^(n-1) microframes of 125 us, so 1 ms is 4 and each doubling adds one.
fn high_speed_interval(ms: u8) -> u8 {
    4 + ms.ilog2() as u8
}

/// The functions `interfaces` lists, in order. FunctionFS is first unless a `function ffs` line
/// places it. `macs` are the addresses a `function ncm derive` line takes, from
/// [`crate::derive::macs`]; without them such a line is a [`Fault::NoIdentity`].
pub fn functions(interfaces: &str, macs: Option<Macs>) -> Result<Functions, Fault> {
    let layout = Layout::parse(interfaces)?;
    let mut items = [GadgetFunction::Ffs; MAX_FUNCTIONS];
    let mut len = 0;
    if !layout
        .listed()
        .any(|f| f == Listed::Fixed(GadgetFunction::Ffs))
    {
        items[0] = GadgetFunction::Ffs;
        len = 1;
    }
    for function in layout.functions.iter().flatten() {
        items[len] = match *function {
            Listed::Fixed(function) => function,
            Listed::Derive { line } => {
                let macs = macs.ok_or(Fault::NoIdentity(line))?;
                GadgetFunction::Ncm {
                    host: macs.host,
                    device: macs.device,
                }
            }
        };
        len += 1;
    }
    Ok(Functions { items, len })
}

/// The set for `identity` with the interfaces `interfaces` describes.
pub fn build(identity: &Identity<'_>, interfaces: &str) -> Result<Set, Fault> {
    let layout = Layout::parse(interfaces)?;
    let strings = [
        identity.manufacturer,
        identity.product_name,
        identity.serial,
    ];
    if strings.iter().flatten().any(|text| !name_fits(text)) {
        return Err(Fault::Name);
    }
    if identity.max_power_ma > 500 {
        return Err(Fault::Power);
    }

    // String indexes, in the order the strings are written at the end: the identity's, then
    // the interfaces' names.
    let mut next_string = 0u8;
    let mut index = |present: bool| {
        if present {
            next_string += 1;
            next_string
        } else {
            0
        }
    };
    let identity_strings = strings.map(|text| index(text.is_some()));
    let mut interface_strings = [0u8; MAX_INTERFACES];
    for (at, interface) in layout.interfaces() {
        interface_strings[at] = index(interface.name.is_some());
    }
    let any_string = next_string != 0;

    let mut out = Writer {
        bytes: [0; MAX_SET],
        len: 0,
        full: false,
    };
    let [vendor_low, vendor_high] = identity.vendor.to_le_bytes();
    let [product_low, product_high] = identity.product.to_le_bytes();
    let [release_low, release_high] = identity.release.to_le_bytes();
    out.put(&[
        18,
        1,
        0x00,
        0x02,
        identity.class,
        identity.subclass,
        identity.protocol,
        64,
        vendor_low,
        vendor_high,
        product_low,
        product_high,
        release_low,
        release_high,
        identity_strings[0],
        identity_strings[1],
        identity_strings[2],
        1,
    ]);

    for high_speed in [false, true] {
        let start = out.len;
        let interfaces = layout
            .interfaces()
            .filter(|(_, i)| i.alternate == 0)
            .count();
        // Bit 7 is always set; bit 6 says the device has its own supply.
        let attributes = 0x80 | (u8::from(identity.self_powered) << 6);
        // `bMaxPower` counts 2 mA, rounded up. At most 250.
        let max_power = identity.max_power_ma.div_ceil(2) as u8;
        out.put(&[9, 2, 0, 0, interfaces as u8, 1, 0, attributes, max_power]);
        for (at, interface) in layout.interfaces() {
            out.put(&[
                9,
                4,
                interface.number,
                interface.alternate,
                interface.endpoints as u8,
                interface.class,
                interface.subclass,
                interface.protocol,
                interface_strings[at],
            ]);
            let endpoints = &layout.endpoints[interface.first..][..interface.endpoints];
            for endpoint in endpoints.iter().flatten() {
                let (kind, max_packet, interval) = match endpoint.transfer {
                    Transfer::Bulk if high_speed => (2, 512u16, 0),
                    Transfer::Bulk => (2, 64, 0),
                    Transfer::Interrupt {
                        max_packet,
                        interval_ms,
                    } => {
                        let interval = if high_speed {
                            high_speed_interval(interval_ms)
                        } else {
                            interval_ms
                        };
                        (3, u16::from(max_packet), interval)
                    }
                    // Asynchronous: bits 2 and 3 are 1. `bInterval` is 2^(n-1) frames at full
                    // speed and 2^(n-1) microframes of 125 us at high speed.
                    Transfer::Isochronous {
                        bytes,
                        interval_log2,
                    } => {
                        let interval = interval_log2 + if high_speed { 4 } else { 1 };
                        (0x05, bytes, interval)
                    }
                };
                let [low, high] = max_packet.to_le_bytes();
                out.put(&[7, 5, endpoint.address, kind, low, high, interval]);
            }
        }
        // `wTotalLength`: at most `MAX_SET`, which fits.
        let total = ((out.len - start) as u16).to_le_bytes();
        if let Some(field) = out.bytes.get_mut(start + 2..start + 4) {
            field.copy_from_slice(&total);
        }
    }

    if any_string {
        let [low, high] = LANGUAGE.to_le_bytes();
        out.put(&[4, 3, low, high]);
        for text in strings.iter().flatten() {
            out.string(text);
        }
        for (_, interface) in layout.interfaces() {
            if let Some(name) = interface.name {
                out.string(name);
            }
        }
    }

    if out.full {
        return Err(Fault::TooLong);
    }
    let set = Set {
        bytes: out.bytes,
        len: out.len,
    };
    // What goes to the backend is what its own check takes. A set this builds always is.
    debug_assert!(UsbDescriptors::parse(set.bytes()).is_ok());
    Ok(set)
}
