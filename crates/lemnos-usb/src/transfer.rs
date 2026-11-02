//! Asynchronous transfers: a future for each submitted transfer, which also
//! blocks for callers on plain threads.

use crate::error::{Error, ErrorKind, Result};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

/// One packet of an isochronous transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsoPacket {
    /// The length the packet was scheduled for.
    pub length: usize,
    /// The bytes that moved.
    pub actual_length: usize,
    /// What went wrong with this packet, if anything.
    pub error: Option<Error>,
}

/// A finished transfer: its buffer, back with the data it received (IN) or
/// the data it sent (OUT), and the packet results for isochronous transfers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferOutcome {
    /// The buffer the transfer was submitted with.
    pub buffer: Vec<u8>,
    /// The bytes that moved (IN: the bytes received at the start of `buffer`).
    pub actual_length: usize,
    /// The isochronous packets (empty for control, bulk and interrupt).
    pub packets: Vec<IsoPacket>,
}

impl TransferOutcome {
    /// The bytes that moved: the start of the buffer, `actual_length` long.
    pub fn data(&self) -> &[u8] {
        &self.buffer[..self.actual_length.min(self.buffer.len())]
    }

    /// The buffer, to submit again without allocating.
    pub fn into_buffer(self) -> Vec<u8> {
        self.buffer
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
