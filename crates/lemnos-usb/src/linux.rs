//! The Linux backend: usbdevfs, over `lemnos-linux-sys`.
//!
//! Synchronous transfers are the kernel's control and bulk ioctls. Asynchronous
//! ones (bulk, interrupt and isochronous) are URBs: a reaper thread waits for
//! completions while any are in flight and exits when none are. The reaper
//! holds the backend, not the device, so a transfer outlives its handle.

use crate::buffer::Storage;
use crate::error::{Error, Result};
use crate::transfer::{Canceller, IsoPacket, Shared, Transfer, TransferOutcome, TransferStatus};
use crate::types::{ControlSetup as Setup, Direction};
use lemnos_linux_sys::poll::{POLLERR, POLLHUP, POLLNVAL, POLLOUT, poll_one};
use lemnos_linux_sys::pool::Spares;
use lemnos_linux_sys::usbfs::{
    ControlSetup, IsoPacket as UrbPacket, RingConfig, UrbCompletion, UrbKind, UrbRing, UsbfsFile,
};
use std::collections::HashMap;
use std::io;
use std::os::fd::BorrowedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Packet tables kept for reuse by the isochronous readers.
const SPARE_PACKET_TABLES: usize = 16;

/// How long the reaper waits for a completion before checking whether any
/// transfer is still in flight.
const REAP_POLL: Duration = Duration::from_millis(100);

/// `GET_CONFIGURATION` (USB 2.0 chapter 9).
const GET_CONFIGURATION: u8 = 0x08;

#[derive(Default)]
struct State {
    pending: HashMap<u64, Arc<Shared>>,
    /// A reaper thread is running (it exits when nothing is pending).
    reaping: bool,
    /// The caller reaps (`reap_now`, `reap_until`): no reaper thread is started.
    caller_driven: bool,
    /// A URB ring reaps this device: nothing else may submit or reap.
    ring: bool,
}

/// An open usbdevfs device and its asynchronous transfers.
pub(crate) struct Usbfs {
    file: UsbfsFile,
    state: Mutex<State>,
    next_tag: AtomicU64,
    /// Whether the kernel maps DMA buffers for this device (`USBDEVFS_CAP_MMAP`).
    mapped: bool,
    /// The largest mapping asked for, in bytes: the kernel's usbfs memory limit
    /// (`usbfs_memory_mb`, shared by every usbfs user). Larger buffers are heap.
    mapping_limit: usize,
    /// The packet scratch of the reaping, kept between completions.
    scratch: Mutex<Vec<UrbPacket>>,
    /// Packet tables handed to outcomes, given back by the isochronous readers.
    packet_tables: Spares<Vec<IsoPacket>>,
}

impl Usbfs {
    /// Opens `/dev/bus/usb/BBB/DDD` for the device `info` describes.
    pub(crate) fn open(info: &nusb::DeviceInfo) -> Result<Arc<Self>> {
        let path = PathBuf::from(format!(
            "/dev/bus/usb/{:03}/{:03}",
            info.busnum(),
            info.device_address()
        ));
        let file = UsbfsFile::open(&path)
            .map_err(|error| Error::from_io(&format!("opening {}", path.display()), &error))?;
        let mapped = file.has_mmap().unwrap_or(false);
        Ok(Arc::new(Self {
            file,
            state: Mutex::new(State::default()),
            next_tag: AtomicU64::new(1),
            mapped,
            mapping_limit: usbfs_memory_limit(),
            scratch: Mutex::new(Vec::new()),
            packet_tables: Spares::new(SPARE_PACKET_TABLES),
        }))
    }

    /// Memory for a transfer of `len` bytes: a usbdevfs mapping when the kernel
    /// offers one, and the heap otherwise (also when a mapping fails).
    pub(crate) fn allocate(&self, len: usize) -> Storage {
        if self.mapped
            && len <= self.mapping_limit
            && let Ok(buffer) = self.file.map_buffer(len)
        {
            return buffer;
        }
        crate::buffer::heap(vec![0; len])
    }

    /// The device's raw descriptors (see `configurations_from_blob`).
    pub(crate) fn read_descriptors(&self) -> Result<Vec<u8>> {
        map_io(
            "reading the device descriptors",
            self.file.read_descriptors(),
        )
    }

    /// The active configuration's value.
    pub(crate) fn active_configuration(&self, timeout: Duration) -> Result<u8> {
        let mut value = [0u8; 1];
        let setup = Setup {
            direction: Direction::In,
            kind: crate::types::ControlKind::Standard,
            recipient: crate::types::ControlRecipient::Device,
            request: GET_CONFIGURATION,
            value: 0,
            index: 0,
        };
        self.control_in(&setup, &mut value, timeout)?;
        Ok(value[0])
    }

    pub(crate) fn control_in(
        &self,
        setup: &Setup,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize> {
        let native = native_setup(setup);
        map_io(
            "control transfer",
            self.file.control_in(native, data, timeout_ms(timeout)),
        )
    }

    pub(crate) fn control_out(
        &self,
        setup: &Setup,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize> {
        let native = native_setup(setup);
        map_io(
            "control transfer",
            self.file.control_out(native, data, timeout_ms(timeout)),
        )
    }

    pub(crate) fn bulk_in(
        &self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize> {
        map_io(
            "bulk read",
            self.file.bulk_in(endpoint, data, timeout_ms(timeout)),
        )
    }

    pub(crate) fn bulk_out(&self, endpoint: u8, data: &[u8], timeout: Duration) -> Result<usize> {
        map_io(
            "bulk write",
            self.file.bulk_out(endpoint, data, timeout_ms(timeout)),
        )
    }

    /// The kernel driver bound to interface `number` (`USBDEVFS_GETDRIVER`).
    pub(crate) fn driver(&self, number: u8) -> Result<Option<String>> {
        map_io("reading the bound driver", self.file.driver(number))
    }

    /// The device's speed as the kernel's `enum usb_device_speed` (`USBDEVFS_GET_SPEED`).
    pub(crate) fn speed(&self) -> Result<i32> {
        map_io("reading the device speed", self.file.speed())
    }

    pub(crate) fn claim_interface(&self, number: u8, detach: bool) -> Result<()> {
        map_io(
            "claiming the interface",
            self.file.claim_interface(number, detach),
        )
    }

    pub(crate) fn release_interface(&self, number: u8, reattach: bool) -> Result<()> {
        map_io(
            "releasing the interface",
            self.file.release_interface(number, reattach),
        )
    }

    pub(crate) fn set_interface(&self, number: u8, alternate: u8) -> Result<()> {
        map_io(
            "selecting the alternate setting",
            self.file.set_interface(number, alternate),
        )
    }

    pub(crate) fn set_configuration(&self, value: u8) -> Result<()> {
        map_io(
            "selecting the configuration",
            self.file.set_configuration(value),
        )
    }

    pub(crate) fn clear_halt(&self, endpoint: u8) -> Result<()> {
        map_io("clearing the halt", self.file.clear_halt(endpoint))
    }

    pub(crate) fn reset(&self) -> Result<()> {
        map_io("resetting the device", self.file.reset())
    }

    /// Submits an asynchronous transfer and returns its handle.
    pub(crate) fn submit(
        self: &Arc<Self>,
        kind: UrbKind,
        endpoint: u8,
        buffer: Storage,
        packet_lengths: &[u32],
        zero_packet: bool,
    ) -> Result<Transfer> {
        let tag = self.next_tag.fetch_add(1, Ordering::Relaxed);
        let shared = Shared::new();
        {
            // Registered before the kernel sees the URB, so a completion that
            // arrives at once finds its waiter.
            let mut state = self.lock_state();
            if state.ring {
                return Err(ring_owns_reaping());
            }
            state.pending.insert(tag, Arc::clone(&shared));
            if !state.reaping && !state.caller_driven {
                let reaper = Arc::clone(self);
                if let Err(error) = std::thread::Builder::new()
                    .name("lemnos-usb-reap".into())
                    .spawn(move || reap_loop(reaper))
                {
                    state.pending.remove(&tag);
                    return Err(Error::from_io("starting the USB completion thread", &error));
                }
                state.reaping = true;
            }
        }
        if let Err(error) =
            self.file
                .submit(tag, kind, endpoint, buffer, packet_lengths, zero_packet)
        {
            self.lock_state().pending.remove(&tag);
            return Err(Error::from_io("submitting a USB transfer", &error));
        }
        Ok(Transfer::new(
            shared,
            Some(Box::new(UsbfsCancel {
                usb: Arc::clone(self),
                tag,
            })),
        ))
    }

    /// Delivers one reaped completion to its transfer. `reaped` is the moment the
    /// kernel handed it over: taken before the waiter is woken.
    fn deliver(&self, done: UrbCompletion, packets: &[UrbPacket], reaped: Instant) {
        let shared = self.lock_state().pending.remove(&done.tag);
        let Some(shared) = shared else {
            return;
        };
        shared.complete(Ok(outcome(done, packets, &self.packet_tables, reaped)));
    }

    /// Reaps every completion the kernel has ready, without waiting, and
    /// delivers each to its transfer. Returns how many were delivered. The
    /// reaper thread and the caller-driven calls both reap through here.
    fn reap_available(&self) -> io::Result<usize> {
        let mut packets = std::mem::take(&mut *self.lock_scratch());
        let mut delivered = 0;
        let result = loop {
            match self.file.reap(false, &mut packets) {
                Ok(Some(done)) => {
                    let reaped = Instant::now();
                    self.deliver(done, &packets, reaped);
                    delivered += 1;
                }
                Ok(None) => break Ok(delivered),
                Err(error) => break Err(error),
            }
        };
        *self.lock_scratch() = packets;
        result
    }

    /// Caller-driven reaping: switches the device to it (no reaper thread).
    /// Refused while transfers are in flight, so no thread holds completions.
    pub(crate) fn set_caller_driven(&self, on: bool) -> Result<()> {
        let mut state = self.lock_state();
        if state.reaping || !state.pending.is_empty() {
            return Err(Error::new(
                crate::error::ErrorKind::Busy,
                "transfers are in flight; set the reaping mode before submitting",
            ));
        }
        state.caller_driven = on;
        Ok(())
    }

    /// A URB ring on this device. The ring takes over the device's reaping, so
    /// it is refused while a transfer is in flight or another ring exists; give
    /// it back with [`Usbfs::release_ring`].
    pub(crate) fn urb_ring(&self, config: RingConfig) -> Result<UrbRing> {
        let mut state = self.lock_state();
        if state.ring || state.reaping || !state.pending.is_empty() {
            return Err(Error::new(
                crate::error::ErrorKind::Busy,
                "a URB ring needs the device to itself: a ring or a transfer is active",
            ));
        }
        let ring = map_io("allocating a URB ring", self.file.urb_ring(config))?;
        state.ring = true;
        Ok(ring)
    }

    /// The URB ring has gone: transfers may be submitted again.
    pub(crate) fn release_ring(&self) {
        self.lock_state().ring = false;
    }

    /// The device file, for polling.
    pub(crate) fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }

    /// Delivers the completions ready now; returns how many.
    pub(crate) fn reap_now(&self) -> Result<usize> {
        if self.lock_state().ring {
            return Err(ring_owns_reaping());
        }
        self.reap_available()
            .map_err(|error| self.reap_failed(&error))
    }

    /// Waits until a completion is ready or `deadline` passes, then delivers the
    /// ready completions. Returns how many were delivered (zero at the deadline).
    pub(crate) fn reap_until(&self, deadline: Instant) -> Result<usize> {
        loop {
            let delivered = self.reap_now()?;
            if delivered > 0 {
                return Ok(delivered);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(0);
            }
            let wait = (deadline - now).min(REAP_POLL);
            match poll_one(self.file.as_fd(), POLLOUT, Some(wait)) {
                Ok(revents) => {
                    if let Err(error) = hangup_check(revents) {
                        return Err(self.reap_failed(&error));
                    }
                }
                Err(error) => return Err(self.reap_failed(&error)),
            }
        }
    }

    /// A reaping error ends every transfer in flight (the kernel's completions
    /// can no longer be matched), and is the caller's error too.
    fn reap_failed(&self, error: &std::io::Error) -> Error {
        self.fail_all(error);
        Error::from_io("reaping USB transfers", error)
    }

    /// Whether the caller reaps this device (no reaper thread).
    pub(crate) fn caller_driven(&self) -> bool {
        self.lock_state().caller_driven
    }

    /// Gives back a packet table an outcome held (an isochronous reader does
    /// this once it has read the packets).
    pub(crate) fn recycle_packets(&self, packets: Vec<IsoPacket>) {
        self.packet_tables.give(packets);
    }

    fn lock_scratch(&self) -> MutexGuard<'_, Vec<UrbPacket>> {
        self.scratch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Fails every transfer in flight: the reaper cannot finish them.
    fn fail_all(&self, error: &std::io::Error) {
        let pending = {
            let mut state = self.lock_state();
            state.reaping = false;
            std::mem::take(&mut state.pending)
        };
        for shared in pending.into_values() {
            shared.complete(Err(Error::from_io("reaping USB transfers", error)));
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The usbfs memory limit in bytes: `usbfs_memory_mb` (default 16 MiB) from the
/// usbcore module parameters. A mapping over it cannot succeed, so it is heap.
fn usbfs_memory_limit() -> usize {
    const DEFAULT_MB: usize = 16;
    std::fs::read_to_string("/sys/module/usbcore/parameters/usbfs_memory_mb")
        .ok()
        .and_then(|text| text.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_MB)
        .saturating_mul(1024 * 1024)
}

/// Waits for completions while transfers are in flight, then exits.
fn reap_loop(usb: Arc<Usbfs>) {
    loop {
        {
            // One lock for the check and the exit: a submit that follows sees
            // `reaping` clear and starts a new reaper.
            let mut state = usb.lock_state();
            if state.pending.is_empty() {
                state.reaping = false;
                return;
            }
        }
        // A hang-up or error on the device file cannot be reaped past: fail the
        // transfers now. Without this check a hung-up file reports ready at once,
        // and nothing reaps it, so the loop would spin.
        match poll_one(usb.file.as_fd(), POLLOUT, Some(REAP_POLL)).and_then(hangup_check) {
            Ok(0) => continue,
            Ok(_) => {}
            Err(error) => {
                usb.fail_all(&error);
                return;
            }
        }
        if let Err(error) = usb.reap_available() {
            usb.fail_all(&error);
            return;
        }
    }
}

/// The outcome of a completed URB. It always carries the buffer back, and the
/// status: a whole-URB failure is a status, not an error that drops the memory.
/// An isochronous URB's whole status is `Ok` unless it ends the URB; its packets
/// carry their own.
fn outcome(
    done: UrbCompletion,
    urb_packets: &[UrbPacket],
    tables: &Spares<Vec<IsoPacket>>,
    reaped: Instant,
) -> TransferOutcome {
    let isochronous = !urb_packets.is_empty();
    let status = urb_status(done.status, isochronous);
    // A bulk transfer has no packet table: an empty Vec allocates nothing. An
    // isochronous one takes a table the readers gave back, when there is one.
    let mut packets = if isochronous {
        tables.take().unwrap_or_default()
    } else {
        Vec::new()
    };
    packets.clear();
    packets.extend(urb_packets.iter().map(|packet| IsoPacket {
        length: packet.length as usize,
        actual_length: packet.actual_length as usize,
        status: TransferStatus::from_kernel(packet.status),
    }));
    TransferOutcome {
        buffer: crate::buffer::wrap(done.buffer),
        actual_length: done.actual_length,
        packets,
        status,
        reaped,
    }
}

/// The error for a poll that reports the device file gone (`POLLERR`, `POLLHUP`,
/// `POLLNVAL`); any other events are fine.
fn hangup_check(revents: i16) -> io::Result<i16> {
    if revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
        return Err(io::Error::other(
            "the device file reports an error or a hang-up",
        ));
    }
    Ok(revents)
}

/// The refusal while a URB ring reaps the device.
fn ring_owns_reaping() -> Error {
    Error::new(
        crate::error::ErrorKind::Busy,
        "a URB ring reaps this device: drop it before other asynchronous transfers",
    )
}

/// A URB's status: an isochronous URB is fine unless it ended as a whole, since
/// its packets carry their own.
pub(crate) fn urb_status(status: i32, isochronous: bool) -> TransferStatus {
    if isochronous && !is_fatal_for_packets(status) {
        TransferStatus::Ok
    } else {
        TransferStatus::from_kernel(status)
    }
}

/// Statuses that end the whole isochronous URB, not just one packet.
fn is_fatal_for_packets(status: i32) -> bool {
    matches!(
        -status,
        libc::ENOENT | libc::ECONNRESET | libc::ENODEV | libc::ESHUTDOWN
    )
}

/// Cancels one URB by its tag.
struct UsbfsCancel {
    usb: Arc<Usbfs>,
    tag: u64,
}

impl Canceller for UsbfsCancel {
    fn cancel(&self) {
        let _ = self.usb.file.discard(self.tag);
    }
}

fn native_setup(setup: &Setup) -> ControlSetup {
    ControlSetup {
        request_type: setup.request_type(),
        request: setup.request,
        value: setup.value,
        index: setup.index,
    }
}

fn map_io<T>(context: &str, result: std::io::Result<T>) -> Result<T> {
    result.map_err(|error| Error::from_io(context, &error))
}

/// The usbdevfs timeout: milliseconds, where zero waits forever.
pub(crate) fn timeout_ms(timeout: Duration) -> u32 {
    u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX)
}

/// The URB kind for a transfer type.
pub(crate) fn urb_kind(transfer: crate::types::TransferType) -> Option<UrbKind> {
    match transfer {
        crate::types::TransferType::Bulk => Some(UrbKind::Bulk),
        crate::types::TransferType::Interrupt => Some(UrbKind::Interrupt),
        crate::types::TransferType::Isochronous => Some(UrbKind::Isochronous),
        crate::types::TransferType::Control => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_linux_sys::usbfs::UrbBuffer;
    use std::time::Instant;

    /// A device whose file is `/dev/null`: the state checks run before any
    /// usbdevfs call, and the ring needs only a descriptor to duplicate.
    fn null_device() -> Arc<Usbfs> {
        Arc::new(Usbfs {
            file: UsbfsFile::open(std::path::Path::new("/dev/null")).unwrap(),
            state: Mutex::new(State::default()),
            next_tag: AtomicU64::new(1),
            mapped: false,
            mapping_limit: 0,
            scratch: Mutex::new(Vec::new()),
            packet_tables: Spares::new(1),
        })
    }

    const RING: RingConfig = RingConfig {
        endpoint: 0x81,
        kind: lemnos_linux_sys::usbfs::RingKind::Bulk { size: 64 },
        count: 2,
        mmap: false,
    };

    fn is_busy<T>(result: Result<T>) -> bool {
        result.is_err_and(|error| error.kind() == crate::error::ErrorKind::Busy)
    }

    #[test]
    fn a_ring_has_the_device_to_itself() {
        let usb = null_device();
        let ring = usb.urb_ring(RING).unwrap();
        assert!(is_busy(usb.urb_ring(RING)));
        assert!(is_busy(usb.reap_now()));
        let buffer = crate::buffer::heap(vec![0; 8]);
        assert!(is_busy(usb.submit(UrbKind::Bulk, 0x81, buffer, &[], false)));
        assert!(usb.lock_state().pending.is_empty());

        drop(ring);
        usb.release_ring();
        assert!(usb.urb_ring(RING).is_ok());
    }

    #[test]
    fn a_ring_waits_for_transfers_in_flight() {
        let usb = null_device();
        usb.lock_state().pending.insert(1, Shared::new());
        assert!(is_busy(usb.urb_ring(RING)));
        usb.lock_state().pending.clear();
        usb.lock_state().reaping = true;
        assert!(is_busy(usb.urb_ring(RING)));
    }

    #[test]
    fn urb_status_keeps_packet_failures_in_the_packets() {
        assert_eq!(urb_status(-libc::EXDEV, true), TransferStatus::Ok);
        assert_eq!(urb_status(-libc::ENOENT, true), TransferStatus::Cancelled);
        assert_eq!(
            urb_status(-libc::ENODEV, true),
            TransferStatus::Disconnected
        );
        assert_eq!(
            urb_status(-libc::EXDEV, false),
            TransferStatus::Error(libc::EXDEV)
        );
    }

    fn completion(status: i32, actual_length: usize) -> UrbCompletion {
        UrbCompletion {
            tag: 1,
            buffer: UrbBuffer::Heap(vec![0; 8]),
            status,
            actual_length,
        }
    }

    fn packet(length: u32, actual_length: u32, status: i32) -> UrbPacket {
        UrbPacket {
            length,
            actual_length,
            status,
        }
    }

    /// A bulk outcome has no packet table, and takes none from the pool.
    #[test]
    fn a_bulk_outcome_takes_no_packet_table() {
        let tables = Spares::new(4);
        tables.give(Vec::with_capacity(8));
        let out = outcome(completion(0, 5), &[], &tables, Instant::now());
        assert!(out.packets.is_empty());
        assert_eq!(tables.len(), 1, "the pool keeps its table");
    }

    /// An isochronous outcome reuses the table a reader gave back: its capacity
    /// survives, and the packet statuses are the URB's.
    #[test]
    fn an_isochronous_outcome_reuses_a_returned_table() {
        let tables = Spares::new(4);
        tables.give(Vec::with_capacity(16));
        let urb = [packet(100, 100, 0), packet(100, 0, -libc::EPROTO)];
        let out = outcome(completion(0, 100), &urb, &tables, Instant::now());
        assert_eq!(out.packets.len(), 2);
        assert_eq!(out.packets[0].status, TransferStatus::Ok);
        assert_eq!(
            out.packets[1].status,
            TransferStatus::Error(libc::EPROTO),
            "a packet's own error"
        );
        assert_eq!(
            out.status,
            TransferStatus::Ok,
            "the URB as a whole completed"
        );
        assert!(
            out.packets.capacity() >= 16,
            "the returned table was reused"
        );
        assert!(tables.is_empty(), "the table left the pool");
    }

    /// A steady isochronous stream hands the same packet table round and round:
    /// the reader gives each table back as the next outcome is built, so the
    /// table's storage (its pointer) never changes after the first transfer.
    #[test]
    fn a_steady_isochronous_stream_reuses_one_packet_table() {
        let tables = Spares::new(SPARE_PACKET_TABLES);
        let urb = [packet(100, 100, 0), packet(100, 40, 0), packet(100, 100, 0)];
        let mut first_storage = None;
        for _ in 0..200 {
            let out = outcome(completion(0, 240), &urb, &tables, Instant::now());
            let storage = out.packets.as_ptr() as usize;
            assert_eq!(out.packets.len(), 3);
            match first_storage {
                None => first_storage = Some(storage),
                Some(first) => assert_eq!(storage, first, "the table was allocated again"),
            }
            let (_buffer, packets) = out.into_parts();
            tables.give(packets);
            assert_eq!(tables.len(), 1, "one table circulates");
        }
    }

    /// A status that ends the whole isochronous URB is the transfer's status, and
    /// the buffer still comes back with it.
    #[test]
    fn a_fatal_status_is_the_outcome_status_and_keeps_the_buffer() {
        let tables = Spares::new(4);
        let urb = [packet(100, 0, 0)];
        let out = outcome(completion(-libc::ENODEV, 0), &urb, &tables, Instant::now());
        assert_eq!(out.status, TransferStatus::Disconnected);
        assert_eq!(out.buffer.len(), 8, "the buffer is back");
    }

    /// A stall is a status of the whole transfer (the caller clears the halt).
    #[test]
    fn a_stall_is_a_status_not_an_error() {
        let tables = Spares::new(4);
        let out = outcome(completion(-libc::EPIPE, 0), &[], &tables, Instant::now());
        assert_eq!(out.status, TransferStatus::Stalled);
        assert!(out.packets.is_empty());
    }

    /// A bulk outcome hands the very memory it submitted back: the caller's
    /// buffer is moved through the transfer, never copied.
    #[test]
    fn a_bulk_outcome_hands_back_the_same_memory() {
        let tables = Spares::new(4);
        let heap = vec![7u8; 16];
        let address = heap.as_ptr();
        let done = UrbCompletion {
            tag: 1,
            buffer: UrbBuffer::Heap(heap),
            status: 0,
            actual_length: 16,
        };
        let out = outcome(done, &[], &tables, Instant::now());
        assert_eq!(out.buffer.as_slice().as_ptr(), address, "no copy");
        assert_eq!(out.status, TransferStatus::Ok);
    }
}
