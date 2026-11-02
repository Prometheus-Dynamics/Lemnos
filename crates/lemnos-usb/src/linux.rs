//! The Linux backend: usbdevfs, over `lemnos-linux-sys`.
//!
//! Synchronous transfers are the kernel's control and bulk ioctls. Asynchronous
//! ones (bulk, interrupt and isochronous) are URBs: a reaper thread waits for
//! completions while any are in flight and exits when none are. The reaper
//! holds the backend, not the device, so a transfer outlives its handle.

use crate::buffer::Storage;
use crate::error::{Error, Result};
use crate::transfer::{Canceller, IsoPacket, Shared, Transfer, TransferOutcome};
use crate::types::{ControlSetup as Setup, Direction};
use lemnos_linux_sys::poll::{POLLOUT, poll_one};
use lemnos_linux_sys::pool::Spares;
use lemnos_linux_sys::usbfs::{
    ControlSetup, IsoPacket as UrbPacket, UrbCompletion, UrbKind, UsbfsFile,
};
use std::collections::HashMap;
use std::io;
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
}

/// An open usbdevfs device and its asynchronous transfers.
pub(crate) struct Usbfs {
    file: UsbfsFile,
    state: Mutex<State>,
    next_tag: AtomicU64,
    /// Whether the kernel maps DMA buffers for this device (`USBDEVFS_CAP_MMAP`).
    mapped: bool,
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
            scratch: Mutex::new(Vec::new()),
            packet_tables: Spares::new(SPARE_PACKET_TABLES),
        }))
    }

    /// Memory for a transfer of `len` bytes: a usbdevfs mapping when the kernel
    /// offers one, and the heap otherwise (also when a mapping fails).
    pub(crate) fn allocate(&self, len: usize) -> Storage {
        if self.mapped
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

    fn deliver(&self, done: UrbCompletion, packets: &[UrbPacket]) {
        let shared = self.lock_state().pending.remove(&done.tag);
        let Some(shared) = shared else {
            return;
        };
        shared.complete(outcome(done, packets, &self.packet_tables));
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
                    self.deliver(done, &packets);
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

    /// Delivers the completions ready now; returns how many.
    pub(crate) fn reap_now(&self) -> Result<usize> {
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
            if let Err(error) = poll_one(self.file.as_fd(), POLLOUT, Some(wait)) {
                return Err(self.reap_failed(&error));
            }
        }
    }

    /// A reaping error ends every transfer in flight (the kernel's completions
    /// can no longer be matched), and is the caller's error too.
    fn reap_failed(&self, error: &std::io::Error) -> Error {
        self.fail_all(error);
        Error::from_io("reaping USB transfers", error)
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
        match poll_one(usb.file.as_fd(), POLLOUT, Some(REAP_POLL)) {
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

fn outcome(
    done: UrbCompletion,
    urb_packets: &[UrbPacket],
    tables: &Spares<Vec<IsoPacket>>,
) -> Result<TransferOutcome> {
    // An isochronous URB can complete with a per-packet error status; its
    // packets say which ones failed. Only a status that ends the whole URB is
    // an error for the transfer.
    let isochronous = !urb_packets.is_empty();
    if done.status != 0 && !(isochronous && !is_fatal_for_packets(done.status)) {
        return Err(Error::from_status("USB transfer", done.status));
    }
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
        error:
            (packet.status != 0).then(|| Error::from_status("isochronous packet", packet.status)),
    }));
    Ok(TransferOutcome {
        buffer: crate::buffer::wrap(done.buffer),
        actual_length: done.actual_length,
        packets,
    })
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
        let out = outcome(completion(0, 5), &[], &tables).unwrap();
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
        let out = outcome(completion(0, 100), &urb, &tables).unwrap();
        assert_eq!(out.packets.len(), 2);
        assert!(out.packets[0].error.is_none());
        assert!(out.packets[1].error.is_some(), "a packet's own error");
        assert!(
            out.packets.capacity() >= 16,
            "the returned table was reused"
        );
        assert!(tables.is_empty(), "the table left the pool");
    }

    /// A status that ends the whole isochronous URB is the transfer's error.
    #[test]
    fn a_fatal_status_fails_the_isochronous_transfer() {
        let tables = Spares::new(4);
        let urb = [packet(100, 0, 0)];
        assert!(outcome(completion(-libc::ENODEV, 0), &urb, &tables).is_err());
    }
}
