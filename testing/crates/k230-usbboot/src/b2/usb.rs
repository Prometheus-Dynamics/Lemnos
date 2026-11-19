//! The board's side of [`super::Board`]: the bus read from sysfs (opens nothing), the access decided by
//! `access(2)` on the node (opens nothing; the kernel's own check, ACLs included), and the open and claim
//! through libusb, made only when [`super::run`] gets that far.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusb::{Context, DeviceHandle, UsbContext};

use super::{Access, Board, Link, LinkError, Seen};
use lemnos_flash_wire::usb::{BULK_IN, BULK_OUT};

/// The bus as sysfs states it.
pub struct Sysfs {
    root: PathBuf,
}

impl Default for Sysfs {
    fn default() -> Self {
        Self::at("/sys/bus/usb/devices")
    }
}

fn read(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name))
        .ok()
        .map(|s| s.trim().to_string())
}

impl Sysfs {
    /// The bus under `root` (a test's copy of the tree, or the real one).
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// One device's view, or `None` when `port` is not a device (an interface, a root hub's name).
    pub fn seen(&self, port: &str) -> Option<Seen> {
        let dir = self.root.join(port);
        let vid = u16::from_str_radix(&read(&dir, "idVendor")?, 16).ok()?;
        let pid = u16::from_str_radix(&read(&dir, "idProduct")?, 16).ok()?;
        Some(Seen {
            port: port.to_string(),
            vid,
            pid,
            product: read(&dir, "product"),
            interface: read(&dir.join(format!("{port}:1.0")), "interface"),
            speed: read(&dir, "speed"),
            configuration: read(&dir, "bConfigurationValue").and_then(|v| v.parse().ok()),
            busnum: read(&dir, "busnum")?.parse().ok()?,
            devnum: read(&dir, "devnum")?.parse().ok()?,
        })
    }
}

/// A node's owner, mode and whether it carries an ACL, for the refusal's text. Opens nothing.
fn describe(node: &str) -> String {
    match rustix::fs::stat(node) {
        Ok(st) => {
            let acl =
                rustix::fs::getxattr(node, "system.posix_acl_access", &mut [0u8; 256]).is_ok();
            format!(
                "mode {:o} owner {}:{}{}",
                st.st_mode & 0o7777,
                st.st_uid,
                st.st_gid,
                if acl { " with an ACL" } else { ", no ACL" }
            )
        }
        Err(e) => format!("stat failed: {e}"),
    }
}

impl Board for Sysfs {
    fn scan(&mut self) -> Vec<Seen> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out: Vec<Seen> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| !name.contains(':'))
            .filter_map(|name| self.seen(&name))
            .collect();
        out.sort_by(|a, b| a.port.cmp(&b.port));
        out
    }

    fn access(&mut self, seen: &Seen) -> Access {
        let node = seen.devnode();
        match rustix::fs::access(
            node.as_str(),
            rustix::fs::Access::READ_OK | rustix::fs::Access::WRITE_OK,
        ) {
            Ok(()) => Access::Granted,
            Err(e) => Access::Denied(format!("access(R|W): {e}; {}", describe(&node))),
        }
    }

    fn open(&mut self, seen: &Seen) -> Result<Box<dyn Link>, String> {
        let context = Context::new().map_err(|e| e.to_string())?;
        let devices = context.devices().map_err(|e| e.to_string())?;
        for device in devices.iter() {
            if u32::from(device.bus_number()) != seen.busnum
                || u32::from(device.address()) != seen.devnum
            {
                continue;
            }
            let path = device.port_numbers().unwrap_or_default();
            let port = format!(
                "{}-{}",
                device.bus_number(),
                path.iter().map(u8::to_string).collect::<Vec<_>>().join(".")
            );
            if port != seen.port {
                return Err(format!(
                    "device {}:{} is at {port}, not {}",
                    seen.busnum, seen.devnum, seen.port
                ));
            }
            let d = device.device_descriptor().map_err(|e| e.to_string())?;
            if (d.vendor_id(), d.product_id()) != (seen.vid, seen.pid) {
                return Err("the device's ids changed since the scan".to_string());
            }
            let handle = device.open().map_err(|e| e.to_string())?;
            handle.claim_interface(0).map_err(|e| e.to_string())?;
            return Ok(Box::new(Rusb { handle }));
        }
        Err(format!(
            "no device {}:{} on the bus any more",
            seen.busnum, seen.devnum
        ))
    }
}

struct Rusb {
    handle: DeviceHandle<Context>,
}

fn map(error: rusb::Error) -> LinkError {
    match error {
        rusb::Error::Pipe => LinkError::Stall,
        rusb::Error::Timeout => LinkError::Timeout,
        rusb::Error::NoDevice => LinkError::Gone,
        other => LinkError::Other(other.to_string()),
    }
}

impl Link for Rusb {
    fn control_in(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        len: u16,
        timeout: Duration,
    ) -> Result<Vec<u8>, LinkError> {
        let mut buf = vec![0u8; usize::from(len)];
        let n = self
            .handle
            .read_control(request_type, request, value, index, &mut buf, timeout)
            .map_err(map)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn control_none(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        timeout: Duration,
    ) -> Result<(), LinkError> {
        self.handle
            .write_control(request_type, request, value, index, &[], timeout)
            .map(|_| ())
            .map_err(map)
    }

    fn bulk_out(&mut self, data: &[u8], timeout: Duration) -> Result<(), LinkError> {
        let n = self
            .handle
            .write_bulk(BULK_OUT, data, timeout)
            .map_err(map)?;
        if n != data.len() {
            return Err(LinkError::Other(format!(
                "{n} of {} bytes sent",
                data.len()
            )));
        }
        Ok(())
    }

    fn bulk_in(&mut self, max: usize, timeout: Duration) -> Result<Vec<u8>, LinkError> {
        let mut buf = vec![0u8; max];
        let n = self
            .handle
            .read_bulk(BULK_IN, &mut buf, timeout)
            .map_err(map)?;
        buf.truncate(n);
        Ok(buf)
    }
}

impl Drop for Rusb {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(0);
    }
}
