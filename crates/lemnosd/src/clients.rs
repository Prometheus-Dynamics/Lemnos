//! Client connections: non-blocking sockets with a read buffer and a
//! bounded write queue.

use lemnos_ipc::{Event, Message, Request, WireError, decode_request};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;

/// Bytes queued for a client before old readings, then old events, are
/// dropped.
const MAX_QUEUED: usize = 256 * 1024;

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
    queue: VecDeque<(Class, Vec<u8>)>,
    queued: usize,
    written: usize,
    /// Events dropped and not yet reported.
    dropped: u32,
    pub closed: bool,
}

impl Client {
    pub fn new(id: u32, stream: UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            id,
            owner: id,
            name: String::new(),
            priority: 50,
            keep: false,
            events: true,
            greeted: false,
            stream,
            read: Vec::with_capacity(1024),
            queue: VecDeque::new(),
            queued: 0,
            written: 0,
            dropped: 0,
            closed: false,
        })
    }

    /// Reads what arrived; returns the complete requests.
    pub fn receive(&mut self) -> Result<Vec<Request>, WireError> {
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
        let mut requests = Vec::new();
        while let Some((request, used)) = decode_request(&self.read)? {
            self.read.drain(..used);
            requests.push(request);
        }
        Ok(requests)
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
        let frame = message.encode();
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
        self.flush();
        if self.dropped > 0 && self.queued < MAX_QUEUED / 2 {
            let count = std::mem::take(&mut self.dropped);
            let frame = Message::Event(Event::Dropped { count }).encode();
            self.queued += frame.len();
            self.queue.push_back((Class::Answer, frame));
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
                        self.queue.pop_front();
                        self.written = 0;
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
    use lemnos_ipc::decode_message;

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
