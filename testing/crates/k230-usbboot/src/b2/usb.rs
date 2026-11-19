//! The board's side of [`super::Board`]: the bus read from sysfs (opens nothing), the access decided by
//! `access(2)` on the node (opens nothing; the kernel's own check, ACLs included), and the open and claim
//! through libusb, made only when [`super::run`] gets that far.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use rusb::{Context, DeviceHandle, UsbContext};

use super::{Access, Board, Link, LinkError, Seen};
use crate::bootrom::{DOWNLOAD_PID, DOWNLOAD_VID};
use lemnos_flash_wire::usb::{BULK_IN, BULK_OUT, PRODUCT_ID, VENDOR_ID};

/// The bus as sysfs states it.
///
/// The scan must not block: while the kernel enumerates a device on a hub's port it holds that hub's
/// device lock, and the string and configuration attributes take the device lock before they answer
/// (Linux `drivers/usb/core/sysfs.c` at the k230_sdk's 5.10 tree: `usb_string_attr` :123-140 for
/// `product`, `usb_actconfig_show` :24-45,90 for `bConfigurationValue`). B2 run 2's 15 s bound ended at
/// 30.7 s on such reads. So every device is scanned by the lock-free attributes only (`idVendor`,
/// `idProduct` :709-723, `busnum`, `devnum` :199-215), and the locking ones are read only for a device
/// with the stage's or the boot ROM's ids, each on its own thread with a bound ([`ATTR_BOUND`]): a read
/// that does not answer in time leaves that field unknown for this scan and is picked up by a later one.
pub struct Sysfs {
    root: PathBuf,
    /// Reads still running, by path and the device number they were started for.
    pending: HashMap<(PathBuf, u32), Receiver<Option<String>>>,
    /// udev's database (`/run/udev/data`): a device it has finished has an entry `c<major>:<minor>`.
    udev: PathBuf,
}

/// How long one locking attribute read may take before the scan goes on without it.
pub const ATTR_BOUND: Duration = Duration::from_millis(100);

impl Default for Sysfs {
    fn default() -> Self {
        Self::at("/sys/bus/usb/devices")
    }
}

fn read(dir: &Path, name: &str) -> Option<String> {
    read_path(&dir.join(name))
}

fn read_path(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

impl Sysfs {
    /// The bus under `root` (a test's copy of the tree, or the real one).
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            pending: HashMap::new(),
            udev: PathBuf::from("/run/udev/data"),
        }
    }

    /// The same, with udev's database under `dir` (a test's copy).
    pub fn with_udev(mut self, dir: impl Into<PathBuf>) -> Self {
        self.udev = dir.into();
        self
    }

    /// One locking attribute of device `devnum`, read on a thread for at most [`ATTR_BOUND`]. A read
    /// still running from an earlier scan of the same device is waited on again, never doubled; one
    /// started for an earlier device at the same path is dropped.
    fn bounded(&mut self, path: PathBuf, devnum: u32) -> Option<String> {
        self.pending.retain(|(p, n), _| *p != path || *n == devnum);
        let key = (path, devnum);
        let rx = match self.pending.remove(&key) {
            Some(rx) => rx,
            None => {
                let (tx, rx) = mpsc::channel();
                let at = key.0.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(read_path(&at));
                });
                rx
            }
        };
        match rx.recv_timeout(ATTR_BOUND) {
            Ok(value) => value,
            Err(RecvTimeoutError::Timeout) => {
                self.pending.insert(key, rx);
                None
            }
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    /// One device's view, or `None` when `port` is not a device (an interface, a root hub's name).
    /// The locking attributes are read (bounded) only for the stage's or the boot ROM's ids.
    pub fn seen(&mut self, port: &str) -> Option<Seen> {
        let dir = self.root.join(port);
        let vid = u16::from_str_radix(&read(&dir, "idVendor")?, 16).ok()?;
        let pid = u16::from_str_radix(&read(&dir, "idProduct")?, 16).ok()?;
        let mut seen = Seen {
            port: port.to_string(),
            vid,
            pid,
            product: None,
            interface: None,
            speed: None,
            configuration: None,
            busnum: read(&dir, "busnum")?.parse().ok()?,
            devnum: read(&dir, "devnum")?.parse().ok()?,
        };
        let ours =
            (vid, pid) == (VENDOR_ID, PRODUCT_ID) || (vid, pid) == (DOWNLOAD_VID, DOWNLOAD_PID);
        if ours {
            let n = seen.devnum;
            seen.speed = read(&dir, "speed");
            seen.interface = read(&dir.join(format!("{port}:1.0")), "interface");
            seen.product = self.bounded(dir.join("product"), n);
            seen.configuration = self
                .bounded(dir.join("bConfigurationValue"), n)
                .and_then(|v| v.parse().ok());
        }
        Some(seen)
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

    /// The device's `dev` attribute (`189:<minor>`, the driver core's, no device lock) names udev's
    /// database entry for it, `c189:<minor>`, which udev writes when it has finished the device.
    fn udev_done(&mut self, seen: &Seen) -> bool {
        read(&self.root.join(&seen.port), "dev")
            .filter(|dev| dev.starts_with("189:"))
            .is_some_and(|dev| self.udev.join(format!("c{dev}")).exists())
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
