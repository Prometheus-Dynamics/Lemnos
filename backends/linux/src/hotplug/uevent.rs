//! Kernel uevents (`NETLINK_KOBJECT_UEVENT`): devices appearing, going away,
//! binding to drivers, without udev. Unprivileged processes may listen.
//!
//! [`UeventMsg`] is the zero-copy view of one message; [`UeventSocket::drain`]
//! hands views of the receive buffer to a callback, so a steady stream of
//! events allocates nothing. [`Uevent`] is the owned form. The generic
//! watcher in this module is built on both.

use lemnos_linux_sys::{netlink, poll};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::time::Duration;

/// What a uevent's `ACTION` says happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Add,
    Remove,
    Change,
    Move,
    Bind,
    Unbind,
    Online,
    Offline,
    /// Any other action (the kernel may add new ones).
    Other,
}

impl Action {
    /// The action named by an `ACTION` value (or the part before `@`).
    pub fn parse(action: &str) -> Action {
        match action {
            "add" => Action::Add,
            "remove" => Action::Remove,
            "change" => Action::Change,
            "move" => Action::Move,
            "bind" => Action::Bind,
            "unbind" => Action::Unbind,
            "online" => Action::Online,
            "offline" => Action::Offline,
            _ => Action::Other,
        }
    }

    /// The kernel's spelling of the action (`Other` has none: `"other"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Action::Add => "add",
            Action::Remove => "remove",
            Action::Change => "change",
            Action::Move => "move",
            Action::Bind => "bind",
            Action::Unbind => "unbind",
            Action::Online => "online",
            Action::Offline => "offline",
            Action::Other => "other",
        }
    }
}

/// One uevent, borrowed: `action@devpath\0KEY=VALUE\0...`. Variables are
/// read from the message bytes on demand, so nothing is copied or allocated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UeventMsg<'a> {
    action: &'a str,
    devpath: &'a str,
    vars: &'a [u8],
}

impl<'a> UeventMsg<'a> {
    /// Parses a message (kernel or synthesized). `None` when the header
    /// (`action@devpath`) is missing or not UTF-8.
    pub fn parse(msg: &'a [u8]) -> Option<UeventMsg<'a>> {
        let (head, vars) = match msg.iter().position(|&b| b == 0) {
            Some(end) => (&msg[..end], &msg[end + 1..]),
            None => (msg, &msg[msg.len()..]),
        };
        let head = std::str::from_utf8(head).ok()?;
        let (action, devpath) = head.split_once('@')?;
        Some(UeventMsg {
            action,
            devpath,
            vars,
        })
    }

    /// The raw `ACTION` text.
    pub fn action_str(&self) -> &'a str {
        self.action
    }

    /// The `ACTION` as an [`Action`].
    pub fn action(&self) -> Action {
        Action::parse(self.action)
    }

    /// The sysfs path below `/sys` (`/devices/.../usb3/3-1`). Empty for
    /// events that name only a device node.
    pub fn devpath(&self) -> &'a str {
        self.devpath
    }

    /// The variables, in message order.
    pub fn vars(&self) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
        self.vars
            .split(|&b| b == 0)
            .filter(|part| !part.is_empty())
            .filter_map(|part| {
                let part = std::str::from_utf8(part).ok()?;
                part.split_once('=')
            })
    }

    /// A variable's value (the first with that key).
    pub fn get(&self, key: &str) -> Option<&'a str> {
        self.vars().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    /// `SUBSYSTEM`.
    pub fn subsystem(&self) -> Option<&'a str> {
        self.get("SUBSYSTEM")
    }

    /// `DEVTYPE` (`usb_device`, `usb_interface`, `disk`, ...).
    pub fn devtype(&self) -> Option<&'a str> {
        self.get("DEVTYPE")
    }

    /// `DEVNAME`: the device node below `/dev`, without the `/dev/` prefix.
    pub fn devname(&self) -> Option<&'a str> {
        self.get("DEVNAME")
    }

    /// An owned copy of the message.
    pub fn to_uevent(&self) -> Uevent {
        Uevent {
            action: self.action.to_owned(),
            devpath: self.devpath.to_owned(),
            vars: self
                .vars()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }
    }
}

/// Writes a message in the kernel's format into `out` (replacing its
/// contents). Used to synthesize events the kernel did not send (coldplug,
/// rescans) so every consumer parses them the same way.
pub(crate) fn encode<'v>(
    out: &mut Vec<u8>,
    action: &str,
    devpath: &str,
    vars: impl IntoIterator<Item = (&'v str, &'v str)>,
) {
    out.clear();
    out.extend_from_slice(action.as_bytes());
    out.push(b'@');
    out.extend_from_slice(devpath.as_bytes());
    out.push(0);
    for (key, value) in vars {
        out.extend_from_slice(key.as_bytes());
        out.push(b'=');
        out.extend_from_slice(value.as_bytes());
        out.push(0);
    }
}

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
        UeventMsg::parse(msg).map(|m| m.to_uevent())
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

/// What [`UeventSocket::drain`] hands its callback: a borrowed event, or the
/// news that the queue overflowed and events were lost.
#[derive(Clone, Copy, Debug)]
pub enum UeventRef<'a> {
    /// An event.
    Event(UeventMsg<'a>),
    /// Events were lost: rescan.
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
    /// other senders are skipped). Later events stay queued.
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

    /// Calls `f` for every pending uevent and overflow notice, without
    /// allocating, and returns how many there were (0: nothing pending).
    /// Messages from senders other than the kernel are skipped, and so are
    /// datagrams that do not parse. The views are valid only inside `f`.
    pub fn drain(&mut self, mut f: impl FnMut(UeventRef<'_>)) -> io::Result<usize> {
        let UeventSocket { fd, buf } = self;
        let mut delivered = 0usize;
        loop {
            match netlink::recv(fd.as_fd(), buf) {
                Ok(None) => return Ok(delivered),
                // Only the kernel (port 0) sends real uevents on this group.
                Ok(Some((_, sender))) if sender != 0 => continue,
                Ok(Some((n, _))) => {
                    if let Some(msg) = UeventMsg::parse(&buf[..n]) {
                        delivered += 1;
                        f(UeventRef::Event(msg));
                    }
                }
                Err(e) if netlink::is_overflow(&e) => {
                    delivered += 1;
                    f(UeventRef::Overflow);
                }
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

    const USB_ADD: &[u8] = b"add@/devices/platform/xhci-hcd.1/usb3/3-1\0ACTION=add\0DEVPATH=/devices/platform/xhci-hcd.1/usb3/3-1\0SUBSYSTEM=usb\0DEVNAME=bus/usb/003/002\0DEVTYPE=usb_device\0PRODUCT=46d/825/12\0BUSNUM=003\0DEVNUM=002\0SEQNUM=1234\0";

    #[test]
    fn parses_a_usb_add() {
        let ev = Uevent::parse(USB_ADD).unwrap();
        assert_eq!(ev.action, "add");
        assert_eq!(ev.devpath, "/devices/platform/xhci-hcd.1/usb3/3-1");
        assert_eq!(ev.get("DEVTYPE"), Some("usb_device"));
        assert_eq!(ev.subsystem(), Some("usb"));
        assert_eq!(ev.get("BUSNUM"), Some("003"));
        assert_eq!(ev.get("MISSING"), None);
        assert!(Uevent::parse(b"libudev\0junk").is_none());
    }

    #[test]
    fn borrowed_view_matches_the_owned_form() {
        let msg = UeventMsg::parse(USB_ADD).unwrap();
        assert_eq!(msg.action(), Action::Add);
        assert_eq!(msg.action_str(), "add");
        assert_eq!(msg.subsystem(), Some("usb"));
        assert_eq!(msg.devtype(), Some("usb_device"));
        assert_eq!(msg.devname(), Some("bus/usb/003/002"));
        assert_eq!(msg.get("DEVNUM"), Some("002"));
        assert_eq!(msg.to_uevent(), Uevent::parse(USB_ADD).unwrap());
        assert_eq!(msg.vars().count(), 9);
    }

    #[test]
    fn parses_video_and_media_nodes() {
        // Recorded shapes of the kernel's V4L2 and media-controller events.
        let video = b"add@/devices/pci0000:00/0000:00:14.0/usb3/3-2/3-2:1.0/video4linux/video3\0ACTION=add\0DEVPATH=/devices/pci0000:00/0000:00:14.0/usb3/3-2/3-2:1.0/video4linux/video3\0SUBSYSTEM=video4linux\0DEVNAME=video3\0MAJOR=81\0MINOR=3\0SEQNUM=2201\0";
        let msg = UeventMsg::parse(video).unwrap();
        assert_eq!(msg.action(), Action::Add);
        assert_eq!(msg.subsystem(), Some("video4linux"));
        assert_eq!(msg.devname(), Some("video3"));
        assert_eq!(msg.get("MAJOR"), Some("81"));
        assert_eq!(msg.get("MINOR"), Some("3"));

        let media = b"remove@/devices/platform/soc/fe800000.codec/media1\0ACTION=remove\0SUBSYSTEM=media\0DEVNAME=media1\0MAJOR=246\0MINOR=1\0";
        let msg = UeventMsg::parse(media).unwrap();
        assert_eq!(msg.action(), Action::Remove);
        assert_eq!(msg.subsystem(), Some("media"));
        assert_eq!(msg.devname(), Some("media1"));
    }

    #[test]
    fn parses_usb_bind_unbind_and_change() {
        let bind = b"bind@/devices/platform/xhci-hcd.1/usb3/3-1/3-1:1.0\0ACTION=bind\0DEVPATH=/devices/platform/xhci-hcd.1/usb3/3-1/3-1:1.0\0SUBSYSTEM=usb\0DEVTYPE=usb_interface\0DRIVER=uvcvideo\0PRODUCT=46d/825/12\0TYPE=14/2/0\0INTERFACE=14/2/0\0MODALIAS=usb:v046Dp0825d0012dc00dsc00dp00ic0Ec02isc00ip00in00\0";
        let msg = UeventMsg::parse(bind).unwrap();
        assert_eq!(msg.action(), Action::Bind);
        assert_eq!(msg.devtype(), Some("usb_interface"));
        assert_eq!(msg.get("DRIVER"), Some("uvcvideo"));

        let unbind = b"unbind@/devices/platform/xhci-hcd.1/usb3/3-1/3-1:1.0\0ACTION=unbind\0SUBSYSTEM=usb\0DEVTYPE=usb_interface\0";
        assert_eq!(UeventMsg::parse(unbind).unwrap().action(), Action::Unbind);

        let change = b"change@/devices/platform/xhci-hcd.1/usb3/3-1\0ACTION=change\0SUBSYSTEM=usb\0DEVTYPE=usb_device\0";
        assert_eq!(UeventMsg::parse(change).unwrap().action(), Action::Change);
    }

    #[test]
    fn unknown_actions_and_malformed_variables_are_tolerated() {
        let msg = UeventMsg::parse(
            b"frobnicate@/devices/x\0ACTION=frobnicate\0NOEQUALS\0\xff=bad\0SUBSYSTEM=gpio\0",
        )
        .unwrap();
        assert_eq!(msg.action(), Action::Other);
        assert_eq!(msg.action_str(), "frobnicate");
        assert_eq!(msg.subsystem(), Some("gpio"));
        assert_eq!(msg.vars().count(), 2);
        assert!(UeventMsg::parse(b"no-header-separator\0A=B\0").is_none());
        assert!(UeventMsg::parse(&[0xff, b'@', b'x']).is_none());
    }

    #[test]
    fn encoded_messages_round_trip_through_the_parser() {
        let mut buf = Vec::new();
        encode(
            &mut buf,
            "add",
            "/devices/x/video9",
            [("SUBSYSTEM", "video4linux"), ("DEVNAME", "video9")],
        );
        let msg = UeventMsg::parse(&buf).unwrap();
        assert_eq!(msg.action(), Action::Add);
        assert_eq!(msg.devpath(), "/devices/x/video9");
        assert_eq!(msg.devname(), Some("video9"));
        // Reusing the buffer replaces the message.
        encode(&mut buf, "remove", "/devices/y", []);
        let msg = UeventMsg::parse(&buf).unwrap();
        assert_eq!(msg.action(), Action::Remove);
        assert_eq!(msg.vars().count(), 0);
    }

    #[test]
    fn action_names_round_trip() {
        for action in [
            Action::Add,
            Action::Remove,
            Action::Change,
            Action::Move,
            Action::Bind,
            Action::Unbind,
            Action::Online,
            Action::Offline,
        ] {
            assert_eq!(Action::parse(action.as_str()), action);
        }
    }

    #[test]
    fn opens_where_netlink_is_allowed() {
        let Ok(mut sock) = UeventSocket::open() else {
            return;
        };
        assert!(sock.recv().is_ok());
        assert!(!sock.wait(Some(Duration::ZERO)).unwrap() || sock.recv().is_ok());
        assert!(sock.drain(|_| {}).is_ok());
    }
}
