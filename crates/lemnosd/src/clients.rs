//! Client connections: non-blocking sockets with a read buffer and a
//! bounded write queue.

use lemnos_ipc::{Message, Request, WireError, decode_request};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;

/// Bytes queued for a client before old readings are dropped.
const MAX_QUEUED: usize = 256 * 1024;

pub(crate) struct Client {
    pub id: u32,
    /// Owner id for LED intents: shared by connections with the same name.
    pub owner: u32,
    pub name: String,
    pub priority: u8,
    pub keep: bool,
    pub greeted: bool,
    pub stream: UnixStream,
    read: Vec<u8>,
    queue: VecDeque<(bool, Vec<u8>)>,
    queued: usize,
    written: usize,
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
            greeted: false,
            stream,
            read: Vec::with_capacity(1024),
            queue: VecDeque::new(),
            queued: 0,
            written: 0,
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

    /// Queues a message. Readings (`droppable`) are dropped, oldest first,
    /// when the client falls behind; other messages always go out.
    pub fn send(&mut self, message: &Message) {
        let droppable = matches!(message, Message::Reading(_));
        let frame = message.encode();
        self.queued += frame.len();
        self.queue.push_back((droppable, frame));
        while self.queued > MAX_QUEUED {
            // Never drop the frame being written (index 0 once started).
            let start = usize::from(self.written > 0);
            let Some(index) = self.queue.iter().skip(start).position(|(d, _)| *d) else {
                break;
            };
            if let Some((_, dropped)) = self.queue.remove(index + start) {
                self.queued -= dropped.len();
            }
        }
        self.flush();
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
