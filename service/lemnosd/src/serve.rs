//! The serving thread: one poll over the socket, the wake-up and every client.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_linux_sys::poll::{POLLIN, POLLOUT, PollFd, poll_many};
use lemnos_linux_sys::scm::send_with_fds;

use crate::conn::{Attachment, Conn, OUTBOX_START};
use crate::requests::Client;
use crate::{Core, lock};

/// How long a closing client's last answers have to reach it.
const FINISH_WAIT: Duration = Duration::from_millis(200);
/// How long to back off when `accept` fails, for example out of descriptors.
const ACCEPT_RETRY: Duration = Duration::from_millis(10);
/// The listener and the wake-up come first in the poll set.
const FIXED: usize = 2;

struct Slot {
    conn: Arc<Conn>,
    client: Client,
    /// Bytes being written, and how many have gone.
    sending: Vec<u8>,
    sent: usize,
    /// Descriptors that go with bytes in `sending`.
    attachments: VecDeque<Attachment>,
    /// Set once the client is done: nothing more is read, and what is queued goes out by then.
    finish_by: Option<Instant>,
}

impl Slot {
    fn new(core: &Arc<Core>, stream: UnixStream) -> Self {
        let conn = Arc::new(Conn::new(stream, Arc::clone(&core.waker)));
        lock(&core.clients).push(Arc::clone(&conn));
        Self {
            client: Client::new(Arc::clone(core), Arc::clone(&conn)),
            conn,
            sending: Vec::with_capacity(OUTBOX_START),
            sent: 0,
            attachments: VecDeque::new(),
            finish_by: None,
        }
    }

    fn poll_entry(&self) -> PollFd {
        let mut events = 0;
        if self.finish_by.is_none() {
            events |= POLLIN;
        }
        if self.sent < self.sending.len() {
            events |= POLLOUT;
        }
        PollFd::new(self.conn.stream().as_fd(), events)
    }

    fn begin_finish(&mut self) {
        if self.finish_by.is_none() {
            self.finish_by = Some(Instant::now() + FINISH_WAIT);
            self.client.release();
        }
    }

    /// Writes until the socket is full or nothing is left.
    fn flush(&mut self) {
        loop {
            if self.sent == self.sending.len() {
                self.sending.clear();
                self.sent = 0;
                self.attachments.clear();
                if !self
                    .conn
                    .take_queued(&mut self.sending, &mut self.attachments)
                {
                    return;
                }
            }
            let attached = self.attachments.front().filter(|a| a.at == self.sent);
            let result = match attached {
                Some(attachment) => {
                    let handles = attachment.handles.iter().flat_map(|h| h.iter());
                    let borrowed: Vec<BorrowedFd<'_>> = attachment
                        .fds
                        .iter()
                        .chain(handles)
                        .map(AsFd::as_fd)
                        .collect();
                    let sent = send_with_fds(
                        self.conn.stream().as_fd(),
                        &self.sending[self.sent..],
                        &borrowed,
                    );
                    if sent.is_ok() {
                        // The kernel has its own references now.
                        self.attachments.pop_front();
                    }
                    sent
                }
                None => {
                    // Stop before the next attachment, so its bytes go with it.
                    let end = self
                        .attachments
                        .front()
                        .map_or(self.sending.len(), |a| a.at.min(self.sending.len()));
                    self.conn.stream().write(&self.sending[self.sent..end])
                }
            };
            match result {
                Ok(0) => return self.conn.close(),
                Ok(n) => self.sent += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => return,
                Err(_) => return self.conn.close(),
            }
        }
    }

    /// Whether this client is finished with and can go.
    fn is_done(&self, now: Instant) -> bool {
        if self.conn.is_closed() {
            return true;
        }
        self.finish_by
            .is_some_and(|by| self.sent == self.sending.len() || now >= by)
    }
}

fn accept(core: &Arc<Core>, listener: &UnixListener, slots: &mut Vec<Slot>) -> bool {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // A client that can't be set up costs that client, not the service.
                if stream.set_nonblocking(true).is_ok() {
                    slots.push(Slot::new(core, stream));
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => return true,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
}

fn drain(mut wake: &UnixStream) {
    let mut bytes = [0u8; 64];
    while matches!(wake.read(&mut bytes), Ok(n) if n > 0) {}
}

fn remove_finished(core: &Core, slots: &mut Vec<Slot>) {
    let now = Instant::now();
    slots.retain(|slot| {
        if !slot.is_done(now) {
            return true;
        }
        slot.client.release();
        slot.conn.close();
        lock(&core.clients).retain(|c| !Arc::ptr_eq(c, &slot.conn));
        false
    });
}

fn wait_time(slots: &[Slot], accept_failed: bool) -> Option<Duration> {
    let now = Instant::now();
    let finish = slots
        .iter()
        .filter_map(|slot| slot.finish_by)
        .min()
        .map(|by| by.saturating_duration_since(now));
    let retry = accept_failed.then_some(ACCEPT_RETRY);
    match (finish, retry) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Serves until the service stops.
pub(crate) fn run(core: &Arc<Core>, listener: &UnixListener, wake: &UnixStream) {
    let mut slots: Vec<Slot> = Vec::new();
    let mut fds: Vec<PollFd> = Vec::new();
    let mut accept_failed = false;
    while !core.stopping() {
        fds.clear();
        // After a failed accept the listener stays readable, so leave it out until the retry.
        let listen_events = if accept_failed { 0 } else { POLLIN };
        fds.push(PollFd::new(listener.as_fd(), listen_events));
        fds.push(PollFd::new(wake.as_fd(), POLLIN));
        fds.extend(slots.iter().map(Slot::poll_entry));
        let timeout = wait_time(&slots, accept_failed);
        if poll_many(&mut fds, timeout).is_err() {
            std::thread::sleep(ACCEPT_RETRY);
            continue;
        }
        if fds[1].revents() != 0 {
            drain(wake);
        }
        core.waker.clear();

        // By position, before anything changes the list.
        for (slot, entry) in slots.iter_mut().zip(&fds[FIXED..]) {
            if entry.revents() != 0
                && slot.finish_by.is_none()
                && !slot.client.read_ready(slot.conn.stream())
            {
                slot.begin_finish();
            }
        }
        accept_failed = !accept(core, listener, &mut slots);
        for slot in &mut slots {
            slot.flush();
        }
        remove_finished(core, &mut slots);
    }
}
