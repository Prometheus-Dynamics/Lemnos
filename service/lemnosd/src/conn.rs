//! One client: what's waiting to be written to it. The serving thread does the writing; other
//! threads queue here and wake it.

use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lemnos_core::CallerId;
use lemnos_wire::{Frame, MAX_QUEUED_BYTES, append};

/// Values stop being queued here. Replies have the rest of the room.
const VALUE_LIMIT: usize = 1 << 20;
pub(crate) const OUTBOX_START: usize = 16 * 1024;

/// Wakes the serving thread from another thread.
pub(crate) struct Waker {
    tx: UnixStream,
    armed: AtomicBool,
}

impl Waker {
    pub(crate) const fn new(tx: UnixStream) -> Self {
        Self {
            tx,
            armed: AtomicBool::new(false),
        }
    }

    pub(crate) fn wake(&self) {
        if !self.armed.swap(true, Ordering::AcqRel) {
            // A full socket already holds a wake-up.
            let _ = (&self.tx).write(&[1]);
        }
    }

    /// Lets the next `wake` write again. The serving thread calls this before it looks at the
    /// queues, so nothing queued afterwards goes unnoticed.
    pub(crate) fn clear(&self) {
        self.armed.store(false, Ordering::Release);
    }
}

struct Outbox {
    bytes: Vec<u8>,
    closed: bool,
}

pub(crate) struct Conn {
    stream: UnixStream,
    outbox: Mutex<Outbox>,
    waker: Arc<Waker>,
    pub(crate) events: AtomicBool,
    /// Subscriptions this client holds.
    pub(crate) subscriptions: Mutex<Vec<u64>>,
    /// Who this client is to a driver.
    pub(crate) id: CallerId,
    /// Drivers have been told this client has gone.
    pub(crate) gone: AtomicBool,
}

/// The next client's number. Starts above [`CallerId::LOCAL`] and is never reused.
static NEXT_CALLER: AtomicU64 = AtomicU64::new(1);

impl Conn {
    pub(crate) fn new(stream: UnixStream, waker: Arc<Waker>) -> Self {
        Self {
            stream,
            outbox: Mutex::new(Outbox {
                bytes: Vec::with_capacity(OUTBOX_START),
                closed: false,
            }),
            waker,
            events: AtomicBool::new(false),
            subscriptions: Mutex::new(Vec::new()),
            id: CallerId(NEXT_CALLER.fetch_add(1, Ordering::Relaxed)),
            gone: AtomicBool::new(false),
        }
    }

    pub(crate) const fn stream(&self) -> &UnixStream {
        &self.stream
    }

    fn lock(&self) -> MutexGuard<'_, Outbox> {
        self.outbox.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues an answer on the serving thread, which writes it before it next waits. A client that
    /// lets too many pile up is closed: it isn't reading, and answers can't be dropped.
    pub(crate) fn answer(&self, frame: &Frame<'_>) {
        let mut outbox = self.lock();
        if outbox.closed {
            return;
        }
        if outbox.bytes.len() > MAX_QUEUED_BYTES || append(frame, &mut outbox.bytes).is_err() {
            drop(outbox);
            self.close();
        }
    }

    /// Queues an answer from another thread.
    pub(crate) fn event(&self, frame: &Frame<'_>) {
        self.answer(frame);
        self.waker.wake();
    }

    /// Queues a value from another thread, unless this client has fallen behind.
    pub(crate) fn value(&self, frame: &Frame<'_>) {
        let mut outbox = self.lock();
        if outbox.closed || outbox.bytes.len() > VALUE_LIMIT {
            return;
        }
        if append(frame, &mut outbox.bytes).is_ok() {
            drop(outbox);
            self.waker.wake();
        }
    }

    /// Swaps the queued bytes for the empty `sending`. False when nothing is queued.
    pub(crate) fn take_queued(&self, sending: &mut Vec<u8>) -> bool {
        let mut outbox = self.lock();
        if outbox.bytes.is_empty() {
            return false;
        }
        core::mem::swap(&mut outbox.bytes, sending);
        true
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Ends the connection.
    pub(crate) fn close(&self) {
        self.lock().closed = true;
        let _ = self.stream.shutdown(Shutdown::Both);
        self.waker.wake();
    }

    pub(crate) fn wants_events(&self) -> bool {
        self.events.load(Ordering::Relaxed)
    }
}
