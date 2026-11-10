//! An opened device: the transfers.

use std::time::{Duration, Instant};

use lemnos_hal::ErrorKind;
use lemnos_hal::UsbDevice;
use lemnos_hal::blocks::{ControlSetup, IsoPacket};
use lemnos_linux_sys::usbfs::{
    self, MAX_ISO_PACKETS, Readiness, RingCompletion, RingConfig, UrbRing, UsbfsFile,
};

use crate::descriptors;
use crate::error::{kind_of, kind_of_errno, timeout_ms};

/// How long a timed-out isochronous transfer is given to come back after its cancel.
const CANCEL_GRACE: Duration = Duration::from_millis(100);

/// A USB device opened through usbfs. Dropping it closes the file, and the kernel releases its
/// claims.
#[derive(Debug)]
pub struct UsbfsDevice {
    file: UsbfsFile,
    /// One URB, shaped to the largest transfer seen so far, so a steady stream allocates nothing.
    iso: Option<UrbRing>,
}

impl UsbfsDevice {
    /// A device on an open usbfs file.
    pub fn from_file(file: UsbfsFile) -> Self {
        Self { file, iso: None }
    }

    /// The URB ring for a transfer of `total` bytes, with nothing left in flight. A URB the kernel
    /// still holds from an earlier timeout gets another cancel, and while it stays there, this is
    /// `Busy`: the memory the kernel holds can't be reused or freed.
    fn iso_ring(&mut self, total: usize) -> Result<&mut UrbRing, ErrorKind> {
        if let Some(ring) = &mut self.iso
            && ring.in_flight() > 0
            && ring.cancel(CANCEL_GRACE) > 0
        {
            return Err(match ring.wait(Duration::ZERO) {
                Ok(Readiness::Gone) => ErrorKind::Unavailable,
                _ => ErrorKind::Busy,
            });
        }
        if self
            .iso
            .as_ref()
            .is_none_or(|ring| ring.config().buffer_len < total)
        {
            let config = RingConfig {
                count: 1,
                buffer_len: total,
            };
            self.iso = Some(self.file.urb_ring(config).map_err(|e| kind_of(&e))?);
        }
        self.iso.as_mut().ok_or(ErrorKind::Failed)
    }
}

impl UsbDevice for UsbfsDevice {
    type Error = ErrorKind;

    /// Copies as much of the configuration as fits in `out` and returns the bytes copied.
    fn config_descriptor(&mut self, index: u8, out: &mut [u8]) -> Result<usize, ErrorKind> {
        descriptors::config_descriptor(&self.file, index, out)
    }

    /// The kernel's port reset. When the device comes back with the descriptors it had, the
    /// kernel keeps this file's claims and puts the alternate settings back. When it comes back
    /// different, or was already gone, the kernel disconnects it: this is `Unavailable`, and so
    /// is every call after it.
    fn reset(&mut self) -> Result<(), ErrorKind> {
        self.file.reset().map_err(|e| kind_of(&e))
    }

    /// Unbinds the kernel driver from `interface`. An interface held through usbfs, by this
    /// device or another process, is left as it is. The kernel does not bind the driver again
    /// when the interface is released or this device dropped.
    fn detach_kernel_driver(&mut self, interface: u8) -> Result<(), ErrorKind> {
        self.file
            .detach_kernel_driver(interface)
            .map(drop)
            .map_err(|e| kind_of(&e))
    }

    /// Claims `interface` without unbinding a kernel driver. One that holds it makes this `Busy`.
    fn claim_interface(&mut self, interface: u8) -> Result<(), ErrorKind> {
        self.file
            .claim_interface(interface)
            .map_err(|e| kind_of(&e))
    }

    fn release_interface(&mut self, interface: u8) -> Result<(), ErrorKind> {
        self.file
            .release_interface(interface)
            .map_err(|e| kind_of(&e))
    }

    fn set_alternate(&mut self, interface: u8, alternate: u8) -> Result<(), ErrorKind> {
        self.file
            .set_interface(interface, alternate)
            .map_err(|e| kind_of(&e))
    }

    fn control_in(
        &mut self,
        setup: ControlSetup,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.file
            .control_in(native_setup(setup), data, timeout_ms(timeout))
            .map_err(|e| kind_of(&e))
    }

    fn control_out(
        &mut self,
        setup: ControlSetup,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.file
            .control_out(native_setup(setup), data, timeout_ms(timeout))
            .map_err(|e| kind_of(&e))
    }

    fn bulk_in(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.file
            .bulk_in(endpoint, data, timeout_ms(timeout))
            .map_err(|e| kind_of(&e))
    }

    fn bulk_out(
        &mut self,
        endpoint: u8,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.file
            .bulk_out(endpoint, data, timeout_ms(timeout))
            .map_err(|e| kind_of(&e))
    }

    /// usbfs's bulk call also serves interrupt endpoints.
    fn interrupt_in(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.bulk_in(endpoint, data, timeout)
    }

    fn iso_in(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        packets: &mut [IsoPacket],
        timeout: Duration,
    ) -> Result<(), ErrorKind> {
        if packets.is_empty() || packets.len() > MAX_ISO_PACKETS {
            return Err(ErrorKind::InvalidInput);
        }
        let mut lengths = [0u32; MAX_ISO_PACKETS];
        let mut total = 0usize;
        for (length, packet) in lengths.iter_mut().zip(packets.iter_mut()) {
            *length = packet.capacity;
            total = total
                .checked_add(packet.capacity as usize)
                .ok_or(ErrorKind::InvalidInput)?;
            packet.received = 0;
            packet.ok = false;
        }
        if total == 0 || total > data.len() {
            return Err(ErrorKind::InvalidInput);
        }
        let wait = Duration::from_millis(u64::from(timeout_ms(timeout)));
        let deadline = Instant::now()
            .checked_add(wait)
            .ok_or(ErrorKind::InvalidInput)?;
        let lengths = &lengths[..packets.len()];

        let ring = self.iso_ring(total)?;
        ring.submit(0, endpoint, lengths).map_err(|e| kind_of(&e))?;
        loop {
            // The completion may already be there, so reap before waiting.
            if let Some(done) = ring.reap().map_err(|e| kind_of(&e))? {
                return deliver(done, data, packets);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                ring.cancel(CANCEL_GRACE);
                return Err(ErrorKind::Timeout);
            }
            match ring.wait(remaining).map_err(|e| kind_of(&e))? {
                Readiness::Gone => return Err(ErrorKind::Unavailable),
                Readiness::Idle | Readiness::Completion => {}
            }
        }
    }
}

/// Copies a completed URB's packets into the caller's buffers. Each packet goes where its
/// capacity put it, so a short packet leaves its gap.
fn deliver(
    done: RingCompletion<'_>,
    data: &mut [u8],
    packets: &mut [IsoPacket],
) -> Result<(), ErrorKind> {
    // A failure of a single packet is in that packet. These end the whole URB.
    match done.status.wrapping_neg() {
        libc::ENOENT | libc::ECONNRESET => return Err(ErrorKind::Failed),
        errno @ (libc::ENODEV | libc::ESHUTDOWN) => return Err(kind_of_errno(errno)),
        _ => {}
    }
    let mut offset = 0usize;
    for (out, packet) in packets.iter_mut().zip(done.packets()) {
        let end = offset
            .checked_add(packet.data.len())
            .ok_or(ErrorKind::Failed)?;
        data.get_mut(offset..end)
            .ok_or(ErrorKind::Failed)?
            .copy_from_slice(packet.data);
        out.received = packet.data.len() as u32;
        out.ok = packet.status == 0;
        offset = offset
            .checked_add(out.capacity as usize)
            .ok_or(ErrorKind::Failed)?;
    }
    Ok(())
}

fn native_setup(setup: ControlSetup) -> usbfs::ControlSetup {
    usbfs::ControlSetup {
        request_type: setup.request_type,
        request: setup.request,
        value: setup.value,
        index: setup.index,
    }
}

const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<UsbfsDevice>();
};
