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

/// The libusb transport. It holds the handle of the device it opened, and the port that device is
/// on, so after a reset the same port is waited for.
pub struct RusbIo {
    context: Context,
    handle: DeviceHandle<Context>,
    port: Port,
    timeout: Duration,
}

impl RusbIo {
    /// Opens the download-mode device (VID `29f1`, PID `0230`) and claims its interface. Opens a
    /// device, so the tests never call it.
    pub fn open_download() -> Result<Self, UsbError> {
        let context = Context::new().map_err(|e| map_error(e, 0))?;
        let devices = context.devices().map_err(|e| map_error(e, 0))?;
        for device in devices.iter() {
            let descriptor = device.device_descriptor().map_err(|e| map_error(e, 0))?;
            if descriptor.vendor_id() == DOWNLOAD_VID && descriptor.product_id() == DOWNLOAD_PID {
                let port = Port {
                    bus: device.bus_number(),
                    path: device.port_numbers().map_err(|e| map_error(e, 0))?,
                };
                let handle = device.open().map_err(|e| map_error(e, 0))?;
                handle
                    .claim_interface(INTERFACE)
                    .map_err(|e| map_error(e, 0))?;
                return Ok(Self {
                    context,
                    handle,
                    port,
                    timeout: Duration::from_millis(u64::from(ROM_TIMEOUT_MS)),
                });
            }
        }
        Err(UsbError::Other(format!(
            "no device {DOWNLOAD_VID:04x}:{DOWNLOAD_PID:04x} on the bus"
        )))
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
            .map_err(|e| map_error(e, self.timeout_ms()))?;
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

    #[test]
    fn a_device_back_at_its_port_with_other_ids_is_reported_with_those_ids() {
        let seen = vec![(port(1, &[2]), 0x1234, 0x0230)];
        assert_eq!(find_at_port(&seen, &port(1, &[2])), Some((0x1234, 0x0230)));
    }
}
