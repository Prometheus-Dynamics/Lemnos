//! A ring of URBs for streams that must not allocate: the URBs, packet tables
//! and buffers are allocated once, and the caller submits and reaps by slot
//! from its own loop, with no reaper thread. Linux only.

use crate::device::UsbDevice;
use crate::error::{Error, Result};
use crate::linux::{Usbfs, urb_status};
use crate::transfer::TransferStatus;
use lemnos_linux_sys::usbfs::{self, RingCompletion, RingPacket};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use lemnos_linux_sys::usbfs::RingKind as UrbRingKind;
pub use lemnos_linux_sys::usbfs::{MAX_ISO_PACKETS, RingConfig as UrbRingConfig};

/// One isochronous packet of a completed URB.
#[derive(Debug, Clone, Copy)]
pub struct UrbPacket<'a> {
    /// How the packet ended.
    pub status: TransferStatus,
    /// The bytes received.
    pub data: &'a [u8],
}

/// A completed URB, borrowed from its ring until the next ring call.
#[derive(Debug)]
pub struct UrbCompletion<'a> {
    /// How the URB ended. An isochronous URB is `Ok` unless it ended as a whole:
    /// its packets carry their own status.
    pub status: TransferStatus,
    raw: RingCompletion<'a>,
}

impl<'a> UrbCompletion<'a> {
    /// Which URB of the ring: resubmit it with [`UrbRing::submit`].
    pub fn slot(&self) -> usize {
        self.raw.slot
    }

    /// The bytes received (isochronous: all packets together).
    pub fn actual_length(&self) -> usize {
        self.raw.actual_length
    }

    /// Isochronous packets that failed.
    pub fn error_count(&self) -> u32 {
        self.raw.error_count
    }

    /// When it was reaped.
    pub fn reaped(&self) -> Instant {
        self.raw.reaped
    }

    /// A bulk URB's bytes.
    pub fn data(&self) -> &'a [u8] {
        self.raw.data()
    }

    /// An isochronous URB's packets, in bus order (none for bulk).
    pub fn packets(&self) -> impl Iterator<Item = UrbPacket<'a>> + 'a {
        self.raw
            .packets()
            .map(|RingPacket { status, data }| UrbPacket {
                status: TransferStatus::from_kernel(status),
                data,
            })
    }

    /// Packets in this URB (0 for bulk).
    pub fn packet_count(&self) -> usize {
        self.raw.packet_count()
    }
}

/// Gives the device's reaping back when the ring has gone.
struct RingClaim(Arc<Usbfs>);

impl Drop for RingClaim {
    fn drop(&mut self) {
        self.0.release_ring();
    }
}

/// URBs and their buffers on one IN endpoint of a [`UsbDevice`].
///
/// The ring reaps the device's completions itself, so a device has at most one
/// ring, and while it exists the device's asynchronous transfers and
/// caller-driven reaping are refused (`Busy`); synchronous control, bulk and
/// interrupt calls carry on. Wait for completions by polling the device
/// ([`UsbDevice`] is `AsFd`) for `POLLOUT`, and `POLLHUP` for a device that left.
///
/// Dropping the ring cancels and reaps everything in flight. Memory the kernel
/// does not give back within a few seconds is leaked rather than freed.
pub struct UrbRing {
    // Dropped in this order: the claim outlives the URBs.
    ring: usbfs::UrbRing,
    device: UsbDevice,
    _claim: RingClaim,
}

impl std::fmt::Debug for UrbRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.ring.fmt(f)
    }
}

impl UrbRing {
    /// Allocates `config.count` URBs for `config.endpoint`; nothing is submitted
    /// yet. The endpoint's interface must be claimed, and no asynchronous
    /// transfer in flight.
    pub fn new(device: &UsbDevice, config: UrbRingConfig) -> Result<Self> {
        let (ring, usb) = device.urb_ring(config)?;
        Ok(Self {
            ring,
            device: device.clone(),
            _claim: RingClaim(usb),
        })
    }

    /// The ring's shape.
    pub fn config(&self) -> UrbRingConfig {
        self.ring.config()
    }

    /// The device.
    pub fn device(&self) -> &UsbDevice {
        &self.device
    }

    /// Whether the buffers are mapped from usbfs (no copy at reap).
    pub fn is_mapped(&self) -> bool {
        self.ring.is_mapped()
    }

    /// URBs submitted and not yet reaped.
    pub fn in_flight(&self) -> usize {
        self.ring.in_flight()
    }

    /// Submits URB `slot`; a URB already in flight is left alone.
    pub fn submit(&mut self, slot: usize) -> Result<()> {
        self.ring.submit(slot).map_err(submit_failed)
    }

    /// Submits every idle URB.
    pub fn submit_all(&mut self) -> Result<()> {
        self.ring.submit_all().map_err(submit_failed)
    }

    /// Takes one completed URB if there is one; never blocks. The URB stays idle
    /// until [`UrbRing::submit`]. An error of kind `Disconnected` means the
    /// device went away and nothing completed is left.
    pub fn reap(&mut self) -> Result<Option<UrbCompletion<'_>>> {
        let done = self
            .ring
            .reap()
            .map_err(|error| Error::from_io("reaping a URB", &error))?;
        Ok(done.map(|raw| UrbCompletion {
            status: urb_status(raw.status, raw.packet_count() > 0),
            raw,
        }))
    }

    /// The first `filled` bytes of idle URB `slot`, as its last completion left
    /// them (never more than a bulk completion wrote).
    pub fn idle_data(&self, slot: usize, filled: usize) -> &[u8] {
        self.ring.idle_data(slot, filled)
    }

    /// Takes the bytes of idle bulk URB `slot`, `filled` of them, and gives it
    /// `empty` to fill next, with no copy. Refused, with `empty` back, on mapped
    /// buffers, isochronous rings, a URB in flight, `filled` past what its last
    /// completion wrote, and unless `empty`'s capacity is exactly the URB's
    /// buffer length.
    pub fn exchange_buffer(
        &mut self,
        slot: usize,
        filled: usize,
        empty: Vec<u8>,
    ) -> std::result::Result<Vec<u8>, Vec<u8>> {
        self.ring.exchange_buffer(slot, filled, empty)
    }

    /// Discards every URB in flight and reaps them, waiting up to `timeout`.
    /// Returns how many could not be reaped.
    pub fn cancel(&mut self, timeout: Duration) -> usize {
        self.ring.cancel(timeout)
    }
}

fn submit_failed(error: std::io::Error) -> Error {
    Error::from_io("submitting a URB", &error)
}
