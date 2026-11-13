//! An opened device: the transfers.

use std::time::{Duration, Instant};

use lemnos_hal::ErrorKind;
use lemnos_hal::UsbDevice;
use lemnos_hal::blocks::{ControlSetup, IsoPacket, iso_shape_ok};
use lemnos_linux_sys::usbfs::{self, Readiness, RingCompletion, RingConfig, UrbRing, UsbfsFile};

use crate::descriptors;
use crate::error::{kind_of, kind_of_errno, timeout_ms};

/// How long stopping a stream waits for the controller to give its transfers back.
const CANCEL_GRACE: Duration = Duration::from_millis(100);

/// A started isochronous stream: its ring, and the shape of each transfer.
#[derive(Debug)]
struct IsoStream {
    ring: UrbRing,
    endpoint: u8,
    packets: usize,
    packet_bytes: u32,
    /// Each packet's length as usbfs takes it, all `packet_bytes`.
    lengths: [u32; MAX_ISO_PACKETS],
}

impl IsoStream {
    /// Submits every URB of the ring that is not with the kernel: one just reaped, one that
    /// could not go again before, or one whose completion could not be believed.
    fn refill(&mut self) -> Result<(), ErrorKind> {
        let lengths = &self.lengths[..self.packets];
        for slot in 0..self.ring.config().count {
            if !self.ring.is_in_flight(slot) {
                self.ring
                    .submit(slot, self.endpoint, lengths)
                    .map_err(|e| kind_of(&e))?;
            }
        }
        Ok(())
    }
}

/// A USB device opened through usbfs. Dropping it closes the file, and the kernel releases its
/// claims.
#[derive(Debug)]
pub struct UsbfsDevice {
    file: UsbfsFile,
    iso: Option<IsoStream>,
}

impl UsbfsDevice {
    /// A device on an open usbfs file.
    pub fn from_file(file: UsbfsFile) -> Self {
        Self { file, iso: None }
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

    /// A ring of `transfers` URBs, each submitted at once with `URB_ISO_ASAP`, so the kernel
    /// schedules each in the frames after the one before it.
    fn iso_in_start(
        &mut self,
        endpoint: u8,
        transfers: u8,
        packets: u16,
        packet_bytes: u32,
    ) -> Result<(), ErrorKind> {
        if !iso_shape_ok(transfers, packets, packet_bytes) || endpoint & 0x80 == 0 {
            return Err(ErrorKind::InvalidInput);
        }
        if self.iso.is_some() {
            return Err(ErrorKind::Busy);
        }
        let packets = usize::from(packets);
        let config = RingConfig {
            count: usize::from(transfers),
            buffer_len: packets
                .checked_mul(packet_bytes as usize)
                .ok_or(ErrorKind::InvalidInput)?,
        };
        let ring = self.file.urb_ring(config).map_err(|e| kind_of(&e))?;
        let mut stream = IsoStream {
            ring,
            endpoint,
            packets,
            packet_bytes,
            lengths: [packet_bytes; MAX_ISO_PACKETS],
        };
        for slot in 0..config.count {
            let lengths = &stream.lengths[..packets];
            if let Err(error) = stream.ring.submit(slot, endpoint, lengths) {
                // Dropping the ring takes back what was submitted.
                return Err(kind_of(&error));
            }
        }
        self.iso = Some(stream);
        Ok(())
    }

    fn iso_in_next(
        &mut self,
        data: &mut [u8],
        packets: &mut [IsoPacket],
        timeout: Duration,
    ) -> Result<(), ErrorKind> {
        let stream = self.iso.as_mut().ok_or(ErrorKind::InvalidInput)?;
        let room = stream.packets * stream.packet_bytes as usize;
        if packets.len() != stream.packets || data.len() < room {
            return Err(ErrorKind::InvalidInput);
        }
        for packet in packets.iter_mut() {
            *packet = IsoPacket {
                capacity: stream.packet_bytes,
                received: 0,
                ok: false,
            };
        }
        stream.refill()?;
        let wait = Duration::from_millis(u64::from(timeout_ms(timeout)));
        let deadline = Instant::now()
            .checked_add(wait)
            .ok_or(ErrorKind::InvalidInput)?;
        let delivered = loop {
            // The completion may already be there, so reap before waiting.
            match stream.ring.reap() {
                Ok(Some(done)) => break deliver(done, stream.packet_bytes, data, packets),
                Ok(None) => {}
                // A completion that can't be true: its URB is the ring's again, and goes back.
                Err(error) => {
                    let kind = kind_of(&error);
                    let _ = stream.refill();
                    return Err(kind);
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ErrorKind::Timeout);
            }
            match stream.ring.wait(remaining).map_err(|e| kind_of(&e))? {
                Readiness::Gone => return Err(ErrorKind::Unavailable),
                Readiness::Idle | Readiness::Completion => {}
            }
        };
        // Back to the controller at once, whatever this one brought. One that can't go now
        // goes at the next call, unless the device has gone.
        if let Err(ErrorKind::Unavailable) = stream.refill() {
            return Err(ErrorKind::Unavailable);
        }
        delivered
    }

    fn iso_in_stop(&mut self) -> Result<(), ErrorKind> {
        let Some(mut stream) = self.iso.take() else {
            return Ok(());
        };
        // What the kernel still holds after this, the ring's own drop waits for longer, and
        // leaks rather than free memory the kernel may write.
        stream.ring.cancel(CANCEL_GRACE);
        drop(stream);
        Ok(())
    }
}

/// Copies a completed URB's packets into the caller's buffers: packet `i` at `i *
/// packet_bytes`, so a short packet leaves the rest of its room as it was.
fn deliver(
    done: RingCompletion<'_>,
    packet_bytes: u32,
    data: &mut [u8],
    packets: &mut [IsoPacket],
) -> Result<(), ErrorKind> {
    for packet in packets.iter_mut() {
        *packet = IsoPacket {
            capacity: packet_bytes,
            received: 0,
            ok: false,
        };
    }
    // A failure of a single packet is in that packet. These end the whole URB.
    match done.status.wrapping_neg() {
        libc::ENOENT | libc::ECONNRESET => return Err(ErrorKind::Failed),
        errno @ (libc::ENODEV | libc::ESHUTDOWN) => return Err(kind_of_errno(errno)),
        _ => {}
    }
    let size = packet_bytes as usize;
    for ((out, packet), room) in packets
        .iter_mut()
        .zip(done.packets())
        .zip(data.chunks_mut(size))
    {
        room.get_mut(..packet.data.len())
            .ok_or(ErrorKind::Failed)?
            .copy_from_slice(packet.data);
        out.received = packet.data.len() as u32;
        out.ok = packet.status == 0;
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

/// The most packets a stream's transfer has, as the HAL allows.
const MAX_ISO_PACKETS: usize = lemnos_hal::blocks::MAX_ISO_PACKETS as usize;
const _: () = assert!(MAX_ISO_PACKETS <= usbfs::MAX_ISO_PACKETS);

const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<UsbfsDevice>();
};
