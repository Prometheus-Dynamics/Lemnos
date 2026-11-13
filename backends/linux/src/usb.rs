//! USB devices by the names a board file uses, opened through usbfs.
//!
//! A name says where the device is plugged in or what it says it is, never what Lemnos makes of
//! it:
//!
//! - `usb:1-1.4`: the port, as sysfs names it (bus 1, port 1 of the root hub, port 4 of the hub
//!   there). This is the same device for as long as the cabling is.
//! - `usb:1a2b:3c4d`: the vendor and product numbers, four hex digits each.
//! - `usb:class:0e`, `usb:class:0e:01`, `usb:class:0e:01:00`: the class, subclass and protocol of
//!   the device or of one of its interfaces, two hex digits each (see
//!   [`lemnos_hal::usb_match`]).
//!
//! A name of the last two kinds must fit exactly one plugged-in device when a device is opened by
//! it. With two alike, name each by its port, or let an `[[each]]` entry of the board file bind
//! one device for each: [`find`] reports every device that fits by its port.
//!
//! The name is looked up each time the device is asked for, so a device that re-enumerated (a
//! new address on the same port, or a new vendor and product after a firmware load) is found by
//! the name that fits what it is now. Everything here reads sysfs; nothing opens a device but
//! [`open`].

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

use lemnos_core::{Result, Status, status_of};
use lemnos_hal::blocks::{ControlSetup, IsoPacket, UsbDeviceId};
use lemnos_hal::usb_match::{ClassCodes, UsbCodes, UsbPattern};
use lemnos_hal::{ErrorKind, UsbDevice, UsbHost};
use lemnos_usb::{UsbfsDevice, UsbfsHost};

use crate::Claim;

const PREFIX: &str = "usb:";

/// Longest sysfs attribute this reads.
const ATTR_MAX: usize = 32;

/// What a board file's USB name says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsbName<'a> {
    /// A sysfs device name such as `1-1.4`.
    Port(&'a str),
    /// What the device says it is.
    Is(UsbPattern),
}

impl<'a> UsbName<'a> {
    /// The name, if it is one of ours and well formed. Nothing but digits, `-` and `.` gets into
    /// a path.
    pub(crate) fn parse(need: &'a str) -> Option<Self> {
        if let Some(pattern) = UsbPattern::parse(need) {
            return Some(Self::Is(pattern));
        }
        let rest = need.strip_prefix(PREFIX)?;
        let (bus, ports) = rest.split_once('-')?;
        let number = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        (number(bus) && ports.split('.').all(number)).then_some(Self::Port(rest))
    }
}

/// The port of a sysfs entry that is a whole device: not an interface, not a root hub.
fn device_port(entry: &str) -> Option<&str> {
    match UsbName::parse(&format!("{PREFIX}{entry}")) {
        Some(UsbName::Port(_)) => Some(entry),
        _ => None,
    }
}

/// The port of every whole device plugged in, in order.
fn ports(sys: &Path) -> Result<Vec<String>> {
    let devices = sys.join("bus/usb/devices");
    let entries = match fs::read_dir(&devices) {
        Ok(entries) => entries,
        // No USB in this kernel, so nothing plugged in.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(status_of(ErrorKind::from_io(error.kind()))),
    };
    let mut ports: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| device_port(name).is_some())
        .collect();
    ports.sort();
    Ok(ports)
}

/// What the device on `port` says it is: its device descriptor's codes, and the class codes of
/// each interface of its configuration. `None` when it isn't there or can't be read, which is
/// what a device leaving looks like.
fn codes(sys: &Path, port: &str) -> Option<(UsbCodes, Vec<ClassCodes>)> {
    let devices = sys.join("bus/usb/devices");
    let dir = devices.join(port);
    let byte = |dir: &Path, name: &str| u8::try_from(hex(dir, name)?).ok();
    let codes = UsbCodes {
        vendor: u16::try_from(hex(&dir, "idVendor")?).ok()?,
        product: u16::try_from(hex(&dir, "idProduct")?).ok()?,
        class: byte(&dir, "bDeviceClass")?,
        subclass: byte(&dir, "bDeviceSubClass")?,
        protocol: byte(&dir, "bDeviceProtocol")?,
    };
    // Interfaces are listed beside it as `PORT:CONFIGURATION.INTERFACE`.
    let mut interfaces = Vec::new();
    for entry in fs::read_dir(&devices).ok()?.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|name| name.strip_prefix(port)) else {
            continue;
        };
        if !rest.starts_with(':') {
            continue;
        }
        let dir = devices.join(&name);
        if let (Some(class), Some(subclass), Some(protocol)) = (
            byte(&dir, "bInterfaceClass"),
            byte(&dir, "bInterfaceSubClass"),
            byte(&dir, "bInterfaceProtocol"),
        ) {
            interfaces.push([class, subclass, protocol]);
        }
    }
    Some((codes, interfaces))
}

/// Whether the device on `port` fits `pattern`.
fn port_fits(sys: &Path, port: &str, pattern: &UsbPattern) -> bool {
    codes(sys, port).is_some_and(|(codes, interfaces)| pattern.fits(&codes, interfaces))
}

/// Where the named device is right now. `NO_DEVICE` when nothing fits the name, and
/// `INVALID_ARGUMENT` when a name that says what a device is fits more than one.
pub(crate) fn locate(sys: &Path, name: UsbName<'_>) -> Result<UsbDeviceId> {
    let port = match name {
        UsbName::Port(port) => port.to_string(),
        UsbName::Is(pattern) => {
            let mut fitting = ports(sys)?
                .into_iter()
                .filter(|port| port_fits(sys, port, &pattern));
            let port = fitting.next().ok_or(Status::NO_DEVICE)?;
            if fitting.next().is_some() {
                return Err(Status::INVALID_ARGUMENT);
            }
            port
        }
    };
    let dir = sys.join("bus/usb/devices").join(port);
    let number = |attribute: &str| {
        u8::try_from(decimal(&dir, attribute)?)
            .ok()
            .filter(|n| *n != 0)
    };
    match (number("busnum"), number("devnum")) {
        (Some(bus), Some(address)) => Ok(UsbDeviceId { bus, address }),
        _ => Err(Status::NO_DEVICE),
    }
}

/// Reports, by its port (`usb:1-1.4`), each device plugged in now that `pattern` reaches: the
/// device on that port for a port, every device that fits for a name that says what a device
/// is. `NOT_SUPPORTED` for a name that isn't a USB name.
pub(crate) fn find(sys: &Path, pattern: &str, found: &mut dyn FnMut(&str)) -> Result<()> {
    let name = UsbName::parse(pattern).ok_or(Status::NOT_SUPPORTED)?;
    for port in ports(sys)? {
        let fits = match name {
            UsbName::Port(named) => named == port,
            UsbName::Is(pattern) => port_fits(sys, &port, &pattern),
        };
        if fits {
            found(&format!("{PREFIX}{port}"));
        }
    }
    Ok(())
}

/// Whether the board name `name` reaches the device `found` names by its port, now.
pub(crate) fn fits(sys: &Path, name: &str, found: &str) -> bool {
    let Some(UsbName::Port(port)) = UsbName::parse(found) else {
        return false;
    };
    match UsbName::parse(name) {
        Some(UsbName::Port(named)) => named == port,
        Some(UsbName::Is(pattern)) => port_fits(sys, port, &pattern),
        None => false,
    }
}

fn decimal(dir: &Path, name: &str) -> Option<u32> {
    let mut buffer = [0u8; ATTR_MAX];
    attribute(dir, name, &mut buffer)?.parse().ok()
}

fn hex(dir: &Path, name: &str) -> Option<u32> {
    let mut buffer = [0u8; ATTR_MAX];
    u32::from_str_radix(attribute(dir, name, &mut buffer)?, 16).ok()
}

/// The attribute's text, trimmed. Reads at most the buffer, so an outsized file can't cost more.
fn attribute<'a>(dir: &Path, name: &str, buffer: &'a mut [u8; ATTR_MAX]) -> Option<&'a str> {
    let mut file = File::open(dir.join(name)).ok()?;
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    core::str::from_utf8(&buffer[..filled]).ok().map(str::trim)
}

fn host(dev: &Path, sys: &Path) -> UsbfsHost {
    UsbfsHost::at(&sys.join("bus/usb/devices"), &dev.join("bus/usb"))
}

/// The name one owner at a time holds, whichever way the board file named the device.
pub(crate) fn claim_key(id: UsbDeviceId) -> String {
    format!("{PREFIX}{:03}/{:03}", id.bus, id.address)
}

/// Opens the device node of `id`.
pub(crate) fn open(dev: &Path, sys: &Path, id: UsbDeviceId, claim: Claim) -> Result<HeldUsb> {
    let device = host(dev, sys).open(id).map_err(|kind| match kind {
        // The node went between the look and the open: the device left.
        ErrorKind::NotFound => Status::NO_DEVICE,
        kind => status_of(kind),
    })?;
    Ok(HeldUsb {
        device,
        _claim: claim,
    })
}

/// A usbfs device and this process's hold on it. Dropping it closes the device and lets go.
pub(crate) struct HeldUsb {
    device: UsbfsDevice,
    _claim: Claim,
}

macro_rules! forward {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {$(
        fn $name(&mut self, $($arg: $ty),*) -> core::result::Result<$ret, ErrorKind> {
            self.device.$name($($arg),*)
        }
    )*};
}

impl UsbDevice for HeldUsb {
    type Error = ErrorKind;

    forward! {
        config_descriptor(index: u8, out: &mut [u8]) -> usize;
        reset() -> ();
        detach_kernel_driver(interface: u8) -> ();
        claim_interface(interface: u8) -> ();
        release_interface(interface: u8) -> ();
        set_alternate(interface: u8, alternate: u8) -> ();
        control_in(setup: ControlSetup, data: &mut [u8], timeout: Duration) -> usize;
        control_out(setup: ControlSetup, data: &[u8], timeout: Duration) -> usize;
        bulk_in(endpoint: u8, data: &mut [u8], timeout: Duration) -> usize;
        bulk_out(endpoint: u8, data: &[u8], timeout: Duration) -> usize;
        interrupt_in(endpoint: u8, data: &mut [u8], timeout: Duration) -> usize;
        iso_in(endpoint: u8, data: &mut [u8], packets: &mut [IsoPacket], timeout: Duration) -> ();
    }
}
