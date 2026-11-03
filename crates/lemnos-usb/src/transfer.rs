//! Asynchronous transfers: a future for each submitted transfer, which also
//! blocks for callers on plain threads.

use crate::error::{Error, ErrorKind, Result};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

/// How a transfer ended. A copy value: a completion never builds a message. The
/// [`TransferStatus::to_error`] form builds one on request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransferStatus {
    /// The transfer completed (for an isochronous one, see each packet's status).
    Ok,
    /// Cancelled, or discarded by the kernel (`ENOENT`, `ECONNRESET`, `ECANCELED`).
    Cancelled,
    /// The device went away (`ESHUTDOWN`, `ENODEV`): reconnect, do not retry the
    /// same device handle.
    Disconnected,
    /// The endpoint stalled (`EPIPE`): clear the halt, then retry.
    Stalled,
    /// Any other failure: the errno on Linux, or 0 where the platform gives none.
    Error(i32),
}

impl TransferStatus {
    /// Whether the transfer completed.
    pub fn is_ok(self) -> bool {
        self == Self::Ok
    }

    /// The status of a completed URB (`status` as the kernel reports it: zero, or
    /// a negative errno).
    #[cfg(target_os = "linux")]
    pub(crate) fn from_kernel(status: i32) -> Self {
        if status == 0 {
            return Self::Ok;
        }
        match -status {
            libc::ECANCELED | libc::ENOENT | libc::ECONNRESET => Self::Cancelled,
            libc::ESHUTDOWN | libc::ENODEV => Self::Disconnected,
            libc::EPIPE => Self::Stalled,
            errno => Self::Error(errno),
        }
    }

    /// The status of a completed nusb transfer (Windows and macOS).
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn from_nusb(error: &nusb::transfer::TransferError) -> Self {
        use nusb::transfer::TransferError;
        match error {
            TransferError::Cancelled => Self::Cancelled,
            TransferError::Stall => Self::Stalled,
            TransferError::Disconnected => Self::Disconnected,
            _ => Self::Error(0),
        }
    }

    /// The error this status stands for, with `context` in its message. Built
    /// only when asked: `None` for [`TransferStatus::Ok`].
    pub fn to_error(self, context: &str) -> Option<Error> {
        Some(match self {
            Self::Ok => return None,
            Self::Cancelled => Error::new(ErrorKind::Cancelled, format!("{context}: cancelled")),
            Self::Disconnected => Error::new(
                ErrorKind::Disconnected,
                format!("{context}: device disconnected"),
            ),
            Self::Stalled => Error::new(ErrorKind::Stall, format!("{context}: endpoint stalled")),
            Self::Error(code) => error_from_code(context, code),
        })
    }
}

#[cfg(target_os = "linux")]
fn error_from_code(context: &str, errno: i32) -> Error {
    Error::from_status(context, -errno)
}

#[cfg(not(target_os = "linux"))]
fn error_from_code(context: &str, code: i32) -> Error {
    Error::new(ErrorKind::Other, format!("{context}: failed (code {code})"))
}

/// One packet of an isochronous transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsoPacket {
    /// The length the packet was scheduled for.
    pub length: usize,
    /// The bytes that moved.
    pub actual_length: usize,
    /// How the packet ended (a packet can fail while the transfer completes).
    pub status: TransferStatus,
}

/// A transfer's memory: the bytes it moves. On Linux it is a page-aligned
/// usbdevfs mapping when the kernel offers one, and a heap vector otherwise.
pub struct TransferBuffer {
    pub(crate) storage: crate::buffer::Storage,
}

impl TransferBuffer {
    /// A buffer over `bytes` (the heap path, on every platform).
    pub fn from_vec(bytes: Vec<u8>) -> Self {
        crate::buffer::wrap(crate::buffer::heap(bytes))
    }

    /// Bytes in use.
    pub fn len(&self) -> usize {
        self.storage.len()
    }

    /// Whether no bytes are in use.
    pub fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }

    /// Bytes available for a transfer (a mapping's whole length, or a vector's).
    pub fn capacity(&self) -> usize {
        self.storage.capacity()
    }

    /// Whether the memory is a usbdevfs mapping rather than a heap vector.
    pub fn is_mapped(&self) -> bool {
        self.storage.is_mapped()
    }

    /// The bytes in use.
    pub fn as_slice(&self) -> &[u8] {
        self.storage.as_slice()
    }

    /// The bytes in use, for writing before the buffer is submitted.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.storage.as_mut_slice()
    }

    /// Sets the bytes in use (at most the capacity).
    pub fn set_len(&mut self, len: usize) {
        self.storage.set_len(len);
    }

    /// The bytes as a heap vector: the buffer itself, or a copy of a mapping's bytes.
    pub fn into_vec(self) -> Vec<u8> {
        self.storage.into_vec()
    }
}

impl std::fmt::Debug for TransferBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransferBuffer")
            .field("len", &self.len())
            .field("mapped", &self.is_mapped())
            .finish()
    }
}

/// A finished transfer: its buffer, back with the data it received (IN) or
/// the data it sent (OUT), and the packet results for isochronous transfers.
///
/// The buffer comes back whatever the status: a stalled or disconnected transfer
/// returns its memory too, so a caller that reuses buffers does not allocate
/// after an error.
#[derive(Debug)]
pub struct TransferOutcome {
    /// The buffer the transfer was submitted with.
    pub buffer: TransferBuffer,
    /// The bytes that moved (IN: the bytes received at the start of `buffer`).
    pub actual_length: usize,
    /// The isochronous packets (empty for control, bulk and interrupt).
    pub packets: Vec<IsoPacket>,
    /// How the transfer ended. `Ok` moves the bytes; anything else is a failure
    /// of the whole transfer (see [`TransferStatus`]).
    pub status: TransferStatus,
    /// When the completion was reaped from the kernel, before any waiter was
    /// woken: the moment the device finished, not when the caller ran.
    pub reaped: Instant,
}

impl TransferOutcome {
    /// The bytes that moved: the start of the buffer, `actual_length` long.
    pub fn data(&self) -> &[u8] {
        &self.buffer.as_slice()[..self.actual_length.min(self.buffer.len())]
    }

    /// The buffer, to submit again without allocating.
    pub fn into_buffer(self) -> TransferBuffer {
        self.buffer
    }

    /// The buffer and the packet table, both to reuse.
    #[cfg(target_os = "linux")]
    pub(crate) fn into_parts(self) -> (TransferBuffer, Vec<IsoPacket>) {
        (self.buffer, self.packets)
    }
}

/// How a backend stops a transfer it has in flight.
pub(crate) trait Canceller: Send + Sync {
    fn cancel(&self);
}

/// The state a backend completes and a transfer waits on.
#[derive(Default)]
pub(crate) struct Shared {
    state: Mutex<Slot>,
    done: Condvar,
}

#[derive(Default)]
struct Slot {
    result: Option<Result<TransferOutcome>>,
    waker: Option<Waker>,
}

impl Shared {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Records the result and wakes whoever waits for it.
    pub(crate) fn complete(&self, result: Result<TransferOutcome>) {
        let waker = {
            let mut slot = self.lock();
            slot.result = Some(result);
            slot.waker.take()
        };
        self.done.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A transfer in flight. Await it (it is a future), or [`Transfer::wait`] for
/// it on this thread. Dropping it does not cancel the transfer: the backend
/// still owns the buffer until the device is done with it.
pub struct Transfer {
    shared: Arc<Shared>,
    canceller: Option<Box<dyn Canceller>>,
}

impl Transfer {
    pub(crate) fn new(shared: Arc<Shared>, canceller: Option<Box<dyn Canceller>>) -> Self {
        Self { shared, canceller }
    }

    /// Runs `work` on a new thread and returns the transfer it completes. The
    /// transfer has no canceller: the work runs until it returns.
    pub(crate) fn from_worker(
        work: impl FnOnce() -> Result<TransferOutcome> + Send + 'static,
    ) -> Result<Self> {
        let shared = Shared::new();
        let done = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("lemnos-usb-transfer".into())
            .spawn(move || done.complete(work()))
            .map_err(|error| {
                Error::new(
                    ErrorKind::Other,
                    format!("starting a USB transfer thread: {error}"),
                )
            })?;
        Ok(Self::new(shared, None))
    }

    /// Whether the transfer has finished (its result is ready to take).
    pub fn is_complete(&self) -> bool {
        self.shared.lock().result.is_some()
    }

    /// Blocks until the transfer finishes.
    pub fn wait(self) -> Result<TransferOutcome> {
        let mut slot = self.shared.lock();
        loop {
            if let Some(result) = slot.result.take() {
                return result;
            }
            slot = self
                .shared
                .done
                .wait(slot)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Waits until the transfer completes or `timeout` passes (`None`: forever),
    /// without taking its result. True when the result is ready to take. This
    /// waits on the transfer alone: a caller-driven device reaps with
    /// [`crate::UsbDevice::reap_until`], not here.
    #[cfg(target_os = "linux")]
    pub(crate) fn wait_complete(&self, timeout: Option<Duration>) -> bool {
        let deadline = timeout.map(|limit| Instant::now() + limit);
        let mut slot = self.shared.lock();
        loop {
            if slot.result.is_some() {
                return true;
            }
            match deadline {
                None => {
                    slot = self
                        .shared
                        .done
                        .wait(slot)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return false;
                    }
                    let (guard, _) = self
                        .shared
                        .done
                        .wait_timeout(slot, deadline - now)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    slot = guard;
                }
            }
        }
    }

    /// Blocks until the transfer finishes or `timeout` passes. On timeout the
    /// transfer is returned, still in flight.
    pub fn wait_timeout(
        self,
        timeout: Duration,
    ) -> std::result::Result<Result<TransferOutcome>, Self> {
        let deadline = std::time::Instant::now() + timeout;
        let mut slot = self.shared.lock();
        loop {
            if let Some(result) = slot.result.take() {
                return Ok(result);
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                drop(slot);
                return Err(self);
            }
            let (guard, _) = self
                .shared
                .done
                .wait_timeout(slot, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            slot = guard;
        }
    }

    /// Asks the backend to cancel the transfer. The transfer then completes
    /// with [`crate::ErrorKind::Cancelled`] (or with its data, when it finished first).
    /// On Windows and macOS this cancels every transfer queued on the endpoint.
    pub fn cancel(&self) {
        if let Some(canceller) = &self.canceller {
            canceller.cancel();
        }
    }
}

impl Future for Transfer {
    type Output = Result<TransferOutcome>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut slot = self.shared.lock();
        match slot.result.take() {
            Some(result) => Poll::Ready(result),
            None => {
                slot.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}
