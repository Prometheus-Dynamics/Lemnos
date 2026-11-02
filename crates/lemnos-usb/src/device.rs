//! An opened USB device: claims, endpoint lookup, and the transfers.
//!
//! Endpoints are addressed by their `bEndpointAddress`, as on the wire. A
//! transfer needs its interface claimed on Windows and macOS; on Linux the
//! kernel also wants the claim for bulk and interrupt endpoints.

use crate::config::ConfigurationInfo;
#[cfg(target_os = "linux")]
use crate::config::configurations_from_blob;
use crate::error::{Error, ErrorKind, Result};
use crate::info::UsbDeviceInfo;
use crate::listing::find_device;
use crate::location::UsbLocation;
use crate::stream::{BulkReader, BulkWriter};
use crate::transfer::Transfer;
use crate::types::{ControlSetup, Direction, TransferType};
use std::sync::Arc;
use std::time::Duration;

#[cfg(target_os = "linux")]
use crate::linux::Usbfs;
#[cfg(not(target_os = "linux"))]
use crate::native::Native;

/// The timeout of the control requests the crate makes itself.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(1);

/// The bus-specific handle.
enum Backend {
    #[cfg(target_os = "linux")]
    Usbfs(Arc<Usbfs>),
    #[cfg(not(target_os = "linux"))]
    Native(Native),
}

struct Inner {
    info: UsbDeviceInfo,
    backend: Backend,
    configurations: Vec<ConfigurationInfo>,
}

/// An open USB device. Cheap to clone: clones share the same handle.
#[derive(Clone)]
pub struct UsbDevice {
    inner: Arc<Inner>,
}

impl UsbDevice {
    /// Opens the device a listing describes.
    pub fn open(info: &UsbDeviceInfo) -> Result<Self> {
        #[cfg(target_os = "linux")]
        let (backend, configurations) = {
            let usb = Usbfs::open(info.native())?;
            let configurations = configurations_from_blob(&usb.read_descriptors()?)?;
            (Backend::Usbfs(usb), configurations)
        };
        #[cfg(not(target_os = "linux"))]
        let (backend, configurations) = {
            let native = Native::open(info.native())?;
            let configurations = native.configurations()?;
            (Backend::Native(native), configurations)
        };
        Ok(Self {
            inner: Arc::new(Inner {
                info: info.clone(),
                backend,
                configurations,
            }),
        })
    }

    /// Opens the device at `location` now.
    pub fn open_location(location: &UsbLocation) -> Result<Self> {
        let info = find_device(location)?.ok_or_else(|| {
            Error::new(ErrorKind::NotFound, format!("no USB device at {location}"))
        })?;
        Self::open(&info)
    }

    /// What the listing said about the device when it was opened.
    pub fn info(&self) -> &UsbDeviceInfo {
        &self.inner.info
    }

    /// Every configuration the device has, as parsed when it was opened.
    pub fn configurations(&self) -> &[ConfigurationInfo] {
        &self.inner.configurations
    }

    /// The configuration the device is in now.
    pub fn active_configuration(&self) -> Result<ConfigurationInfo> {
        let value = match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.active_configuration(CONTROL_TIMEOUT)?,
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => native.active_configuration()?,
        };
        self.inner
            .configurations
            .iter()
            .find(|configuration| configuration.value == value)
            .cloned()
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    format!("configuration {value} is not in the descriptors"),
                )
            })
    }

    /// Selects configuration `value`. Claims are released first by the caller.
    pub fn set_configuration(&self, value: u8) -> Result<()> {
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.set_configuration(value),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => native.set_configuration(value),
        }
    }

    /// Claims interface `number` for exclusive use.
    pub fn claim_interface(&self, number: u8) -> Result<Interface> {
        self.claim(number, false)
    }

    /// Claims interface `number`, first unbinding the kernel driver that has
    /// it (Linux). Elsewhere this is the same as [`UsbDevice::claim_interface`].
    /// The driver is bound again when the claim is released.
    pub fn detach_and_claim_interface(&self, number: u8) -> Result<Interface> {
        self.claim(number, true)
    }

    fn claim(&self, number: u8, detach: bool) -> Result<Interface> {
        let detached = match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => {
                usb.claim_interface(number, detach)?;
                detach
            }
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => {
                // WinUSB and IOKit claim without unbinding anything.
                let _ = detach;
                native.claim_interface(number)?;
                false
            }
        };
        Ok(Interface {
            device: Arc::clone(&self.inner),
            number,
            detached,
        })
    }

    /// Resets the device. Its claims are lost; reopen it afterwards.
    pub fn reset(&self) -> Result<()> {
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.reset(),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => native.reset(),
        }
    }

    /// A control IN transfer of up to `length` bytes.
    pub fn control_in(
        &self,
        setup: &ControlSetup,
        length: u16,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        check_direction(setup.direction, Direction::In, "control read")?;
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => {
                let mut data = vec![0; usize::from(length)];
                let received = usb.control_in(setup, &mut data, timeout)?;
                data.truncate(received);
                Ok(data)
            }
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => native.control_in(setup, length, timeout),
        }
    }

    /// A control OUT transfer; returns the bytes sent.
    pub fn control_out(
        &self,
        setup: &ControlSetup,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize> {
        check_direction(setup.direction, Direction::Out, "control write")?;
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.control_out(setup, data, timeout),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => native.control_out(setup, data, timeout),
        }
    }

    /// A bulk IN transfer into `buffer`; returns the bytes received.
    pub fn bulk_read(&self, endpoint: u8, buffer: &mut [u8], timeout: Duration) -> Result<usize> {
        self.read(endpoint, TransferType::Bulk, buffer, timeout)
    }

    /// A bulk OUT transfer of `data`; returns the bytes sent.
    pub fn bulk_write(&self, endpoint: u8, data: &[u8], timeout: Duration) -> Result<usize> {
        self.write(endpoint, TransferType::Bulk, data, timeout)
    }

    /// An interrupt IN transfer into `buffer`; returns the bytes received.
    pub fn interrupt_read(
        &self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout: Duration,
    ) -> Result<usize> {
        self.read(endpoint, TransferType::Interrupt, buffer, timeout)
    }

    /// An interrupt OUT transfer of `data`; returns the bytes sent.
    pub fn interrupt_write(&self, endpoint: u8, data: &[u8], timeout: Duration) -> Result<usize> {
        self.write(endpoint, TransferType::Interrupt, data, timeout)
    }

    fn read(
        &self,
        endpoint: u8,
        transfer: TransferType,
        buffer: &mut [u8],
        timeout: Duration,
    ) -> Result<usize> {
        check_endpoint(endpoint, Direction::In)?;
        // usbfs runs bulk and interrupt endpoints through one ioctl, so Linux
        // ignores the transfer type.
        let _ = transfer;
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.bulk_in(endpoint, buffer, timeout),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => {
                let data = native.transfer_in(endpoint, transfer, buffer.len(), timeout)?;
                buffer[..data.len()].copy_from_slice(&data);
                Ok(data.len())
            }
        }
    }

    fn write(
        &self,
        endpoint: u8,
        transfer: TransferType,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize> {
        check_endpoint(endpoint, Direction::Out)?;
        if data.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "transfer payload must not be empty",
            ));
        }
        // See `read`: usbfs ignores the transfer type.
        let _ = transfer;
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.bulk_out(endpoint, data, timeout),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => native.transfer_out(endpoint, transfer, data, timeout),
        }
    }

    /// Starts a bulk IN transfer that fills `buffer` (its length is the
    /// most it asks for). Await the [`Transfer`], or wait for it.
    pub fn submit_bulk_in(&self, endpoint: u8, buffer: Vec<u8>) -> Result<Transfer> {
        self.submit(endpoint, TransferType::Bulk, Direction::In, buffer)
    }

    /// Starts a bulk OUT transfer of `data`.
    pub fn submit_bulk_out(&self, endpoint: u8, data: Vec<u8>) -> Result<Transfer> {
        self.submit(endpoint, TransferType::Bulk, Direction::Out, data)
    }

    /// Starts an interrupt IN transfer that fills `buffer`.
    pub fn submit_interrupt_in(&self, endpoint: u8, buffer: Vec<u8>) -> Result<Transfer> {
        self.submit(endpoint, TransferType::Interrupt, Direction::In, buffer)
    }

    /// Starts an interrupt OUT transfer of `data`.
    pub fn submit_interrupt_out(&self, endpoint: u8, data: Vec<u8>) -> Result<Transfer> {
        self.submit(endpoint, TransferType::Interrupt, Direction::Out, data)
    }

    fn submit(
        &self,
        endpoint: u8,
        transfer: TransferType,
        direction: Direction,
        buffer: Vec<u8>,
    ) -> Result<Transfer> {
        check_endpoint(endpoint, direction)?;
        if direction == Direction::Out && buffer.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "transfer payload must not be empty",
            ));
        }
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => {
                let kind = crate::linux::urb_kind(transfer).ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        "control transfers are not submitted",
                    )
                })?;
                usb.submit(kind, endpoint, buffer, &[])
            }
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => native.submit(endpoint, transfer, direction, buffer),
        }
    }

    /// Starts an isochronous IN transfer: `buffer` is split into packets of
    /// `packet_len` bytes (the last one may be shorter). Linux only: nusb,
    /// used elsewhere, has no isochronous transfers.
    pub fn submit_isochronous_in(
        &self,
        endpoint: u8,
        buffer: Vec<u8>,
        packet_len: usize,
    ) -> Result<Transfer> {
        check_endpoint(endpoint, Direction::In)?;
        if packet_len == 0 || buffer.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "isochronous transfers need a packet length and a buffer",
            ));
        }
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => {
                let packets = buffer.len().div_ceil(packet_len);
                let mut lengths = vec![packet_len as u32; packets];
                if let Some(last) = lengths.last_mut() {
                    *last = (buffer.len() - (packets - 1) * packet_len) as u32;
                }
                usb.submit(
                    lemnos_linux_sys::usbfs::UrbKind::Isochronous,
                    endpoint,
                    buffer,
                    &lengths,
                )
            }
            #[cfg(not(target_os = "linux"))]
            Backend::Native(_) => Err(Error::new(
                ErrorKind::Unsupported,
                "isochronous transfers are supported on Linux only",
            )),
        }
    }

    /// Clears the halt on `endpoint` (after a stall).
    pub fn clear_halt(&self, endpoint: u8) -> Result<()> {
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.clear_halt(endpoint),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => native
                .control_out(
                    &ControlSetup {
                        direction: Direction::Out,
                        kind: crate::types::ControlKind::Standard,
                        recipient: crate::types::ControlRecipient::Endpoint,
                        request: 1,
                        value: 0,
                        index: u16::from(endpoint),
                    },
                    &[],
                    CONTROL_TIMEOUT,
                )
                .map(|_| ()),
        }
    }

    /// A buffered reader over bulk IN endpoint `endpoint`: `depth` transfers
    /// of `chunk` bytes stay in flight, so the device is never waiting for the
    /// host between them.
    pub fn bulk_reader(&self, endpoint: u8, chunk: usize, depth: usize) -> BulkReader {
        BulkReader::new(self.clone(), endpoint, chunk, depth)
    }

    /// A buffered writer to bulk OUT endpoint `endpoint`: writes are gathered
    /// into `chunk`-byte transfers, up to `depth` of them in flight. Flush (or
    /// drop) the writer to send the tail.
    pub fn bulk_writer(&self, endpoint: u8, chunk: usize, depth: usize) -> BulkWriter {
        BulkWriter::new(self.clone(), endpoint, chunk, depth)
    }
}

/// A claimed interface. Releasing it (dropping it) lets others claim it, and
/// hands it back to the kernel driver if the claim detached one.
pub struct Interface {
    device: Arc<Inner>,
    number: u8,
    // Read on Linux only, where a detaching claim is released with a reattach.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    detached: bool,
}

impl Interface {
    /// `bInterfaceNumber`.
    pub fn number(&self) -> u8 {
        self.number
    }

    /// Selects alternate setting `alternate` of this interface.
    pub fn set_alternate(&self, alternate: u8) -> Result<()> {
        match &self.device.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.set_interface(self.number, alternate),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => native.set_alternate(self.number, alternate),
        }
    }
}

impl Drop for Interface {
    fn drop(&mut self) {
        match &self.device.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => {
                let _ = usb.release_interface(self.number, self.detached);
            }
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => native.release_interface(self.number),
        }
    }
}

/// Endpoint 0 is the control endpoint: never a bulk or interrupt address.
pub(crate) fn check_endpoint(endpoint: u8, direction: Direction) -> Result<()> {
    if endpoint & 0x0f == 0 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "endpoint zero is the control endpoint",
        ));
    }
    let wanted = match direction {
        Direction::In => 0x80,
        Direction::Out => 0x00,
    };
    if endpoint & 0x80 != wanted {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("endpoint {endpoint:#04x} is not an {direction:?} endpoint"),
        ));
    }
    Ok(())
}

fn check_direction(actual: Direction, wanted: Direction, context: &str) -> Result<()> {
    if actual == wanted {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{context}: wrong direction in the setup packet"),
        ))
    }
}
