//! One client: what's waiting to be written to it, and its writer thread.

use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lemnos_wire::{Frame, MAX_QUEUED_BYTES, append};

/// Values stop being queued here. Replies have the rest of the room.
const VALUE_LIMIT: usize = 1 << 20;
const OUTBOX_START: usize = 16 * 1024;
const FINISH_WAIT: Duration = Duration::from_millis(200);

struct Outbox {
    bytes: Vec<u8>,
    closed: bool,
    /// The writer holds bytes it hasn't finished writing.
    writing: bool,
}

pub(crate) struct Conn {
    stream: UnixStream,
    outbox: Mutex<Outbox>,
    changed: Condvar,
    pub(crate) events: AtomicBool,
    /// Subscriptions this client holds.
    pub(crate) subscriptions: Mutex<Vec<u64>>,
}

impl Conn {
    pub(crate) fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            outbox: Mutex::new(Outbox {
                bytes: Vec::with_capacity(OUTBOX_START),
                closed: false,
                writing: false,
            }),
            changed: Condvar::new(),
            events: AtomicBool::new(false),
            subscriptions: Mutex::new(Vec::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Outbox> {
        self.outbox.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues an answer. A client that lets too many pile up is closed: it
    /// isn't reading, and answers can't be dropped.
    pub(crate) fn answer(&self, frame: &Frame<'_>) {
        let mut outbox = self.lock();
        if outbox.closed {
            return;
        }
        if outbox.bytes.len() > MAX_QUEUED_BYTES || append(frame, &mut outbox.bytes).is_err() {
            drop(outbox);
            self.close();
            return;
        }
        self.changed.notify_all();
    }

    /// Queues a value, unless this client has fallen behind.
    pub(crate) fn value(&self, frame: &Frame<'_>) {
        let mut outbox = self.lock();
        if outbox.closed || outbox.bytes.len() > VALUE_LIMIT {
            return;
        }
        if append(frame, &mut outbox.bytes).is_ok() {
            self.changed.notify_all();
        }
    }

    /// Gives what's queued a moment to reach the client, then closes. A last
    /// answer, such as why the connection is ending, still arrives.
    pub(crate) fn finish(&self) {
        let until = Instant::now() + FINISH_WAIT;
        let mut outbox = self.lock();
        while (!outbox.bytes.is_empty() || outbox.writing) && !outbox.closed {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            outbox = self
                .changed
                .wait_timeout(outbox, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        drop(outbox);
        self.close();
    }

    /// Ends the connection. Both of its threads stop.
    pub(crate) fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    /// Writes until the connection closes.
    pub(crate) fn write_loop(&self, mut stream: UnixStream) {
        let mut sending = Vec::with_capacity(OUTBOX_START);
        loop {
            {
                let mut outbox = self.lock();
                while outbox.bytes.is_empty() && !outbox.closed {
                    outbox = self
                        .changed
                        .wait(outbox)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                if outbox.closed {
                    return;
                }
                core::mem::swap(&mut outbox.bytes, &mut sending);
                outbox.writing = true;
            }
            if stream.write_all(&sending).is_err() {
                self.close();
                return;
            }
            sending.clear();
            self.lock().writing = false;
            self.changed.notify_all();
        }
    }

    pub(crate) fn wants_events(&self) -> bool {
        self.events.load(Ordering::Relaxed)
    }
}
