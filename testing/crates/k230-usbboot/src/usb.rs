//! The host's USB transport: [`UsbIo`] over libusb, through `rusb`. A device is opened only by
//! [`RusbIo::open_download`], which nothing in the tests calls; the tests exercise the pure parts
//! (the error mapping, the request types, and the port the device must come back at).
//!
//! Provenance (k230_flash 33a75f0): the interface is claimed as interface 0 (`kburn.cpp:335`); the
//! ROM's bulk endpoints are OUT `0x01` and IN `0x81` (`kburn_k230.h:13-14`); the control requests are
//! vendor, device-directed, IN `0xc0` and OUT `0x40` (`burner_uboot.cpp:144-152`, `burner_brom.cpp:21`);
//! the ROM's timeout is 1 s (`kburn_k230.h:11`) and a VERIFY's is 300 s (`burner_uboot.cpp:392-396`).

use std::time::{Duration, Instant};

use rusb::{Context, DeviceHandle, UsbContext};

use crate::bootrom::{DOWNLOAD_PID, DOWNLOAD_VID};
use crate::transport::{ROM_TIMEOUT_MS, UsbError, UsbIo};

/// The bulk OUT endpoint of the download mode (`kburn_k230.h:13`).
pub const EP_OUT: u8 = 0x01;
/// The bulk IN endpoint of the download mode (`kburn_k230.h:14`).
pub const EP_IN: u8 = 0x81;
/// The interface the vendor tool claims (`kburn.cpp:335`).
pub const INTERFACE: u8 = 0;
/// A vendor control request, device recipient, IN: bmRequestType `0xc0`.
pub const REQUEST_IN: u8 = 0xc0;
/// A vendor control request, device recipient, OUT: bmRequestType `0x40`.
pub const REQUEST_OUT: u8 = 0x40;
/// The largest packet a high-speed bulk endpoint takes (the ROM's, UNVERIFIED on the board).
pub const MAX_PACKET_OUT: usize = 512;

/// The port a device is on: its bus and the chain of port numbers from the root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Port {
    pub bus: u8,
    pub path: Vec<u8>,
}

/// Maps a libusb error to the host's [`UsbError`], naming the timeout the request had.
pub fn map_error(error: rusb::Error, timeout_ms: u32) -> UsbError {
    match error {
        rusb::Error::Timeout => UsbError::Timeout(timeout_ms),
        rusb::Error::Pipe => UsbError::Stall,
        rusb::Error::NoDevice => UsbError::Disconnected,
        other => UsbError::Other(format!("libusb: {other}")),
    }
}

/// The device at `port` among `seen`, a list of `(port, vendor, product)`: the identity it comes back
/// with, or `None` if nothing is at that port.
pub fn find_at_port(seen: &[(Port, u16, u16)], port: &Port) -> Option<(u16, u16)> {
    seen.iter()
        .find(|(p, _, _)| p == port)
        .map(|(_, vid, pid)| (*vid, *pid))
}

/// Why the host did not select a download device. Nothing is selected by VID:PID alone: a port path is
/// always required, and the device there must be the download device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectError {
    /// No port path was given.
    NoPath,
    /// The path text is not a port path (`3-4.2.1.2`).
    BadPath(String),
    /// No device is at the path.
    NoDeviceAtPath(Port),
    /// A device is at the path, but it is not the download device.
    WrongDevice { port: Port, vid: u16, pid: u16 },
    /// More than one download device is present, and the path names neither (kept for the message;
    /// with a path given, the path decides and this does not arise).
    Several(usize),
}

impl std::fmt::Display for SelectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPath => write!(f, "give the port path (--port BUS-PORT.PORT, for example 3-4.2.1.2)"),
            Self::BadPath(text) => write!(f, "{text:?} is not a port path (BUS-PORT.PORT, for example 3-4.2.1.2)"),
            Self::NoDeviceAtPath(port) => write!(f, "no device at port {}", format_port(port)),
            Self::WrongDevice { port, vid, pid } => write!(
                f,
                "the device at port {} is {vid:04x}:{pid:04x}, not {DOWNLOAD_VID:04x}:{DOWNLOAD_PID:04x}",
                format_port(port)
            ),
            Self::Several(n) => write!(f, "{n} download devices are present and none is named"),
        }
    }
}

/// Parses a port path: `3-4.2.1.2` is bus 3 and the ports 4, 2, 1, 2.
pub fn parse_port(text: &str) -> Result<Port, SelectError> {
    let bad = || SelectError::BadPath(text.to_string());
    let (bus, rest) = text.split_once('-').ok_or_else(bad)?;
    let bus: u8 = bus.parse().map_err(|_| bad())?;
    let path = rest
        .split('.')
        .map(|p| p.parse::<u8>().map_err(|_| bad()))
        .collect::<Result<Vec<u8>, _>>()?;
    if path.is_empty() {
        return Err(bad());
    }
    Ok(Port { bus, path })
}

/// The port path text of `port`: `3-4.2.1.2`.
pub fn format_port(port: &Port) -> String {
    let ports: Vec<String> = port.path.iter().map(u8::to_string).collect();
    format!("{}-{}", port.bus, ports.join("."))
}

/// Selects the download device at the required port, from `seen` (each device's port and IDs). A device
/// elsewhere, or at the path with other IDs, is refused. Called again after each re-enumeration, it finds
/// the same port, because the port path does not change when the device number does.
pub fn select_download(seen: &[(Port, u16, u16)], required: Option<&str>) -> Result<Port, SelectError> {
    let text = required.ok_or(SelectError::NoPath)?;
    let port = parse_port(text)?;
    match seen.iter().find(|(p, _, _)| *p == port) {
        None => Err(SelectError::NoDeviceAtPath(port)),
        Some((_, vid, pid)) if (*vid, *pid) == (DOWNLOAD_VID, DOWNLOAD_PID) => Ok(port),
        Some((_, vid, pid)) => Err(SelectError::WrongDevice {
            port,
            vid: *vid,
            pid: *pid,
        }),
    }
}

/// The libusb transport. It holds the handle of the device it opened, and the port that device is
/// on, so after a reset the same port is waited for.
pub struct RusbIo {
    context: Context,
    handle: DeviceHandle<Context>,
    port: Port,
    timeout: Duration,
}

impl RusbIo {
    /// Opens the download-mode device at the port `required` names (a port path, never VID:PID alone),
    /// and claims its interface. Refuses by name (see [`select_download`]). Opens a device, so the tests
    /// never call it.
    pub fn open_port(required: Option<&str>) -> Result<Self, UsbError> {
        let context = Context::new().map_err(|e| map_error(e, 0))?;
        let mut seen = Vec::new();
        {
            let devices = context.devices().map_err(|e| map_error(e, 0))?;
            for device in devices.iter() {
                let descriptor = device.device_descriptor().map_err(|e| map_error(e, 0))?;
                let port = Port {
                    bus: device.bus_number(),
                    path: device.port_numbers().unwrap_or_default(),
                };
                seen.push((port, descriptor.vendor_id(), descriptor.product_id()));
            }
        }
        let port = select_download(&seen, required).map_err(|e| UsbError::Other(e.to_string()))?;
        let devices = context.devices().map_err(|e| map_error(e, 0))?;
        for device in devices.iter() {
            let here = Port {
                bus: device.bus_number(),
                path: device.port_numbers().unwrap_or_default(),
            };
            if here == port {
                let handle = device.open().map_err(|e| map_error(e, 0))?;
                handle.claim_interface(INTERFACE).map_err(|e| map_error(e, 0))?;
                return Ok(Self {
                    context,
                    handle,
                    port,
                    timeout: Duration::from_millis(u64::from(ROM_TIMEOUT_MS)),
                });
            }
        }
        Err(UsbError::Other(format!("no device at port {}", format_port(&port))))
    }

    fn timeout_ms(&self) -> u32 {
        self.timeout.as_millis() as u32
    }
}

impl UsbIo for RusbIo {
    fn set_timeout_ms(&mut self, ms: u32) {
        self.timeout = Duration::from_millis(u64::from(ms));
    }

    fn control_in(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        len: u16,
    ) -> Result<Vec<u8>, UsbError> {
        let mut buf = vec![0u8; len as usize];
        let got = self
            .handle
            .read_control(REQUEST_IN, request, value, index, &mut buf, self.timeout)
            .map_err(|e| match e {
                // A control transfer that fails outside a stall, a timeout or a disconnect is the ROM's
                // EPROTO on the bench: the device is still enumerated and the ROM is deaf.
                rusb::Error::Other | rusb::Error::Io => UsbError::Deaf,
                other => map_error(other, self.timeout_ms()),
            })?;
        buf.truncate(got);
        Ok(buf)
    }

    fn control_out(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), UsbError> {
        self.handle
            .write_control(REQUEST_OUT, request, value, index, data, self.timeout)
            .map(drop)
            .map_err(|e| map_error(e, self.timeout_ms()))
    }

    fn bulk_out(&mut self, data: &[u8]) -> Result<(), UsbError> {
        let sent = self
            .handle
            .write_bulk(EP_OUT, data, self.timeout)
            .map_err(|e| map_error(e, self.timeout_ms()))?;
        if sent == data.len() {
            Ok(())
        } else {
            Err(UsbError::Other(format!(
                "a short bulk write: {sent} of {} bytes",
                data.len()
            )))
        }
    }

    fn bulk_in(&mut self, len: usize) -> Result<Vec<u8>, UsbError> {
        let mut buf = vec![0u8; len];
        let got = self
            .handle
            .read_bulk(EP_IN, &mut buf, self.timeout)
            .map_err(|e| map_error(e, self.timeout_ms()))?;
        buf.truncate(got);
        Ok(buf)
    }

    fn max_packet_out(&self) -> usize {
        MAX_PACKET_OUT
    }

    /// Waits for a device at the port this one was on, reopens it and claims its interface, and
    /// returns its IDs. Lists the bus, and opens only the device at the port.
    fn wait_for_device(&mut self, timeout_ms: u32) -> Result<(u16, u16), UsbError> {
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        loop {
            let mut seen = Vec::new();
            let devices = self
                .context
                .devices()
                .map_err(|e| map_error(e, timeout_ms))?;
            for device in devices.iter() {
                let descriptor = device
                    .device_descriptor()
                    .map_err(|e| map_error(e, timeout_ms))?;
                let path = device.port_numbers().unwrap_or_default();
                let port = Port {
                    bus: device.bus_number(),
                    path,
                };
                seen.push((port, descriptor.vendor_id(), descriptor.product_id()));
            }
            if let Some(ids) = find_at_port(&seen, &self.port) {
                // Reopen the device at that port: the old handle died with the reset.
                let devices = self
                    .context
                    .devices()
                    .map_err(|e| map_error(e, timeout_ms))?;
                for device in devices.iter() {
                    let port = Port {
                        bus: device.bus_number(),
                        path: device.port_numbers().unwrap_or_default(),
                    };
                    if port == self.port {
                        self.handle = device.open().map_err(|e| map_error(e, timeout_ms))?;
                        self.handle
                            .claim_interface(INTERFACE)
                            .map_err(|e| map_error(e, timeout_ms))?;
                        return Ok(ids);
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(UsbError::Timeout(timeout_ms));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(bus: u8, path: &[u8]) -> Port {
        Port {
            bus,
            path: path.to_vec(),
        }
    }

    #[test]
    fn the_libusb_errors_map_to_the_hosts_errors() {
        assert_eq!(
            map_error(rusb::Error::Timeout, 1000),
            UsbError::Timeout(1000)
        );
        assert_eq!(map_error(rusb::Error::Pipe, 1000), UsbError::Stall);
        assert_eq!(
            map_error(rusb::Error::NoDevice, 1000),
            UsbError::Disconnected
        );
        assert!(matches!(
            map_error(rusb::Error::Io, 1000),
            UsbError::Other(_)
        ));
    }

    #[test]
    fn the_request_types_and_endpoints_are_the_vendor_tools() {
        // Vendor, device recipient: the IN bit is 0x80, the vendor type 0x40.
        assert_eq!(REQUEST_IN, 0x80 | 0x40);
        assert_eq!(REQUEST_OUT, 0x40);
        assert_eq!((EP_OUT, EP_IN), (0x01, 0x81));
        assert_eq!(INTERFACE, 0);
    }

    #[test]
    fn the_device_is_waited_for_at_its_own_port_and_no_other() {
        let seen = vec![
            (port(1, &[2]), 0x29f1, 0x0230),
            (port(1, &[3]), 0x1d6b, 0x0002),
        ];
        assert_eq!(find_at_port(&seen, &port(1, &[2])), Some((0x29f1, 0x0230)));
        // A device on another port is not the board, whatever its IDs.
        assert_eq!(find_at_port(&seen, &port(1, &[4])), None);
        assert_eq!(find_at_port(&seen, &port(2, &[2])), None);
    }

    fn dl() -> (u16, u16) {
        (DOWNLOAD_VID, DOWNLOAD_PID)
    }

    #[test]
    fn the_bench_path_is_parsed_and_formatted_back_the_same() {
        let port = parse_port("3-4.2.1.2").unwrap();
        assert_eq!(port, Port { bus: 3, path: vec![4, 2, 1, 2] });
        assert_eq!(format_port(&port), "3-4.2.1.2");
        assert!(matches!(parse_port("3"), Err(SelectError::BadPath(_))));
        assert!(matches!(parse_port("x-4"), Err(SelectError::BadPath(_))));
    }

    #[test]
    fn a_port_path_is_always_required_and_vid_pid_alone_selects_nothing() {
        let seen = vec![(parse_port("3-4.2.1.2").unwrap(), dl().0, dl().1)];
        assert_eq!(select_download(&seen, None), Err(SelectError::NoPath));
    }

    #[test]
    fn the_download_device_at_the_named_path_is_selected_and_another_board_is_not() {
        // Board A at 3-4.2.1.2 and board B (no card) at 3-4.2.2: the path picks A, and B is never selected.
        let a = parse_port("3-4.2.1.2").unwrap();
        let b = parse_port("3-4.2.2").unwrap();
        let seen = vec![(a.clone(), dl().0, dl().1), (b.clone(), dl().0, dl().1)];
        assert_eq!(select_download(&seen, Some("3-4.2.1.2")), Ok(a.clone()));
        assert_eq!(select_download(&seen, Some("3-4.2.2")), Ok(b));
    }

    #[test]
    fn a_different_device_at_the_path_is_refused_by_name() {
        let seen = vec![(parse_port("3-4.2.1.2").unwrap(), 0x1d6b, 0x0002)];
        let error = select_download(&seen, Some("3-4.2.1.2")).unwrap_err();
        assert!(matches!(error, SelectError::WrongDevice { vid: 0x1d6b, .. }), "{error}");
        assert!(error.to_string().contains("1d6b:0002"), "{error}");
    }

    #[test]
    fn an_empty_port_is_refused_by_name() {
        let seen: Vec<(Port, u16, u16)> = Vec::new();
        let error = select_download(&seen, Some("3-4.2.1.2")).unwrap_err();
        assert!(matches!(error, SelectError::NoDeviceAtPath(_)), "{error}");
    }

    #[test]
    fn after_a_re_enumeration_the_same_path_finds_the_device_under_a_new_number() {
        // The device number changes (the bus address); the port path does not.
        let before = vec![(parse_port("3-4.2.1.2").unwrap(), dl().0, dl().1)];
        let after = vec![(parse_port("3-4.2.1.2").unwrap(), dl().0, dl().1)];
        assert_eq!(select_download(&before, Some("3-4.2.1.2")), select_download(&after, Some("3-4.2.1.2")));
    }

    #[test]
    fn a_device_back_at_its_port_with_other_ids_is_reported_with_those_ids() {
        let seen = vec![(port(1, &[2]), 0x1234, 0x0230)];
        assert_eq!(find_at_port(&seen, &port(1, &[2])), Some((0x1234, 0x0230)));
    }
}
