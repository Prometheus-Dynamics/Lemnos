//! Buffered bulk streams: several transfers in flight, so the device never
//! waits for the host between them. Both are blocking `std::io` adapters.

use crate::device::UsbDevice;
use crate::error::{Error, Result};
use crate::transfer::{Transfer, TransferOutcome};
use std::collections::VecDeque;
use std::io::{self, Read, Write};

fn to_io(error: Error) -> io::Error {
    io::Error::other(error)
}

/// Reads a bulk IN endpoint as a stream of bytes.
pub struct BulkReader {
    device: UsbDevice,
    endpoint: u8,
    chunk: usize,
    depth: usize,
    queue: VecDeque<Transfer>,
    spare: Vec<Vec<u8>>,
    current: Option<TransferOutcome>,
    position: usize,
}

impl BulkReader {
    pub(crate) fn new(device: UsbDevice, endpoint: u8, chunk: usize, depth: usize) -> Self {
        Self {
            device,
            endpoint,
            chunk: chunk.max(1),
            depth: depth.max(1),
            queue: VecDeque::new(),
            spare: Vec::new(),
            current: None,
            position: 0,
        }
    }

    /// Keeps `depth` transfers in flight.
    fn top_up(&mut self) -> Result<()> {
        while self.queue.len() < self.depth {
            let buffer = self.spare.pop().unwrap_or_else(|| vec![0; self.chunk]);
            let transfer = self.device.submit_bulk_in(self.endpoint, buffer)?;
            self.queue.push_back(transfer);
        }
        Ok(())
    }
}

impl Read for BulkReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            if let Some(outcome) = &self.current {
                let data = outcome.data();
                if self.position < data.len() {
                    let count = out.len().min(data.len() - self.position);
                    out[..count].copy_from_slice(&data[self.position..self.position + count]);
                    self.position += count;
                    return Ok(count);
                }
            }
            if let Some(outcome) = self.current.take() {
                self.spare.push(outcome.into_buffer());
            }
            self.position = 0;
            self.top_up().map_err(to_io)?;
            let transfer = self
                .queue
                .pop_front()
                .ok_or_else(|| io::Error::other("no transfer queued"))?;
            self.current = Some(transfer.wait().map_err(to_io)?);
        }
    }
}

impl Drop for BulkReader {
    fn drop(&mut self) {
        for transfer in &self.queue {
            transfer.cancel();
        }
    }
}

/// Writes to a bulk OUT endpoint: bytes are gathered into transfers of `chunk`
/// bytes, and up to `depth` of them are in flight.
pub struct BulkWriter {
    device: UsbDevice,
    endpoint: u8,
    chunk: usize,
    depth: usize,
    queue: VecDeque<Transfer>,
    spare: Vec<Vec<u8>>,
    current: Vec<u8>,
}

impl BulkWriter {
    pub(crate) fn new(device: UsbDevice, endpoint: u8, chunk: usize, depth: usize) -> Self {
        let chunk = chunk.max(1);
        Self {
            device,
            endpoint,
            chunk,
            depth: depth.max(1),
            queue: VecDeque::new(),
            spare: Vec::new(),
            current: Vec::with_capacity(chunk),
        }
    }

    /// Sends the gathered bytes as one transfer, waiting first while `depth`
    /// transfers are in flight.
    fn send_current(&mut self) -> Result<()> {
        if self.current.is_empty() {
            return Ok(());
        }
        while self.queue.len() >= self.depth {
            self.retire_oldest()?;
        }
        let mut next = self
            .spare
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(self.chunk));
        next.clear();
        let data = std::mem::replace(&mut self.current, next);
        let transfer = self.device.submit_bulk_out(self.endpoint, data)?;
        self.queue.push_back(transfer);
        Ok(())
    }

    /// Waits for the oldest transfer and keeps its buffer for reuse.
    fn retire_oldest(&mut self) -> Result<()> {
        if let Some(transfer) = self.queue.pop_front() {
            let outcome = transfer.wait()?;
            self.spare.push(outcome.into_buffer());
        }
        Ok(())
    }
}

impl Write for BulkWriter {
    fn write(&mut self, mut data: &[u8]) -> io::Result<usize> {
        let written = data.len();
        while !data.is_empty() {
            let room = self.chunk - self.current.len();
            let count = room.min(data.len());
            self.current.extend_from_slice(&data[..count]);
            data = &data[count..];
            if self.current.len() == self.chunk {
                self.send_current().map_err(to_io)?;
            }
        }
        Ok(written)
    }

    /// Sends the gathered tail and waits until every transfer has completed.
    fn flush(&mut self) -> io::Result<()> {
        self.send_current().map_err(to_io)?;
        while !self.queue.is_empty() {
            self.retire_oldest().map_err(to_io)?;
        }
        Ok(())
    }
}

impl Drop for BulkWriter {
    /// Flushes what was written. A failure here is lost: call `flush` to see it.
    fn drop(&mut self) {
        let _ = self.flush();
    }
}
