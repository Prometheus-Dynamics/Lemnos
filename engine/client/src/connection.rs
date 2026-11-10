//! One socket and its two buffers.

use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use lemnos_wire::MAX_FRAME;

use crate::stream::{MAX_DESCRIPTORS, StreamPlatform};

const RX_START: usize = 16 * 1024;
/// More descriptors than this waiting means the peer isn't following the protocol.
const MAX_WAITING_FDS: usize = 2 * MAX_DESCRIPTORS;

/// While a stream is being opened, the socket is read so that descriptors aren't lost.
pub(crate) struct Taker {
    pub(crate) platform: Arc<dyn StreamPlatform>,
    pub(crate) fds: Vec<OwnedFd>,
}

impl core::fmt::Debug for Taker {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Taker")
            .field("fds", &self.fds.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(crate) struct Connection {
    pub(crate) stream: UnixStream,
    pub(crate) rx: Vec<u8>,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) tx: Vec<u8>,
    pub(crate) sent: usize,
    wait: Option<Duration>,
    pub(crate) taker: Option<Taker>,
}

impl Connection {
    pub(crate) fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            rx: vec![0; RX_START],
            start: 0,
            end: 0,
            tx: Vec::with_capacity(RX_START),
            sent: 0,
            wait: None,
            taker: None,
        }
    }

    /// Sets how long a blocking read waits. Costs a system call only when it
    /// changes.
    pub(crate) fn set_wait(&mut self, wait: Duration) -> io::Result<()> {
        let wait = wait.max(Duration::from_millis(1));
        if self.wait != Some(wait) {
            self.stream.set_read_timeout(Some(wait))?;
            self.wait = Some(wait);
        }
        Ok(())
    }

    /// Writes what it can. What's left goes on the next call.
    pub(crate) fn flush(&mut self) -> io::Result<()> {
        while self.sent < self.tx.len() {
            match self.stream.write(&self.tx[self.sent..]) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(n) => self.sent += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
        self.tx.clear();
        self.sent = 0;
        Ok(())
    }

    pub(crate) const fn unsent(&self) -> usize {
        self.tx.len() - self.sent
    }

    /// Reads once. Returns the bytes read, 0 when nothing is there yet.
    pub(crate) fn fill(&mut self) -> io::Result<usize> {
        if self.start == self.end {
            self.start = 0;
            self.end = 0;
        } else if self.end == self.rx.len() {
            self.rx.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
            if self.end == self.rx.len() {
                let grown = (self.rx.len() * 2).min(MAX_FRAME);
                self.rx.resize(grown, 0);
            }
        }
        let rx = &mut self.rx[self.end..];
        let read = match &mut self.taker {
            None => self.stream.read(rx),
            Some(taker) => {
                let read =
                    taker
                        .platform
                        .recv_with_descriptors(self.stream.as_fd(), rx, &mut taker.fds);
                if taker.fds.len() > MAX_WAITING_FDS {
                    // Dropped with the connection, which this error ends.
                    return Err(ErrorKind::InvalidData.into());
                }
                read
            }
        };
        match read {
            Ok(0) => Err(ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                self.end += n;
                Ok(n)
            }
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) =>
            {
                Ok(0)
            }
            Err(e) => Err(e),
        }
    }
}
