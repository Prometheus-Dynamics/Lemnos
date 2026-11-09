//! Listing devices from sysfs and opening their nodes.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use lemnos_hal::ErrorKind;
use lemnos_hal::UsbHost;
use lemnos_hal::blocks::{UsbDeviceId, UsbDeviceInfo, UsbSpeed};
use lemnos_linux_sys::usbfs::UsbfsFile;

use crate::UsbfsDevice;
use crate::error::kind_of;

/// Longest sysfs attribute this reads.
const ATTR_MAX: usize = 32;

/// This machine's USB host, as sysfs and usbfs show it.
#[derive(Debug, Clone)]
pub struct UsbfsHost {
    sys_devices: PathBuf,
    dev_usb: PathBuf,
}

impl Default for UsbfsHost {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbfsHost {
    /// The system's roots: `/sys/bus/usb/devices` and `/dev/bus/usb`.
    pub fn new() -> Self {
        Self::at(Path::new("/sys/bus/usb/devices"), Path::new("/dev/bus/usb"))
    }

    /// A host whose devices are listed under `sys_devices` and opened under `dev_usb`.
    pub fn at(sys_devices: &Path, dev_usb: &Path) -> Self {
        Self {
            sys_devices: sys_devices.to_path_buf(),
            dev_usb: dev_usb.to_path_buf(),
        }
    }
}

impl UsbHost for UsbfsHost {
    type Error = ErrorKind;
    type Device = UsbfsDevice;

    /// Fills `out` with the devices in order of bus and address. Root hubs are left out, since
    /// they aren't plugged in, and so is an entry that can't be read or parsed, which is what a
    /// device leaving during the listing looks like.
    fn enumerate(&mut self, out: &mut [UsbDeviceInfo]) -> Result<usize, ErrorKind> {
        let entries = match fs::read_dir(&self.sys_devices) {
            Ok(entries) => entries,
            // No USB in this kernel, so nothing plugged in.
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(kind_of(&error)),
        };
        let mut total = 0;
        let mut filled = 0;
        for entry in entries {
            let entry = entry.map_err(|error| kind_of(&error))?;
            let name = entry.file_name();
            if name.as_encoded_bytes().contains(&b':')
                || name.as_encoded_bytes().starts_with(b"usb")
            {
                continue;
            }
            if let Some(info) = read_device(&entry.path()) {
                total += 1;
                filled = insert_sorted(out, filled, info);
            }
        }
        Ok(total)
    }

    fn open(&mut self, id: UsbDeviceId) -> Result<UsbfsDevice, ErrorKind> {
        if id.bus == 0 || id.address == 0 {
            return Err(ErrorKind::NotFound);
        }
        let node = self
            .dev_usb
            .join(format!("{:03}/{:03}", id.bus, id.address));
        let file = UsbfsFile::open(&node).map_err(|error| kind_of(&error))?;
        Ok(UsbfsDevice::from_file(file))
    }
}

/// Keeps `out[..filled]` sorted, dropping whatever sorts after a full buffer's end. Returns the
/// new `filled`.
fn insert_sorted(out: &mut [UsbDeviceInfo], filled: usize, info: UsbDeviceInfo) -> usize {
    let key = |device: &UsbDeviceInfo| (device.id.bus, device.id.address);
    let position = out[..filled].partition_point(|device| key(device) < key(&info));
    if position == out.len() {
        return filled;
    }
    let end = filled.min(out.len() - 1);
    out.copy_within(position..end, position + 1);
    out[position] = info;
    (filled + 1).min(out.len())
}

/// One device's attributes. `None` when any is missing or out of range.
fn read_device(dir: &Path) -> Option<UsbDeviceInfo> {
    let bus = u8::try_from(decimal(dir, "busnum")?).ok()?;
    let address = u8::try_from(decimal(dir, "devnum")?).ok()?;
    if bus == 0 || address == 0 {
        return None;
    }
    Some(UsbDeviceInfo {
        id: UsbDeviceId { bus, address },
        vendor: u16::try_from(hex(dir, "idVendor")?).ok()?,
        product: u16::try_from(hex(dir, "idProduct")?).ok()?,
        class: u8::try_from(hex(dir, "bDeviceClass")?).ok()?,
        speed: speed(dir),
    })
}

/// The kernel prints the speed in Mbit/s.
fn speed(dir: &Path) -> UsbSpeed {
    let mut buffer = [0u8; ATTR_MAX];
    match attribute(dir, "speed", &mut buffer) {
        Some("1.5") => UsbSpeed::Low,
        Some("12") => UsbSpeed::Full,
        Some("480") => UsbSpeed::High,
        Some("5000") => UsbSpeed::Super,
        Some("10000" | "20000") => UsbSpeed::SuperPlus,
        _ => UsbSpeed::Unknown,
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
