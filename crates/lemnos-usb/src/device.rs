//! An opened USB device: claims, endpoint lookup, and the transfers.
//!
//! Endpoints are addressed by their `bEndpointAddress`, as on the wire. A
//! transfer needs its interface claimed on Windows and macOS; on Linux the
//! kernel also wants the claim for bulk and interrupt endpoints.

use crate::config::ConfigurationInfo;
#[cfg(target_os = "linux")]
use crate::config::configurations_from_blob;
use crate::descriptor::DeviceDescriptor;
use crate::error::{Error, ErrorKind, Result};
use crate::info::{Speed, UsbDeviceInfo};
use crate::listing::find_device;
use crate::location::UsbLocation;
use crate::stream::{BulkReader, BulkWriter};
#[cfg(target_os = "linux")]
use crate::transfer::IsoPacket;
use crate::transfer::{Transfer, TransferBuffer, TransferOutcome, TransferStatus};
use crate::types::{ControlSetup, Direction, TransferType};
use std::sync::{Arc, Mutex, MutexGuard};
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
    descriptor: DeviceDescriptor,
    configurations: Vec<ConfigurationInfo>,
    /// The interfaces this handle has claimed. Transfers on an interface that
    /// is not in here are refused before the kernel sees them.
    claimed: Mutex<Vec<u8>>,
}

/// An open USB device. Cheap to clone: clones share the same handle.
#[derive(Clone)]
pub struct UsbDevice {
    inner: Arc<Inner>,
}

impl UsbDevice {
    /// Opens the device a listing describes.
    pub fn open(info: &UsbDeviceInfo) -> Result<Self> {
        // The raw blob starts with the device descriptor: no extra request.
        #[cfg(target_os = "linux")]
        let (backend, descriptor, configurations) = {
            let usb = Usbfs::open(info.native())?;
            let blob = usb.read_descriptors()?;
            let descriptor = DeviceDescriptor::from_bytes(&blob)?;
            let configurations = configurations_from_blob(&blob)?;
            (Backend::Usbfs(usb), descriptor, configurations)
        };
        #[cfg(not(target_os = "linux"))]
        let (backend, descriptor, configurations) = {
            let native = Native::open(info.native())?;
            let descriptor = native.device_descriptor()?;
            let configurations = native.configurations()?;
            (Backend::Native(native), descriptor, configurations)
        };
        Ok(Self {
            inner: Arc::new(Inner {
                info: info.clone(),
                backend,
                descriptor,
                configurations,
                claimed: Mutex::new(Vec::new()),
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

    /// The device descriptor, as read when the device was opened: versions,
    /// class, IDs, string indices and the configuration count. No request is
    /// made for it. A boot ROM and its loader differ here (for example in
    /// `iSerialNumber`, or in `bcdUSB`).
    pub fn device_descriptor(&self) -> &DeviceDescriptor {
        &self.inner.descriptor
    }

    /// Every configuration the device has, as parsed when it was opened.
    pub fn configurations(&self) -> &[ConfigurationInfo] {
        &self.inner.configurations
    }

    /// The kernel driver bound to interface `interface` (Linux, `USBDEVFS_GETDRIVER`):
    /// its name, or `None` when no driver is bound. Other platforms are
    /// `Unsupported`: WinUSB and IOKit do not report a bound driver this way.
    pub fn driver(&self, interface: u8) -> Result<Option<String>> {
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.driver(interface),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(_) => {
                let _ = interface;
                Err(Error::new(
                    ErrorKind::Unsupported,
                    "the bound driver is read on Linux only",
                ))
            }
        }
    }

    /// The speed the open device negotiated (Linux, `USBDEVFS_GET_SPEED`). `None`
    /// when the kernel reports an unknown or wireless speed. Other platforms are
    /// `Unsupported`: use [`UsbDeviceInfo::speed`] from the listing.
    pub fn speed(&self) -> Result<Option<Speed>> {
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => Ok(Speed::from_usbdevfs(usb.speed()?)),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(_) => Err(Error::new(
                ErrorKind::Unsupported,
                "the open device's speed is read on Linux only; use the listing",
            )),
        }
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
                // Named before the claim unbinds it, so the release can bind it again.
                let driver = if detach { usb.driver(number)? } else { None };
                usb.claim_interface(number, detach)?;
                driver
            }
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => {
                // WinUSB and IOKit claim without unbinding anything.
                let _ = detach;
                native.claim_interface(number)?;
                None
            }
        };
        self.lock_claimed().push(number);
        Ok(Interface {
            device: Arc::clone(&self.inner),
            number,
            detached,
        })
    }

    fn lock_claimed(&self) -> MutexGuard<'_, Vec<u8>> {
        self.inner
            .claimed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Refuses a transfer on an interface this handle has not claimed. The kernel
    /// refuses it too (EBUSY), but only after the request reached usbfs; this says
    /// what to do.
    fn require_claimed(&self, interface: u8) -> Result<()> {
        claimed_check(&self.lock_claimed(), interface)
    }

    /// The claim check for a control request: an interface-recipient request is
    /// addressed to the interface in the low byte of `wIndex`.
    fn check_control(&self, setup: &ControlSetup) -> Result<()> {
        match setup.recipient {
            crate::types::ControlRecipient::Interface => {
                self.require_claimed((setup.index & 0xff) as u8)
            }
            _ => Ok(()),
        }
    }

    /// The claim check for a transfer on `endpoint`: its interface, as the active
    /// descriptors name it, must be claimed. An endpoint no descriptor names is
    /// left to the kernel.
    fn check_endpoint_claimed(&self, endpoint: u8) -> Result<()> {
        match endpoint_interface(&self.inner.configurations, endpoint) {
            Some(interface) => self.require_claimed(interface),
            None => Ok(()),
        }
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
        self.check_control(setup)?;
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

    /// A control IN transfer into `data` (its length is the most it asks for);
    /// returns the bytes received. The buffer is the caller's either way.
    pub(crate) fn control_in_into(
        &self,
        setup: &ControlSetup,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize> {
        check_direction(setup.direction, Direction::In, "control read")?;
        self.check_control(setup)?;
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.control_in(setup, data, timeout),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => {
                let received = native.control_in(setup, data.len() as u16, timeout)?;
                let n = received.len().min(data.len());
                data[..n].copy_from_slice(&received[..n]);
                Ok(n)
            }
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
        self.check_control(setup)?;
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
        self.check_endpoint_claimed(endpoint)?;
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
        self.check_endpoint_claimed(endpoint)?;
        // An empty payload is a zero-length packet (the terminator some devices
        // read to: see `BulkWriter::flush_end`).
        // See `read`: usbfs ignores the transfer type.
        let _ = transfer;
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.bulk_out(endpoint, data, timeout),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => native.transfer_out(endpoint, transfer, data, timeout),
        }
    }

    /// Starts a control IN transfer of up to `length` bytes. The transfer runs
    /// on its own thread, bounded by `timeout`, and can be awaited like a bulk
    /// transfer. It cannot be cancelled: `cancel` does nothing for it.
    pub fn submit_control_in(
        &self,
        setup: ControlSetup,
        length: u16,
        timeout: Duration,
    ) -> Result<Transfer> {
        check_direction(setup.direction, Direction::In, "control read")?;
        let device = self.clone();
        Transfer::from_worker(move || {
            let mut data = vec![0; usize::from(length)];
            let (actual_length, status) = match device.control_in_into(&setup, &mut data, timeout) {
                Ok(received) => (received, TransferStatus::Ok),
                Err(error) => (0, status_of(&error)),
            };
            Ok(finished(
                TransferBuffer::from_vec(data),
                actual_length,
                status,
            ))
        })
    }

    /// Starts a control OUT transfer of `data`. The transfer runs on its own
    /// thread, as for [`UsbDevice::submit_control_in`]; the outcome's buffer is
    /// `data`, back for reuse.
    pub fn submit_control_out(
        &self,
        setup: ControlSetup,
        data: Vec<u8>,
        timeout: Duration,
    ) -> Result<Transfer> {
        check_direction(setup.direction, Direction::Out, "control write")?;
        let device = self.clone();
        Transfer::from_worker(move || {
            let (sent, status) = match device.control_out(&setup, &data, timeout) {
                Ok(sent) => (sent, TransferStatus::Ok),
                Err(error) => (0, status_of(&error)),
            };
            Ok(finished(TransferBuffer::from_vec(data), sent, status))
        })
    }

    /// The `wMaxPacketSize` of `endpoint` as the descriptors give it (the first
    /// alternate setting that has it), or `None` when no configuration does.
    pub fn endpoint_max_packet_size(&self, endpoint: u8) -> Option<u16> {
        self.inner
            .configurations
            .iter()
            .flat_map(|configuration| configuration.interfaces.iter())
            .flat_map(|interface| interface.alternates.iter())
            .flat_map(|setting| setting.endpoints.iter())
            .find(|info| info.address == endpoint)
            .map(|info| info.max_packet_size)
    }

    /// Chooses who reaps this device's completions. By default a reaper thread
    /// does it. With `on`, no thread is started and the caller reaps with
    /// [`UsbDevice::reap_now`] or [`UsbDevice::reap_until`], on any thread it
    /// likes (a ring, a poll loop). Set it before submitting: it is refused while
    /// transfers are in flight. Linux only; other platforms reap on their own
    /// threads and refuse `on`.
    pub fn set_caller_driven_reaping(&self, on: bool) -> Result<()> {
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.set_caller_driven(on),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(_) => {
                if on {
                    Err(Error::new(
                        ErrorKind::Unsupported,
                        "caller-driven reaping is Linux only",
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Delivers every completion the kernel has ready, without waiting; returns
    /// how many. Caller-driven reaping only (see
    /// [`UsbDevice::set_caller_driven_reaping`]); the transfers waiting on them
    /// complete.
    pub fn reap_now(&self) -> Result<usize> {
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.reap_now(),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(_) => Err(Error::new(
                ErrorKind::Unsupported,
                "caller-driven reaping is Linux only",
            )),
        }
    }

    /// Waits until a completion is ready or `deadline` passes, then delivers the
    /// ready completions; returns how many (zero at the deadline). Caller-driven
    /// reaping only, as [`UsbDevice::reap_now`].
    pub fn reap_until(&self, deadline: std::time::Instant) -> Result<usize> {
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.reap_until(deadline),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(_) => {
                let _ = deadline;
                Err(Error::new(
                    ErrorKind::Unsupported,
                    "caller-driven reaping is Linux only",
                ))
            }
        }
    }

    /// Whether the caller reaps this device's completions (see
    /// [`UsbDevice::set_caller_driven_reaping`]).
    #[cfg(target_os = "linux")]
    pub(crate) fn caller_driven(&self) -> bool {
        match &self.inner.backend {
            Backend::Usbfs(usb) => usb.caller_driven(),
        }
    }

    /// A URB ring on an IN endpoint of a claimed interface, and the backend that
    /// holds the ring's place.
    #[cfg(target_os = "linux")]
    pub(crate) fn urb_ring(
        &self,
        config: crate::ring::UrbRingConfig,
    ) -> Result<(lemnos_linux_sys::usbfs::UrbRing, Arc<Usbfs>)> {
        check_endpoint(config.endpoint, Direction::In)?;
        self.check_endpoint_claimed(config.endpoint)?;
        match &self.inner.backend {
            Backend::Usbfs(usb) => Ok((usb.urb_ring(config)?, Arc::clone(usb))),
        }
    }

    /// Gives an outcome's packet table back for reuse (an isochronous reader does).
    #[cfg(target_os = "linux")]
    pub(crate) fn recycle_packets(&self, packets: Vec<IsoPacket>) {
        match &self.inner.backend {
            Backend::Usbfs(usb) => usb.recycle_packets(packets),
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

    /// Starts a bulk OUT transfer of `data` that ends with a zero-length
    /// packet when `data` is a whole number of packets. Linux only: this is
    /// usbdevfs's `URB_ZERO_PACKET`, applied within the one transfer. Elsewhere
    /// it is `Unsupported`; use [`BulkWriter::flush_end`] there.
    pub fn submit_bulk_out_zlp(&self, endpoint: u8, data: Vec<u8>) -> Result<Transfer> {
        check_endpoint(endpoint, Direction::Out)?;
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => usb.submit(
                lemnos_linux_sys::usbfs::UrbKind::Bulk,
                endpoint,
                crate::buffer::heap(data),
                &[],
                true,
            ),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(_) => {
                let _ = data;
                Err(Error::new(
                    ErrorKind::Unsupported,
                    "URB_ZERO_PACKET is Linux only; use BulkWriter::flush_end",
                ))
            }
        }
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
        self.submit_buffer(
            endpoint,
            transfer,
            direction,
            TransferBuffer::from_vec(buffer),
        )
    }

    /// Memory for a transfer of `len` bytes. On Linux, a usbdevfs mapping when
    /// the kernel offers one (page-aligned, used by the host controller as is);
    /// otherwise, and when a mapping fails, a heap vector.
    pub fn allocate_buffer(&self, len: usize) -> TransferBuffer {
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => crate::buffer::wrap(usb.allocate(len)),
            #[cfg(not(target_os = "linux"))]
            Backend::Native(_) => TransferBuffer::from_vec(vec![0; len]),
        }
    }

    /// Starts a bulk IN transfer into a buffer from [`UsbDevice::allocate_buffer`]
    /// (or any buffer): its length is the most it asks for.
    pub fn submit_bulk_in_buffer(&self, endpoint: u8, buffer: TransferBuffer) -> Result<Transfer> {
        self.submit_buffer(endpoint, TransferType::Bulk, Direction::In, buffer)
    }

    /// Starts a bulk OUT transfer of a buffer's bytes ([`TransferBuffer::len`]).
    pub fn submit_bulk_out_buffer(&self, endpoint: u8, buffer: TransferBuffer) -> Result<Transfer> {
        self.submit_buffer(endpoint, TransferType::Bulk, Direction::Out, buffer)
    }

    /// The submission every async transfer goes through.
    pub(crate) fn submit_buffer(
        &self,
        endpoint: u8,
        transfer: TransferType,
        direction: Direction,
        buffer: TransferBuffer,
    ) -> Result<Transfer> {
        check_endpoint(endpoint, direction)?;
        self.check_endpoint_claimed(endpoint)?;
        match &self.inner.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => {
                let kind = crate::linux::urb_kind(transfer).ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        "control transfers are not submitted",
                    )
                })?;
                usb.submit(kind, endpoint, crate::buffer::unwrap(buffer), &[], false)
            }
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => {
                native.submit(endpoint, transfer, direction, buffer.into_vec())
            }
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
        self.submit_isochronous_in_buffer(endpoint, TransferBuffer::from_vec(buffer), packet_len)
    }

    /// As [`UsbDevice::submit_isochronous_in`], into a buffer from
    /// [`UsbDevice::allocate_buffer`].
    pub fn submit_isochronous_in_buffer(
        &self,
        endpoint: u8,
        buffer: TransferBuffer,
        packet_len: usize,
    ) -> Result<Transfer> {
        if packet_len == 0 || buffer.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "isochronous transfers need a packet length and a buffer",
            ));
        }
        let lengths = iso_packet_lengths(buffer.len(), packet_len);
        self.submit_isochronous_packets(endpoint, buffer, &lengths)
    }

    /// The isochronous submission: one packet per entry of `lengths`, which
    /// sum to the buffer's length. Streams call this with lengths they keep, so
    /// a steady stream allocates no lengths.
    #[cfg(target_os = "linux")]
    pub(crate) fn submit_isochronous_packets(
        &self,
        endpoint: u8,
        buffer: TransferBuffer,
        lengths: &[u32],
    ) -> Result<Transfer> {
        check_endpoint(endpoint, Direction::In)?;
        self.check_endpoint_claimed(endpoint)?;
        match &self.inner.backend {
            Backend::Usbfs(usb) => usb.submit(
                lemnos_linux_sys::usbfs::UrbKind::Isochronous,
                endpoint,
                crate::buffer::unwrap(buffer),
                lengths,
                false,
            ),
        }
    }

    /// The isochronous submission on other platforms: refused.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn submit_isochronous_packets(
        &self,
        endpoint: u8,
        buffer: TransferBuffer,
        lengths: &[u32],
    ) -> Result<Transfer> {
        let _ = (endpoint, buffer, lengths);
        Err(Error::new(
            ErrorKind::Unsupported,
            "isochronous transfers are supported on Linux only",
        ))
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

    /// An isochronous IN reader over `endpoint`: `depth` transfers in flight,
    /// each of `packets` packets of `packet_len` bytes. Linux only.
    #[cfg(target_os = "linux")]
    pub fn isochronous_reader(
        &self,
        endpoint: u8,
        packet_len: usize,
        packets: usize,
        depth: usize,
    ) -> Result<crate::stream::IsoReader> {
        check_endpoint(endpoint, Direction::In)?;
        crate::stream::IsoReader::new(self.clone(), endpoint, packet_len, packets, depth)
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

/// The usbdevfs file, for polling: `POLLOUT` when a URB has completed, `POLLHUP`
/// when the device left. Completions are per open file, so a second descriptor
/// on the device would not see them.
#[cfg(target_os = "linux")]
impl std::os::fd::AsFd for UsbDevice {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        match &self.inner.backend {
            Backend::Usbfs(usb) => usb.as_fd(),
        }
    }
}

/// A claimed interface. Releasing it (dropping it) lets others claim it, and
/// hands it back to the kernel driver if the claim detached one.
pub struct Interface {
    device: Arc<Inner>,
    number: u8,
    // The driver a detaching claim unbound (Linux only); the release binds it again.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    detached: Option<String>,
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
        {
            let mut claimed = self
                .device
                .claimed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(index) = claimed.iter().position(|&n| n == self.number) {
                claimed.swap_remove(index);
            }
        }
        match &self.device.backend {
            #[cfg(target_os = "linux")]
            Backend::Usbfs(usb) => {
                let _ = usb.release_interface(self.number, self.detached.is_some());
                // USBDEVFS_IOCTL CONNECT does not rebind an interface after a
                // usbfs release on the kernel tested (see docs/usb.md): bind the
                // driver the claim unbound through sysfs, when it is still unbound.
                if let Some(driver) = &self.detached
                    && matches!(usb.driver(self.number), Ok(None))
                {
                    let _ = rebind_sysfs(
                        &self.device.info.location().to_string(),
                        self.number,
                        driver,
                    );
                }
            }
            #[cfg(not(target_os = "linux"))]
            Backend::Native(native) => native.release_interface(self.number),
        }
    }
}

/// Binds `driver` to interface `number` of the device at `location` through
/// sysfs (`/sys/bus/usb/drivers/<driver>/bind`), named `<location>:<config>.<number>`.
#[cfg(target_os = "linux")]
fn rebind_sysfs(location: &str, number: u8, driver: &str) -> std::io::Result<()> {
    let config = std::fs::read_to_string(format!(
        "/sys/bus/usb/devices/{location}/bConfigurationValue"
    ))?;
    let name = format!("{location}:{}.{number}", config.trim());
    std::fs::write(format!("/sys/bus/usb/drivers/{driver}/bind"), name)
}

/// Whether `interface` is among the `claimed` interfaces.
pub(crate) fn claimed_check(claimed: &[u8], interface: u8) -> Result<()> {
    if claimed.contains(&interface) {
        return Ok(());
    }
    Err(Error::new(
        ErrorKind::Busy,
        format!(
            "interface {interface} is not claimed by this handle: claim it (claim_interface or detach_and_claim_interface) before a transfer on it"
        ),
    ))
}

/// The interface number an endpoint belongs to, from the configurations' descriptors.
pub(crate) fn endpoint_interface(configurations: &[ConfigurationInfo], endpoint: u8) -> Option<u8> {
    configurations
        .iter()
        .flat_map(|configuration| configuration.interfaces.iter())
        .find(|interface| {
            interface
                .alternates
                .iter()
                .any(|setting| setting.endpoints.iter().any(|e| e.address == endpoint))
        })
        .map(|interface| interface.number)
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

/// A worker transfer's outcome: its buffer back with the status (a failed control
/// transfer keeps its buffer too).
fn finished(
    buffer: TransferBuffer,
    actual_length: usize,
    status: TransferStatus,
) -> TransferOutcome {
    TransferOutcome {
        buffer,
        actual_length,
        packets: Vec::new(),
        status,
        reaped: std::time::Instant::now(),
    }
}

/// The status a failed synchronous call stands for, for an outcome.
fn status_of(error: &Error) -> TransferStatus {
    match error.kind() {
        ErrorKind::Stall => TransferStatus::Stalled,
        ErrorKind::Cancelled => TransferStatus::Cancelled,
        ErrorKind::Disconnected => TransferStatus::Disconnected,
        ErrorKind::Timeout => TransferStatus::Error(libc::ETIMEDOUT),
        _ => TransferStatus::Error(libc::EIO),
    }
}

/// The packet lengths of an isochronous transfer of `total` bytes in packets of
/// `packet_len` (the last packet takes the rest). `total` is non-zero.
pub(crate) fn iso_packet_lengths(total: usize, packet_len: usize) -> Vec<u32> {
    let packets = total.div_ceil(packet_len);
    let mut lengths = vec![packet_len as u32; packets];
    if let Some(last) = lengths.last_mut() {
        *last = (total - (packets - 1) * packet_len) as u32;
    }
    lengths
}
