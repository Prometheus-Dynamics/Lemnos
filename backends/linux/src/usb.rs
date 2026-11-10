//! USB devices by the names a board file uses, opened through usbfs.
//!
//! A name says where the device is plugged in or what it calls itself, never what it is:
//!
//! - `usb:1-1.4`: the port, as sysfs names it (bus 1, port 1 of the root hub, port 4 of the hub
//!   there). This is the same device for as long as the cabling is.
//! - `usb:1a2b:3c4d`: the vendor and product numbers, four hex digits each. This must match
//!   exactly one plugged-in device. With two alike, name each by its port.
//!
//! The name is looked up each time the device is asked for, so a device that re-enumerated (a
//! new address on the same port, or a new vendor and product after a firmware load) is found by
//! the name that fits what it is now.

use std::fs;
use std::path::Path;
use std::time::Duration;

use lemnos_core::{Result, Status, status_of};
use lemnos_hal::blocks::{ControlSetup, IsoPacket, UsbDeviceId, UsbDeviceInfo, UsbSpeed};
use lemnos_hal::{ErrorKind, UsbDevice, UsbHost};
use lemnos_usb::{UsbfsDevice, UsbfsHost};

use crate::Claim;

const PREFIX: &str = "usb:";

/// What a board file's USB name says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsbName<'a> {
    /// A sysfs device name such as `1-1.4`.
    Port(&'a str),
    Id {
        vendor: u16,
        product: u16,
    },
}

impl<'a> UsbName<'a> {
    /// The name, if it is one of ours and well formed. Nothing but digits, `-` and `.` gets into
    /// a path.
    pub(crate) fn parse(need: &'a str) -> Option<Self> {
        let rest = need.strip_prefix(PREFIX)?;
        if let Some((vendor, product)) = rest.split_once(':') {
            return Some(Self::Id {
                vendor: hex4(vendor)?,
                product: hex4(product)?,
            });
        }
        let (bus, ports) = rest.split_once('-')?;
        let number = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        (number(bus) && ports.split('.').all(number)).then_some(Self::Port(rest))
    }
}

fn hex4(text: &str) -> Option<u16> {
    let valid = text.len() == 4 && text.bytes().all(|b| b.is_ascii_hexdigit());
    valid.then(|| u16::from_str_radix(text, 16).ok())?
}

/// Where the named device is right now. `NO_DEVICE` when nothing fits the name, and
/// `INVALID_ARGUMENT` when a vendor and product fit more than one device.
pub(crate) fn locate(dev: &Path, sys: &Path, name: UsbName<'_>) -> Result<UsbDeviceId> {
    let devices = sys.join("bus/usb/devices");
    match name {
        UsbName::Port(port) => {
            let number = |attribute: &str| {
                let text = fs::read_to_string(devices.join(port).join(attribute)).ok()?;
                text.trim().parse::<u8>().ok().filter(|n| *n != 0)
            };
            match (number("busnum"), number("devnum")) {
                (Some(bus), Some(address)) => Ok(UsbDeviceId { bus, address }),
                _ => Err(Status::NO_DEVICE),
            }
        }
        UsbName::Id { vendor, product } => {
            let mut found = None;
            for info in plugged_in(dev, sys)? {
                if (info.vendor, info.product) == (vendor, product) {
                    if found.is_some() {
                        return Err(Status::INVALID_ARGUMENT);
                    }
                    found = Some(info.id);
                }
            }
            found.ok_or(Status::NO_DEVICE)
        }
    }
}

/// Every device plugged in. This runs when a device binds, not while it works.
fn plugged_in(dev: &Path, sys: &Path) -> Result<Vec<UsbDeviceInfo>> {
    const NONE: UsbDeviceInfo = UsbDeviceInfo {
        id: UsbDeviceId { bus: 0, address: 0 },
        vendor: 0,
        product: 0,
        class: 0,
        speed: UsbSpeed::Unknown,
    };
    let mut host = host(dev, sys);
    let mut devices = vec![NONE; 32];
    loop {
        let total = host.enumerate(&mut devices).map_err(status_of)?;
        if total <= devices.len() {
            devices.truncate(total);
            return Ok(devices);
        }
        // More arrived than fit: ask again with room for them.
        devices.resize(total, NONE);
    }
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
