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
use lemnos_linux_sys::usbfs::{ControlSetup, UrbCompletion, UrbKind, UsbfsFile};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// How long the reaper waits for a completion before checking whether any
/// transfer is still in flight.
const REAP_POLL: Duration = Duration::from_millis(100);

/// `GET_CONFIGURATION` (USB 2.0 chapter 9).
const GET_CONFIGURATION: u8 = 0x08;

#[derive(Default)]
struct State {
    pending: HashMap<u64, Arc<Shared>>,
    reaping: bool,
}

/// An open usbdevfs device and its asynchronous transfers.
pub(crate) struct Usbfs {
    file: UsbfsFile,
    state: Mutex<State>,
    next_tag: AtomicU64,
    /// Whether the kernel maps DMA buffers for this device (`USBDEVFS_CAP_MMAP`).
    mapped: bool,
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
            if !state.reaping {
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

    fn deliver(&self, done: UrbCompletion) {
        let shared = self.lock_state().pending.remove(&done.tag);
        let Some(shared) = shared else {
            return;
        };
        shared.complete(outcome(done));
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
        loop {
            match usb.file.reap(false) {
                Ok(Some(done)) => usb.deliver(done),
                Ok(None) => break,
                Err(error) => {
                    usb.fail_all(&error);
                    return;
                }
            }
        }
    }
}

fn outcome(done: UrbCompletion) -> Result<TransferOutcome> {
    // An isochronous URB can complete with a per-packet error status; its
    // packets say which ones failed. Only a status that ends the whole URB is
    // an error for the transfer.
    let isochronous = !done.packets.is_empty();
    if done.status != 0 && !(isochronous && !is_fatal_for_packets(done.status)) {
        return Err(Error::from_status("USB transfer", done.status));
    }
    let packets = done
        .packets
        .iter()
        .map(|packet| IsoPacket {
            length: packet.length as usize,
            actual_length: packet.actual_length as usize,
            error: (packet.status != 0)
                .then(|| Error::from_status("isochronous packet", packet.status)),
        })
        .collect();
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
