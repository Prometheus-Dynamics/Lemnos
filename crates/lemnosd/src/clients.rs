//! Client connections: non-blocking sockets with a read buffer and a
//! bounded write queue.

use lemnos_ipc::{Event, Message, Request, WireError, decode_request};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::rc::Rc;

/// Bytes queued for a client before old readings, then old events, are
/// dropped. Readings are superseded by the next one, so a client that is
/// this far behind is better off with the newest values than an old backlog.
const MAX_QUEUED: usize = 64 * 1024;
/// Bytes of answers (replies, frames, device lists: never dropped) queued for
/// a client before it is closed. A client this far behind has stopped
/// reading; without a bound its queue would grow the service's memory.
/// Several full-size frames, so a burst of large answers to a live client
/// still fits.
const MAX_ANSWERS_QUEUED: usize = 4 * lemnos_ipc::wire::MAX_FRAME;
/// The read buffer's starting size, and the size it returns to after a
/// request larger than `READ_SHRINK_ABOVE` (the largest request is ~70 KiB).
const READ_START: usize = 1024;
const READ_SHRINK_ABOVE: usize = 16 * 1024;
/// Written answer frames kept for the next answers, and the largest kept: a client's
/// replies cost no allocation once it has a few.
const SPARE_FRAMES: usize = 4;
const SPARE_MAX: usize = 4 * 1024;

/// What a request needs to know about its client.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Requester<'a> {
    pub id: u32,
    pub name: &'a str,
    pub keep: bool,
}

/// How a queued frame may be dropped when the client falls behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Readings: dropped first, silently (the next one supersedes them).
    Reading,
    /// Events: dropped next, counted and reported with `Event::Dropped`.
    Event,
    /// Replies and the rest: never dropped.
    Answer,
}

pub(crate) struct Client {
    pub id: u32,
    /// Owner id for LED intents: shared by connections with the same name.
    pub owner: u32,
    pub name: String,
    pub priority: u8,
    pub keep: bool,
    /// The client reads events (its greeting asked for them).
    pub events: bool,
    pub greeted: bool,
    pub stream: UnixStream,
    read: Vec<u8>,
    /// Frames waiting to be written. A reading's frame is shared with every
    /// client it goes to (see `send_reading`).
    queue: VecDeque<(Class, Rc<Vec<u8>>)>,
    queued: usize,
    written: usize,
    /// Events dropped and not yet reported.
    dropped: u32,
    /// Written frames only this client held, for its next answers.
    spare: Vec<Rc<Vec<u8>>>,
    pub closed: bool,
}

impl Client {
    pub fn new(id: u32, stream: UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            id,
            // Until `Hello` names it, a connection owns its intents alone: an
            // id outside the named owners' range (1, 2, ... from `owner_for`).
            owner: u32::MAX - id,
            name: String::new(),
            priority: 50,
            keep: false,
            events: true,
            greeted: false,
            stream,
            read: Vec::with_capacity(READ_START),
            queue: VecDeque::new(),
            queued: 0,
            written: 0,
            dropped: 0,
            spare: Vec::new(),
            closed: false,
        })
    }

    /// Reads what arrived; [`Self::next_request`] then hands out the complete requests.
    pub fn receive(&mut self) {
        let mut chunk = [0u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    self.closed = true;
                    break;
                }
                Ok(n) => self.read.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    self.closed = true;
                    break;
                }
            }
        }
    }

    /// The next complete request received, if any.
    pub fn next_request(&mut self) -> Result<Option<Request>, WireError> {
        let Some((request, used)) = decode_request(&self.read)? else {
            // A large request leaves a large buffer behind; give it back once
            // the buffer is empty again.
            if self.read.is_empty() && self.read.capacity() > READ_SHRINK_ABOVE {
                self.read = Vec::with_capacity(READ_START);
            }
            return Ok(None);
        };
        self.read.drain(..used);
        Ok(Some(request))
    }

    /// Queues a message. When the client falls behind, readings are
    /// dropped first (oldest first), then events (counted, and reported as
    /// `Event::Dropped` once the queue drains); answers always go out.
    pub fn send(&mut self, message: &Message) {
        let class = match message {
            Message::Reading(_) => Class::Reading,
            Message::Event(_) => Class::Event,
            _ => Class::Answer,
        };
        let mut frame = self.spare.pop().unwrap_or_default();
        match Rc::get_mut(&mut frame) {
            Some(buf) => message.encode_into(buf),
            None => frame = Rc::new(message.encode()),
        }
        self.enqueue(class, frame);
    }

    /// Queues an answer that `encode` writes into a reused frame (the
    /// frame's bytes are replaced).
    pub fn send_answer(&mut self, encode: impl FnOnce(&mut Vec<u8>)) {
        self.send_encoded(Class::Answer, encode);
    }

    /// Queues an event that `encode` writes into a reused frame.
    pub fn send_event(&mut self, encode: impl FnOnce(&mut Vec<u8>)) {
        self.send_encoded(Class::Event, encode);
    }

    fn send_encoded(&mut self, class: Class, encode: impl FnOnce(&mut Vec<u8>)) {
        let mut frame = self.spare.pop().unwrap_or_default();
        match Rc::get_mut(&mut frame) {
            Some(buf) => encode(buf),
            None => {
                let mut buf = Vec::new();
                encode(&mut buf);
                frame = Rc::new(buf);
            }
        }
        self.enqueue(class, frame);
    }

    /// Queues a reading whose frame (`lemnos_ipc::encode_reading`) the caller
    /// encoded once for every client that gets the same channels.
    pub fn send_reading(&mut self, frame: &Rc<Vec<u8>>) {
        self.enqueue(Class::Reading, Rc::clone(frame));
    }

    /// Queues a watched light's frame (`lemnos_ipc::encode_frame_into`) from the service's
    /// frame pool. Never dropped for a slow reader, as no answer is.
    pub fn send_frame(&mut self, frame: &Rc<Vec<u8>>) {
        self.enqueue(Class::Answer, Rc::clone(frame));
    }

    fn enqueue(&mut self, class: Class, frame: Rc<Vec<u8>>) {
        if self.closed {
            return;
        }
        self.queued += frame.len();
        self.queue.push_back((class, frame));
        for victim in [Class::Reading, Class::Event] {
            while self.queued > MAX_QUEUED {
                // Never drop the frame being written (index 0 once started).
                let start = usize::from(self.written > 0);
                let Some(index) = self
                    .queue
                    .iter()
                    .skip(start)
                    .position(|(c, _)| *c == victim)
                else {
                    break;
                };
                if let Some((_, dropped)) = self.queue.remove(index + start) {
                    self.queued -= dropped.len();
                    if victim == Class::Event {
                        self.dropped = self.dropped.saturating_add(1);
                    }
                }
            }
        }
        if self.queued > MAX_ANSWERS_QUEUED {
            self.closed = true;
            self.queue.clear();
            self.queued = 0;
            return;
        }
        self.flush();
        if self.dropped > 0 && self.queued < MAX_QUEUED / 2 {
            let count = std::mem::take(&mut self.dropped);
            self.enqueue(
                Class::Answer,
                Rc::new(Message::Event(Event::Dropped { count }).encode()),
            );
            self.flush();
        }
    }

    /// Writes as much as the socket takes.
    pub fn flush(&mut self) {
        while let Some((_, frame)) = self.queue.front() {
            match self.stream.write(&frame[self.written..]) {
                Ok(n) => {
                    self.written += n;
                    if self.written == frame.len() {
                        self.queued -= frame.len();
                        self.written = 0;
                        if let Some((_, frame)) = self.queue.pop_front()
                            && Rc::strong_count(&frame) == 1
                            && frame.capacity() <= SPARE_MAX
                            && self.spare.len() < SPARE_FRAMES
                        {
                            self.spare.push(frame);
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    self.closed = true;
                    break;
                }
            }
        }
    }

    pub fn wants_write(&self) -> bool {
        !self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_device::{DeviceStatus, NO_VALUE};
    use lemnos_ipc::{RawReading, decode_message, encode_reading};

    #[test]
    fn a_shared_reading_frame_is_the_reading_message() {
        let (server, mut peer) = UnixStream::pair().unwrap();
        let mut first = Client::new(1, server).unwrap();
        let (server, _peer2) = UnixStream::pair().unwrap();
        let mut second = Client::new(2, server).unwrap();
        let values = [9_806, NO_VALUE, -3];
        let message = Message::Reading(RawReading {
            device: "imu".into(),
            timestamp_us: 7,
            status: DeviceStatus::Available,
            values: values.to_vec(),
            id: 0,
        });
        // One encoding for two clients: the first client's bytes are what the message encodes to.
        let frame = Rc::new(encode_reading("imu", 7, DeviceStatus::Available, &values));
        first.send_reading(&frame);
        second.send_reading(&frame);
        peer.set_nonblocking(true).unwrap();
        let mut received = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = peer.read(&mut buf) {
            if n == 0 {
                break;
            }
            received.extend_from_slice(&buf[..n]);
        }
        assert_eq!(
            decode_message(&received).unwrap(),
            Some((message, received.len()))
        );
    }

    #[test]
    fn events_a_client_does_not_read_are_dropped_and_counted() {
        let (server, mut peer) = UnixStream::pair().unwrap();
        let mut client = Client::new(1, server).unwrap();
        let event = Message::Event(Event::Control {
            device: "fan".into(),
            control: "duty".into(),
            value: 0.5,
            by: "x".repeat(200),
        });
        for _ in 0..10_000 {
            client.send(&event);
        }
        assert!(
            client.queued <= MAX_QUEUED + 1024,
            "bounded: {}",
            client.queued
        );
        assert!(client.dropped > 0);
        // The peer catches up; the next message reports the drops.
        peer.set_nonblocking(true).unwrap();
        let mut received = Vec::new();
        let mut buf = [0u8; 65536];
        let mut drained = |client: &mut Client, received: &mut Vec<u8>| loop {
            client.flush();
            match peer.read(&mut buf) {
                Ok(n) if n > 0 => received.extend_from_slice(&buf[..n]),
                _ if !client.wants_write() => break,
                _ => {}
            }
        };
        drained(&mut client, &mut received);
        client.send(&Message::Reply {
            id: 1,
            result: Ok(0.0),
        });
        drained(&mut client, &mut received);
        let mut dropped = None;
        let mut rest = &received[..];
        while let Some((message, used)) = decode_message(rest).unwrap() {
            if let Message::Event(Event::Dropped { count }) = message {
                dropped = Some(count);
            }
            rest = &rest[used..];
        }
        assert!(dropped.is_some_and(|n| n > 0));
    }
}
