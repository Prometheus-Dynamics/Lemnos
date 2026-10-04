//! Kernel uevents (`NETLINK_KOBJECT_UEVENT`): devices appearing, going away,
//! binding to drivers, without udev. Unprivileged processes may listen.
//! Ported from Styx (`styx-kernel/src/uevent.rs`); `LinuxHotplugWatcher`
//! (feature `hotplug`) is built on it.

use lemnos_linux_sys::{netlink, poll};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::time::Duration;

/// One uevent: `ACTION@DEVPATH` and its `KEY=VALUE` variables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uevent {
    /// `add`, `remove`, `bind`, `unbind`, `change`, `move`, `online`, `offline`.
    pub action: String,
    /// The sysfs path below `/sys` (`/devices/.../usb3/3-1`).
    pub devpath: String,
    /// The variables (`SUBSYSTEM`, `DEVTYPE`, `DEVNAME`, `BUSNUM`, ...).
    pub vars: Vec<(String, String)>,
}

impl Uevent {
    /// Parses a kernel uevent message (`action@devpath\0KEY=VALUE\0...`).
    pub fn parse(msg: &[u8]) -> Option<Uevent> {
        let mut parts = msg.split(|&b| b == 0).filter(|p| !p.is_empty());
        let head = std::str::from_utf8(parts.next()?).ok()?;
        let (action, devpath) = head.split_once('@')?;
        let vars = parts
            .filter_map(|p| {
                let s = std::str::from_utf8(p).ok()?;
                let (k, v) = s.split_once('=')?;
                Some((k.to_owned(), v.to_owned()))
            })
            .collect();
        Some(Uevent {
            action: action.to_owned(),
            devpath: devpath.to_owned(),
            vars,
        })
    }

    /// A variable's value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The `SUBSYSTEM` variable.
    pub fn subsystem(&self) -> Option<&str> {
        self.get("SUBSYSTEM")
    }
}

/// What [`UeventSocket::recv`] returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UeventRecv {
    /// An event.
    Event(Uevent),
    /// The receive queue overflowed: events were lost, rescan.
    Overflow,
}

/// A socket receiving the kernel's uevents. Non-blocking: [`recv`] returns
/// `Ok(None)` when nothing is pending; the descriptor is readable when one is.
///
/// [`recv`]: UeventSocket::recv
#[derive(Debug)]
pub struct UeventSocket {
    fd: OwnedFd,
    buf: Vec<u8>,
}

impl UeventSocket {
    /// Opens and binds the socket to the kernel's uevent group.
    pub fn open() -> io::Result<UeventSocket> {
        Ok(UeventSocket {
            fd: netlink::uevent_socket(netlink::KERNEL_GROUP)?,
            buf: vec![0u8; 16 * 1024],
        })
    }

    /// The next uevent from the kernel, if one is pending (messages from
    /// other senders are skipped).
    pub fn recv(&mut self) -> io::Result<Option<UeventRecv>> {
        loop {
            match netlink::recv(self.fd.as_fd(), &mut self.buf) {
                Ok(None) => return Ok(None),
                // Only the kernel (port 0) sends real uevents on this group.
                Ok(Some((_, sender))) if sender != 0 => continue,
                Ok(Some((n, _))) => {
                    if let Some(ev) = Uevent::parse(&self.buf[..n]) {
                        return Ok(Some(UeventRecv::Event(ev)));
                    }
                }
                Err(e) if netlink::is_overflow(&e) => return Ok(Some(UeventRecv::Overflow)),
                Err(e) => return Err(e),
            }
        }
    }

    /// Waits until a uevent is pending or `timeout` passes; returns whether
    /// one is.
    pub fn wait(&self, timeout: Option<Duration>) -> io::Result<bool> {
        let ready = poll::poll_one(self.fd.as_fd(), poll::POLLIN, timeout)?;
        Ok(ready & poll::POLLIN != 0)
    }
}

impl AsFd for UeventSocket {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl AsRawFd for UeventSocket {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_usb_add() {
        let msg = b"add@/devices/platform/xhci-hcd.1/usb3/3-1\0ACTION=add\0DEVPATH=/devices/platform/xhci-hcd.1/usb3/3-1\0SUBSYSTEM=usb\0DEVNAME=bus/usb/003/002\0DEVTYPE=usb_device\0PRODUCT=46d/825/12\0BUSNUM=003\0DEVNUM=002\0SEQNUM=1234\0";
        let ev = Uevent::parse(msg).unwrap();
        assert_eq!(ev.action, "add");
        assert_eq!(ev.devpath, "/devices/platform/xhci-hcd.1/usb3/3-1");
        assert_eq!(ev.get("DEVTYPE"), Some("usb_device"));
        assert_eq!(ev.subsystem(), Some("usb"));
        assert_eq!(ev.get("BUSNUM"), Some("003"));
        assert_eq!(ev.get("MISSING"), None);
        assert!(Uevent::parse(b"libudev\0junk").is_none());
    }

    #[test]
    fn opens_where_netlink_is_allowed() {
        let Ok(mut sock) = UeventSocket::open() else {
            return;
        };
        assert!(sock.recv().is_ok());
        assert!(!sock.wait(Some(Duration::ZERO)).unwrap() || sock.recv().is_ok());
    }
}
