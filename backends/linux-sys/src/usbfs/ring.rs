//! A ring of isochronous URBs on one device file: the URB structs, the packet tables and the
//! buffers are allocated once, and a steady stream of submissions and completions allocates
//! nothing.
//!
//! The ring reaps straight from the device file, so nothing else may submit or reap on that file
//! while it exists: a completion goes to whoever reaps first. A URB this ring did not submit is
//! reported as an error.
//!
//! Each URB's layout (how many packets and of what lengths) is kept in this ring, apart from the
//! memory the kernel is handed, and a completion is checked against it before any slice is formed.
//! The kernel is not asked to keep anything else true either: it takes the transfer's length from
//! the packet table and never compares it to the buffer, so the table is checked against the
//! buffer on the way in as well.

use std::ffi::c_void;
use std::io;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use super::{
    Channel, DISCARDURB, IsoPacketDesc, REAPURBNDELAY, Readiness, SUBMITURB, URB_ISO_ASAP,
    URB_TYPE_ISO, UrbHeader, check_endpoint, malformed,
};

/// The most packets one isochronous URB may carry (usbfs's limit).
pub const MAX_ISO_PACKETS: usize = 128;

/// How long a cancel sleeps at most between two looks at the completions.
const CANCEL_POLL: Duration = Duration::from_millis(50);
/// The longest a cancel will wait.
const CANCEL_LIMIT: Duration = Duration::from_secs(60);

/// A ring's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingConfig {
    /// The URBs in the ring.
    pub count: usize,
    /// The bytes of each URB's buffer. A URB's packets must fit in it.
    pub buffer_len: usize,
}

/// A URB followed by room for its packet descriptors, as the kernel reads it.
#[repr(C)]
pub(super) struct UrbStorage {
    pub(super) header: UrbHeader,
    pub(super) packets: [IsoPacketDesc; MAX_ISO_PACKETS],
}

/// One isochronous packet of a completed URB.
#[derive(Debug, Clone, Copy)]
pub struct RingPacket<'a> {
    /// Zero, or a negative errno.
    pub status: i32,
    /// The bytes received.
    pub data: &'a [u8],
}

/// A completed URB, borrowed from its ring until the next ring call. Its lengths have been checked
/// against the URB's buffer and layout.
pub struct RingCompletion<'a> {
    /// Which URB of the ring.
    pub slot: usize,
    /// Zero, or a negative errno.
    pub status: i32,
    /// The bytes received, all packets together.
    pub actual_length: usize,
    /// Packets that failed.
    pub error_count: u32,
    buffer: &'a [u8],
    lengths: &'a [u32],
    packets: &'a [IsoPacketDesc],
}

impl std::fmt::Debug for RingCompletion<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RingCompletion")
            .field("slot", &self.slot)
            .field("status", &self.status)
            .field("actual_length", &self.actual_length)
            .field("packets", &self.packets.len())
            .finish()
    }
}

impl<'a> RingCompletion<'a> {
    /// The packets, in bus order. Packet `i` starts at the sum of the lengths scheduled before
    /// it, so a short packet leaves a gap after its bytes.
    pub fn packets(&self) -> impl Iterator<Item = RingPacket<'a>> + 'a {
        let buffer = self.buffer;
        self.lengths
            .iter()
            .zip(self.packets)
            .scan(0usize, move |offset, (&length, desc)| {
                let start = *offset;
                *offset = start.saturating_add(length as usize);
                let end = start.saturating_add(desc.actual_length as usize);
                Some(RingPacket {
                    status: desc.status as i32,
                    // Always in range: `complete` checked each packet against the layout.
                    data: buffer.get(start..end).unwrap_or(&[]),
                })
            })
    }

    /// Packets in this URB.
    pub fn packet_count(&self) -> usize {
        self.packets.len()
    }
}

/// A URB header ready for the kernel, built only from what the ring owns and has checked.
fn idle_header(buffer: NonNull<[u8]>, endpoint: u8, buffer_length: i32, packets: i32) -> UrbHeader {
    UrbHeader {
        kind: URB_TYPE_ISO,
        endpoint,
        status: 0,
        flags: URB_ISO_ASAP,
        buffer: buffer.as_ptr().cast(),
        buffer_length,
        actual_length: 0,
        start_frame: 0,
        number_of_packets: packets,
        error_count: 0,
        signr: 0,
        usercontext: std::ptr::null_mut(),
    }
}

struct Slot {
    storage: NonNull<UrbStorage>,
    buffer: NonNull<[u8]>,
    /// The layout the URB was submitted with: not shared with the kernel.
    lengths: [u32; MAX_ISO_PACKETS],
    packets: usize,
    /// `sum(lengths[..packets])`.
    submitted_len: usize,
    in_flight: bool,
}

/// The URB memory of a ring. Everything here runs without a device; the ring adds the kernel calls.
struct Slots {
    slots: Vec<Slot>,
}

impl Slots {
    fn new(config: &RingConfig) -> Self {
        let mut slots = Vec::with_capacity(config.count);
        for _ in 0..config.count {
            let buffer = NonNull::from(Box::leak(vec![0u8; config.buffer_len].into_boxed_slice()));
            let storage = Box::new(UrbStorage {
                header: idle_header(buffer, 0, 0, 0),
                packets: [IsoPacketDesc {
                    length: 0,
                    actual_length: 0,
                    status: 0,
                }; MAX_ISO_PACKETS],
            });
            slots.push(Slot {
                storage: NonNull::from(Box::leak(storage)),
                buffer,
                lengths: [0; MAX_ISO_PACKETS],
                packets: 0,
                submitted_len: 0,
                in_flight: false,
            });
        }
        Self { slots }
    }

    /// Readies idle URB `slot` for the kernel with this layout and returns its address. Nothing
    /// is written until every length has been checked.
    fn arm(&mut self, slot: usize, endpoint: u8, lengths: &[u32]) -> io::Result<*mut UrbStorage> {
        check_endpoint(endpoint, true)?;
        if lengths.is_empty() || lengths.len() > MAX_ISO_PACKETS {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let entry = self
            .slots
            .get_mut(slot)
            .ok_or(io::ErrorKind::InvalidInput)?;
        if entry.in_flight {
            return Err(io::ErrorKind::ResourceBusy.into());
        }
        // In u64 so that 128 lengths of `u32::MAX` can't wrap.
        let total: u64 = lengths.iter().map(|&length| u64::from(length)).sum();
        let submitted_len = usize::try_from(total)
            .ok()
            .filter(|&total| total <= entry.buffer.len())
            .ok_or(io::ErrorKind::InvalidInput)?;
        // The buffer fits in an i32, by `check_config`.
        let buffer_length =
            i32::try_from(submitted_len).map_err(|_| io::ErrorKind::InvalidInput)?;

        let urb = entry.storage.as_ptr();
        // SAFETY: `urb` is this slot's live allocation, and the URB is idle: the kernel holds no
        // reference to it, so this borrow races with nothing.
        let storage = unsafe { &mut *urb };
        // The whole header, from what this ring knows: nothing an earlier completion left in it
        // is carried into the next submission.
        storage.header = idle_header(entry.buffer, endpoint, buffer_length, lengths.len() as i32);
        for (index, &length) in lengths.iter().enumerate() {
            storage.packets[index] = IsoPacketDesc {
                length,
                actual_length: 0,
                status: 0,
            };
            entry.lengths[index] = length;
        }
        entry.packets = lengths.len();
        entry.submitted_len = submitted_len;
        Ok(urb)
    }

    /// Takes the URB the kernel handed back at `urb`. Only its address is looked at until it has
    /// been matched to a slot in flight.
    fn complete(&mut self, urb: *mut c_void) -> io::Result<RingCompletion<'_>> {
        let slot = self
            .slots
            .iter()
            .position(|entry| entry.storage.as_ptr().cast() == urb && entry.in_flight)
            .ok_or_else(malformed)?;
        let entry = &mut self.slots[slot];
        entry.in_flight = false;
        // SAFETY: the URB was reaped, so the kernel no longer writes the slot's memory, and it
        // stays idle while the completion borrows the ring.
        let storage = unsafe { entry.storage.as_ref() };
        // SAFETY: as above, for the buffer, all of whose bytes were initialised when it was made.
        let buffer = unsafe { entry.buffer.as_ref() };
        let header = &storage.header;
        let packets = storage.packets.get(..entry.packets).ok_or_else(malformed)?;
        let lengths = entry.lengths.get(..entry.packets).ok_or_else(malformed)?;
        let actual_length = check_completion(
            header,
            packets,
            lengths,
            buffer.as_ptr().cast_mut().cast(),
            buffer.len(),
            entry.submitted_len,
        )?;
        Ok(RingCompletion {
            slot,
            status: header.status,
            actual_length,
            error_count: u32::try_from(header.error_count).unwrap_or(0),
            buffer,
            lengths,
            packets,
        })
    }

    fn in_flight(&self) -> usize {
        self.slots.iter().filter(|entry| entry.in_flight).count()
    }
}

/// Checks a completed URB's reported lengths against what was submitted, and returns its
/// `actual_length`. The header fields the kernel has no reason to change must be unchanged.
fn check_completion(
    header: &UrbHeader,
    packets: &[IsoPacketDesc],
    lengths: &[u32],
    buffer: *mut c_void,
    buffer_len: usize,
    submitted_len: usize,
) -> io::Result<usize> {
    let actual_length = usize::try_from(header.actual_length).map_err(|_| malformed())?;
    if actual_length > submitted_len
        || header.buffer != buffer
        || usize::try_from(header.buffer_length).ok() != Some(submitted_len)
        || usize::try_from(header.number_of_packets).ok() != Some(lengths.len())
    {
        return Err(malformed());
    }
    let mut offset: u64 = 0;
    for (desc, &length) in packets.iter().zip(lengths) {
        let actual = u64::from(desc.actual_length);
        if actual > u64::from(length) || offset + actual > buffer_len as u64 {
            return Err(malformed());
        }
        offset += u64::from(length);
    }
    Ok(actual_length)
}

impl Drop for Slots {
    fn drop(&mut self) {
        for entry in &self.slots {
            if entry.in_flight {
                // The kernel may still write these: never free them.
                continue;
            }
            // SAFETY: the storage was leaked from a box this table owns, and the URB is idle.
            drop(unsafe { Box::from_raw(entry.storage.as_ptr()) });
            // SAFETY: the buffer was leaked from a boxed slice this table owns, and the URB is
            // idle.
            drop(unsafe { Box::from_raw(entry.buffer.as_ptr()) });
        }
    }
}

/// URBs and their buffers on an open device file.
///
/// Memory the kernel may still write is freed only once its URB was reaped: [`UrbRing::cancel`]
/// (also run on drop) discards and reaps everything, and what the kernel does not give back in
/// time is leaked, not freed.
pub struct UrbRing {
    channel: Channel,
    config: RingConfig,
    slots: Slots,
}

// SAFETY: the raw pointers are allocations only this ring touches; the kernel writes them between
// submit and reap, and no shared reference exists then.
unsafe impl Send for UrbRing {}

impl UrbRing {
    pub(super) fn new(channel: Channel, config: RingConfig) -> io::Result<Self> {
        check_config(&config)?;
        Ok(Self {
            channel,
            config,
            slots: Slots::new(&config),
        })
    }

    /// The ring's shape.
    pub fn config(&self) -> RingConfig {
        self.config
    }

    /// URBs submitted and not yet reaped.
    pub fn in_flight(&self) -> usize {
        self.slots.in_flight()
    }

    /// Whether URB `slot` is submitted and not yet reaped. `false` for a slot past the ring.
    #[must_use]
    pub fn is_in_flight(&self, slot: usize) -> bool {
        self.slots
            .slots
            .get(slot)
            .is_some_and(|entry| entry.in_flight)
    }

    /// Submits URB `slot` to IN `endpoint`, one packet of each length in `lengths`. A URB in
    /// flight is refused with `ResourceBusy`.
    pub fn submit(&mut self, slot: usize, endpoint: u8, lengths: &[u32]) -> io::Result<()> {
        let urb = self.slots.arm(slot, endpoint, lengths)?;
        // SAFETY: `urb` is a `usbdevfs_urb` followed by its packet descriptors, and its buffer
        // holds at least the bytes those lengths add up to, all owned by this ring and kept
        // alive, unmoved and unread until the URB is reaped.
        unsafe { self.channel.ioctl_ptr(SUBMITURB, urb.cast::<UrbHeader>()) }?;
        self.slots.slots[slot].in_flight = true;
        Ok(())
    }

    /// Takes one completed URB if there is one; never blocks ([`UrbRing::wait`] waits). The URB
    /// stays idle until the next submit. `InvalidData` means the kernel reported a URB this ring
    /// did not submit, or lengths its buffer can't hold. An `ENODEV` error means the device went
    /// away and nothing completed is left.
    pub fn reap(&mut self) -> io::Result<Option<RingCompletion<'_>>> {
        let mut urb: *mut c_void = std::ptr::null_mut();
        // SAFETY: `urb` is a live pointer-sized slot for the completed URB's address; the kernel
        // writes the URB itself into memory this ring owns.
        match unsafe { self.channel.ioctl_ptr(REAPURBNDELAY, &raw mut urb) } {
            Ok(_) => {}
            Err(error) if error.raw_os_error() == Some(libc::EAGAIN) => return Ok(None),
            Err(error) => return Err(error),
        }
        self.slots.complete(urb).map(Some)
    }

    /// Waits up to `timeout` for a completion to reap.
    pub fn wait(&self, timeout: Duration) -> io::Result<Readiness> {
        self.channel.wait(timeout)
    }

    /// Discards every URB in flight and reaps them, waiting up to `timeout`. Returns how many
    /// could not be reaped. Those stay in flight, and their memory stays allocated.
    pub fn cancel(&mut self, timeout: Duration) -> usize {
        for entry in self.slots.slots.iter().filter(|entry| entry.in_flight) {
            // Already completed (`EINVAL`) is fine: the reap below takes it.
            let address = entry.storage.as_ptr() as usize as libc::c_ulong;
            let _ = self.channel.ioctl_int(DISCARDURB, address);
        }
        let deadline = Instant::now() + timeout.min(CANCEL_LIMIT);
        while self.in_flight() > 0 {
            let gone = match self.reap() {
                Ok(Some(_)) => false,
                Ok(None) => {
                    let wait = deadline
                        .saturating_duration_since(Instant::now())
                        .min(CANCEL_POLL);
                    self.wait(wait).is_err()
                }
                // A bad report is still a reap: keep draining.
                Err(error) if error.kind() == io::ErrorKind::InvalidData => false,
                // Gone: the kernel has nothing left to give back.
                Err(_) => true,
            };
            if gone || Instant::now() >= deadline {
                break;
            }
        }
        self.in_flight()
    }
}

impl std::fmt::Debug for UrbRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UrbRing")
            .field("config", &self.config)
            .field("in_flight", &self.in_flight())
            .finish()
    }
}

impl Drop for UrbRing {
    fn drop(&mut self) {
        self.cancel(Duration::from_secs(3));
    }
}

/// Refuses a shape with nothing in it, or whose buffers don't fit the kernel's `int` lengths,
/// together or apart.
fn check_config(config: &RingConfig) -> io::Result<()> {
    let total = config.count.checked_mul(config.buffer_len);
    if config.count == 0
        || config.buffer_len == 0
        || config.buffer_len > i32::MAX as usize
        || total.is_none_or(|total| total > i32::MAX as usize)
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
