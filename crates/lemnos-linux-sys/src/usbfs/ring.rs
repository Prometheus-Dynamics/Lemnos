//! A ring of URBs on one IN endpoint: the URB structs, the packet tables and
//! the buffers are allocated once, and a steady stream of submissions and
//! completions allocates nothing.
//!
//! The ring reaps straight from the device file, so nothing else may submit or
//! reap on that file while it exists: a completion goes to whoever reaps first.
//! The caller keeps that promise (`lemnos-usb` refuses its own transfers while
//! a ring is up); a URB this ring did not submit is reported as an error.

use super::{
    DISCARDURB, ISO_DESC_LEN, IsoPacketDesc, REAPURBNDELAY, SUBMITURB, URB_ISO_ASAP, URB_TYPE_BULK,
    URB_TYPE_ISO, UrbHeader, mapping_len, page_size,
};
use crate::ioctl::{ioctl_int, ioctl_ptr};
use crate::mmap::Mapping;
use crate::poll::{POLLOUT, poll_one};
use std::ffi::c_void;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::ptr::NonNull;
use std::time::{Duration, Instant};

/// The most packets one isochronous URB may carry (usbfs's limit).
pub const MAX_ISO_PACKETS: usize = 128;

/// How long a cancel sleeps at most between two looks at the completions.
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// What one URB of a ring moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingKind {
    /// Isochronous: `packets` (1 to [`MAX_ISO_PACKETS`]) packets of up to
    /// `packet_size` bytes each.
    Iso {
        /// Packets per URB.
        packets: usize,
        /// The stride of the packets in the buffer.
        packet_size: usize,
    },
    /// Bulk: one transfer of up to `size` bytes (a short packet ends it early).
    Bulk {
        /// The buffer's length.
        size: usize,
    },
}

/// A ring's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingConfig {
    /// The IN endpoint's address.
    pub endpoint: u8,
    /// What each URB moves.
    pub kind: RingKind,
    /// The URBs in the ring.
    pub count: usize,
    /// Put the buffers in one usbdevfs mapping (`CAP_MMAP`): the host controller
    /// writes them directly and a reap copies nothing. Falls back to the heap
    /// when the mapping fails. Where DMA is not cache-coherent the mapping is
    /// uncached, so reading it costs more.
    pub mmap: bool,
}

/// A URB followed by room for its packet descriptors, as the kernel reads it.
#[repr(C)]
struct UrbStorage {
    header: UrbHeader,
    packets: [IsoPacketDesc; MAX_ISO_PACKETS],
}

const _: () = assert!(size_of::<IsoPacketDesc>() == ISO_DESC_LEN);

/// One isochronous packet of a completed URB.
#[derive(Debug, Clone, Copy)]
pub struct RingPacket<'a> {
    /// Zero, or a negative errno.
    pub status: i32,
    /// The bytes received.
    pub data: &'a [u8],
}

/// A completed URB, borrowed from its ring until the next ring call.
#[derive(Debug)]
pub struct RingCompletion<'a> {
    /// Which URB of the ring.
    pub slot: usize,
    /// Zero, or a negative errno.
    pub status: i32,
    /// The bytes received (isochronous: all packets together).
    pub actual_length: usize,
    /// Isochronous packets that failed.
    pub error_count: u32,
    /// When it was reaped.
    pub reaped: Instant,
    packet_size: usize,
    buffer: &'a [u8],
    packets: &'a [IsoPacketDesc],
}

impl std::fmt::Debug for IsoPacketDesc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IsoPacketDesc")
            .field("actual_length", &self.actual_length)
            .field("status", &self.status)
            .finish()
    }
}

impl<'a> RingCompletion<'a> {
    /// A bulk URB's bytes.
    pub fn data(&self) -> &'a [u8] {
        &self.buffer[..self.actual_length.min(self.buffer.len())]
    }

    /// An isochronous URB's packets, in bus order (none for bulk).
    pub fn packets(&self) -> impl Iterator<Item = RingPacket<'a>> + 'a {
        let (buffer, size) = (self.buffer, self.packet_size);
        self.packets.iter().enumerate().map(move |(index, desc)| {
            let start = (index * size).min(buffer.len());
            let end = (start + desc.actual_length as usize).min(buffer.len());
            RingPacket {
                status: desc.status as i32,
                data: &buffer[start..end],
            }
        })
    }

    /// Packets in this URB (0 for bulk).
    pub fn packet_count(&self) -> usize {
        self.packets.len()
    }
}

struct Slot {
    storage: NonNull<UrbStorage>,
    buffer: NonNull<[u8]>,
    /// The bytes the last bulk completion wrote.
    filled: usize,
    in_flight: bool,
}

/// The URB memory of a ring. Everything here runs without a device; the ring
/// adds the kernel calls.
struct Slots {
    slots: Vec<Slot>,
    kind: RingKind,
    /// The buffers belong to a mapping, not to this table.
    mapped: bool,
}

impl Slots {
    /// Allocates `count` URBs of `len` bytes each: consecutive parts of the
    /// mapping at `mapped`, or heap buffers without one.
    fn new(config: &RingConfig, len: usize, mapped: Option<NonNull<u8>>) -> Self {
        let (kind, flags, packets, packet_size) = match config.kind {
            RingKind::Iso {
                packets,
                packet_size,
            } => (URB_TYPE_ISO, URB_ISO_ASAP, packets, packet_size),
            RingKind::Bulk { .. } => (URB_TYPE_BULK, 0, 0, 0),
        };
        let mut slots = Vec::with_capacity(config.count);
        for index in 0..config.count {
            let buffer = match mapped {
                Some(base) => {
                    // SAFETY: the mapping holds `count * len` bytes from `base`.
                    let start = unsafe { base.add(index * len) };
                    NonNull::slice_from_raw_parts(start, len)
                }
                None => NonNull::from(Box::leak(vec![0u8; len].into_boxed_slice())),
            };
            let mut storage = Box::new(UrbStorage {
                header: UrbHeader {
                    kind,
                    endpoint: config.endpoint,
                    status: 0,
                    flags,
                    buffer: buffer.as_ptr().cast(),
                    buffer_length: len as i32,
                    actual_length: 0,
                    start_frame: 0,
                    number_of_packets: packets as i32,
                    error_count: 0,
                    signr: 0,
                    usercontext: std::ptr::null_mut(),
                },
                packets: [IsoPacketDesc {
                    length: 0,
                    actual_length: 0,
                    status: 0,
                }; MAX_ISO_PACKETS],
            });
            for desc in &mut storage.packets[..packets] {
                desc.length = packet_size as u32;
            }
            slots.push(Slot {
                storage: NonNull::from(Box::leak(storage)),
                buffer,
                filled: 0,
                in_flight: false,
            });
        }
        Self {
            slots,
            kind: config.kind,
            mapped: mapped.is_some(),
        }
    }

    /// Readies idle URB `slot` for the kernel: its address, or `None` when it is
    /// already in flight.
    fn prepare(&mut self, slot: usize) -> io::Result<Option<*mut UrbStorage>> {
        let entry = self
            .slots
            .get_mut(slot)
            .ok_or_else(|| invalid("no such URB in the ring"))?;
        if entry.in_flight {
            return Ok(None);
        }
        let urb = entry.storage.as_ptr();
        // SAFETY: `urb` is this slot's live allocation, and the URB is idle: the
        // kernel holds no reference to it, so this borrow races with nothing.
        let header = unsafe { &mut (*urb).header };
        header.status = 0;
        header.actual_length = 0;
        header.error_count = 0;
        header.start_frame = 0;
        Ok(Some(urb))
    }

    /// Takes the URB the kernel handed back at `urb`.
    fn complete(&mut self, urb: *mut c_void, reaped: Instant) -> io::Result<RingCompletion<'_>> {
        let slot = self
            .slots
            .iter()
            .position(|entry| entry.storage.as_ptr().cast() == urb && entry.in_flight)
            .ok_or_else(|| {
                io::Error::other("the kernel completed a URB this ring did not submit")
            })?;
        let packet_size = match self.kind {
            RingKind::Iso { packet_size, .. } => packet_size,
            RingKind::Bulk { .. } => 0,
        };
        let entry = &mut self.slots[slot];
        entry.in_flight = false;
        // SAFETY: the URB was reaped, so the kernel no longer writes the slot's
        // memory, and it stays idle while the completion borrows the ring.
        let storage = unsafe { entry.storage.as_ref() };
        // SAFETY: as above, for the buffer.
        let buffer = unsafe { entry.buffer.as_ref() };
        let header = &storage.header;
        let count = (header.number_of_packets.max(0) as usize).min(MAX_ISO_PACKETS);
        let packets = &storage.packets[..count];
        let actual_length = header.actual_length.max(0) as usize;
        entry.filled = actual_length.min(buffer.len());
        Ok(RingCompletion {
            slot,
            status: header.status,
            actual_length,
            error_count: header.error_count.max(0) as u32,
            reaped,
            packet_size,
            buffer,
            packets,
        })
    }

    fn in_flight(&self) -> usize {
        self.slots.iter().filter(|entry| entry.in_flight).count()
    }

    fn idle_data(&self, slot: usize, filled: usize) -> &[u8] {
        let Some(entry) = self.slots.get(slot).filter(|entry| !entry.in_flight) else {
            return &[];
        };
        // Only bytes the kernel wrote: an exchanged bulk buffer may start
        // uninitialised. Isochronous buffers are never exchanged.
        let limit = match self.kind {
            RingKind::Iso { .. } => entry.buffer.len(),
            RingKind::Bulk { .. } => entry.filled,
        };
        // SAFETY: the URB is idle, so the kernel holds no reference to its
        // buffer, and the first `limit` bytes are initialised.
        unsafe { std::slice::from_raw_parts(entry.buffer.as_ptr().cast(), filled.min(limit)) }
    }

    fn exchange_buffer(
        &mut self,
        slot: usize,
        filled: usize,
        mut empty: Vec<u8>,
    ) -> Result<Vec<u8>, Vec<u8>> {
        if self.mapped || !matches!(self.kind, RingKind::Bulk { .. }) {
            return Err(empty);
        }
        let Some(entry) = self.slots.get_mut(slot) else {
            return Err(empty);
        };
        let len = entry.buffer.len();
        if entry.in_flight || empty.capacity() != len || filled > entry.filled {
            return Err(empty);
        }
        let fresh = NonNull::slice_from_raw_parts(
            NonNull::new(empty.as_mut_ptr()).expect("a Vec's pointer is never null"),
            len,
        );
        std::mem::forget(empty);
        let old = std::mem::replace(&mut entry.buffer, fresh);
        entry.filled = 0;
        // SAFETY: the URB is idle, so the kernel holds no reference to it.
        unsafe { (*entry.storage.as_ptr()).header.buffer = fresh.as_ptr().cast() };
        // SAFETY: not mapped, so `old` is a boxed slice or an earlier `empty`, an
        // allocation of `len` bytes this table owned and now gives up, and the
        // kernel wrote at least its first `filled` bytes.
        Ok(unsafe { Vec::from_raw_parts(old.as_ptr().cast(), filled, len) })
    }
}

impl Drop for Slots {
    fn drop(&mut self) {
        for entry in &self.slots {
            if entry.in_flight {
                // The kernel may still write these: never free them.
                continue;
            }
            // SAFETY: the storage was leaked from a box this table owns, and the
            // URB is idle.
            drop(unsafe { Box::from_raw(entry.storage.as_ptr()) });
            if !self.mapped {
                // SAFETY: without a mapping the buffer is a leaked box or an
                // exchanged Vec of the same layout, owned here; the URB is idle.
                drop(unsafe { Box::from_raw(entry.buffer.as_ptr()) });
            }
        }
    }
}

/// URBs and their buffers on one IN endpoint of an open device file.
///
/// Memory the kernel may still write is freed only once its URB was reaped:
/// [`UrbRing::cancel`] (also run on drop) discards and reaps everything, and
/// what the kernel does not give back in time is leaked, not freed.
pub struct UrbRing {
    /// The device file: a duplicate, so the same open file as the device's (the
    /// kernel's URB lists and poll readiness are per open file).
    fd: OwnedFd,
    config: RingConfig,
    slots: Slots,
    /// The usbdevfs mapping the buffers live in.
    mapping: Option<Mapping>,
}

// SAFETY: the raw pointers are allocations only this ring touches; the kernel
// writes them between submit and reap, and no shared reference exists then.
unsafe impl Send for UrbRing {}

impl UrbRing {
    pub(super) fn new(file: BorrowedFd<'_>, config: RingConfig) -> io::Result<Self> {
        let len = buffer_len(&config)?;
        let fd = file.try_clone_to_owned()?;
        // A kernel before 4.6, or usbfs memory exhausted: heap buffers.
        let mut mapping = if config.mmap {
            let total = len
                .checked_mul(config.count)
                .ok_or_else(|| invalid("the ring's buffers overflow"))?;
            Mapping::new(fd.as_fd(), mapping_len(total, page_size()), 0, true).ok()
        } else {
            None
        };
        let base = mapping
            .as_mut()
            .and_then(|mapping| NonNull::new(mapping.as_mut_slice().as_mut_ptr()));
        let slots = Slots::new(&config, len, base);
        Ok(Self {
            fd,
            config,
            slots,
            mapping,
        })
    }

    /// The ring's shape.
    pub fn config(&self) -> RingConfig {
        self.config
    }

    /// Whether the buffers are in a usbdevfs mapping.
    pub fn is_mapped(&self) -> bool {
        self.mapping.is_some()
    }

    /// URBs submitted and not yet reaped.
    pub fn in_flight(&self) -> usize {
        self.slots.in_flight()
    }

    /// Submits URB `slot`; a URB already in flight is left alone.
    pub fn submit(&mut self, slot: usize) -> io::Result<()> {
        let Some(urb) = self.slots.prepare(slot)? else {
            return Ok(());
        };
        // SAFETY: `urb` is a `usbdevfs_urb` followed by its packet descriptors,
        // and its buffer holds `buffer_length` bytes, all owned by this ring and
        // kept alive, unmoved and unread until the URB is reaped.
        unsafe { ioctl_ptr(self.fd.as_fd(), SUBMITURB, urb.cast::<UrbHeader>()) }?;
        self.slots.slots[slot].in_flight = true;
        Ok(())
    }

    /// Submits every idle URB.
    pub fn submit_all(&mut self) -> io::Result<()> {
        for slot in 0..self.slots.slots.len() {
            self.submit(slot)?;
        }
        Ok(())
    }

    /// Takes one completed URB if there is one; never blocks (poll the device
    /// file for `POLLOUT`). The URB stays idle until [`UrbRing::submit`]. An
    /// `ENODEV` error means the device went away and nothing completed is left.
    pub fn reap(&mut self) -> io::Result<Option<RingCompletion<'_>>> {
        let mut urb: *mut c_void = std::ptr::null_mut();
        // SAFETY: `urb` is a live pointer-sized slot for the completed URB's
        // address; the kernel writes the URB itself into memory this ring owns.
        match unsafe { ioctl_ptr(self.fd.as_fd(), REAPURBNDELAY, &mut urb) } {
            Ok(_) => {}
            Err(error) if error.raw_os_error() == Some(libc::EAGAIN) => return Ok(None),
            Err(error) => return Err(error),
        }
        self.slots.complete(urb, Instant::now()).map(Some)
    }

    /// The first `filled` bytes of idle URB `slot`, as its last completion left
    /// them (never more than a bulk completion wrote).
    pub fn idle_data(&self, slot: usize, filled: usize) -> &[u8] {
        self.slots.idle_data(slot, filled)
    }

    /// Takes the bytes of idle bulk URB `slot`, `filled` of them, and gives it
    /// `empty` to fill next, with no copy. Refused, with `empty` back, on mapped
    /// buffers (they cannot move), isochronous rings, a URB in flight, `filled`
    /// past what its last completion wrote, and unless `empty`'s capacity is
    /// exactly the URB's buffer length.
    pub fn exchange_buffer(
        &mut self,
        slot: usize,
        filled: usize,
        empty: Vec<u8>,
    ) -> Result<Vec<u8>, Vec<u8>> {
        self.slots.exchange_buffer(slot, filled, empty)
    }

    /// Discards every URB in flight and reaps them, waiting up to `timeout`.
    /// Returns how many could not be reaped.
    pub fn cancel(&mut self, timeout: Duration) -> usize {
        for entry in self.slots.slots.iter().filter(|entry| entry.in_flight) {
            // Already completed (`EINVAL`) is fine: the reap below takes it.
            let address = entry.storage.as_ptr() as usize as libc::c_ulong;
            let _ = ioctl_int(self.fd.as_fd(), DISCARDURB, address);
        }
        let deadline = Instant::now() + timeout;
        while self.in_flight() > 0 {
            match self.reap() {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                // Gone: the kernel has nothing left to give back.
                Err(_) => break,
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let _ = poll_one(
                self.fd.as_fd(),
                POLLOUT,
                Some((deadline - now).min(CANCEL_POLL)),
            );
        }
        self.in_flight()
    }
}

impl std::fmt::Debug for UrbRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UrbRing")
            .field("config", &self.config)
            .field("in_flight", &self.in_flight())
            .field("mapped", &self.is_mapped())
            .finish()
    }
}

impl Drop for UrbRing {
    fn drop(&mut self) {
        if self.cancel(Duration::from_secs(3)) > 0
            && let Some(mapping) = self.mapping.take()
        {
            // The kernel may still write the mapped buffers.
            std::mem::forget(mapping);
        }
    }
}

/// One URB's buffer length, after checking `config`.
fn buffer_len(config: &RingConfig) -> io::Result<usize> {
    if config.endpoint & 0x80 == 0 {
        return Err(invalid("a URB ring reads an IN endpoint"));
    }
    let len = match config.kind {
        RingKind::Iso {
            packets,
            packet_size,
        } => {
            if !(1..=MAX_ISO_PACKETS).contains(&packets) || packet_size == 0 {
                return Err(invalid(
                    "isochronous URBs take 1 to 128 packets of some size",
                ));
            }
            packets.saturating_mul(packet_size)
        }
        RingKind::Bulk { size } => size,
    };
    if config.count == 0 || len == 0 || len > i32::MAX as usize {
        return Err(invalid("a URB ring needs URBs of 1 byte to 2 GiB"));
    }
    Ok(len)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests;
