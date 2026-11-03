//! Buffered streams: several transfers in flight, so the device never waits
//! for the host between them. The bulk streams are blocking `std::io`
//! adapters; the isochronous reader hands out each completed transfer's data.
//! Buffers are allocated once per stream slot and recycled, so a steady stream
//! allocates no transfer memory.

use crate::device::UsbDevice;
#[cfg(target_os = "linux")]
use crate::device::iso_packet_lengths;
#[cfg(target_os = "linux")]
use crate::error::ErrorKind;
use crate::error::{Error, Result};
use crate::transfer::IsoPacket;
use crate::transfer::{Transfer, TransferBuffer, TransferOutcome};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

/// How long one wait for a completion lasts before the deadline is checked again.
#[cfg(target_os = "linux")]
const WAIT_SLICE: Duration = Duration::from_millis(100);

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
    spare: Vec<TransferBuffer>,
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
            let buffer = match self.spare.pop() {
                Some(buffer) => buffer,
                None => self.device.allocate_buffer(self.chunk),
            };
            let transfer = self.device.submit_bulk_in_buffer(self.endpoint, buffer)?;
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
            let outcome = transfer.wait().map_err(to_io)?;
            if let Some(error) = outcome.status.to_error("bulk read") {
                // The memory comes back to the stream, even on an error.
                self.spare.push(outcome.into_buffer());
                return Err(to_io(error));
            }
            self.current = Some(outcome);
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
    spare: Vec<TransferBuffer>,
    /// The buffer being filled, `chunk` bytes long, and how much of it is filled.
    current: Option<TransferBuffer>,
    filled: usize,
    /// Bytes written since the last [`BulkWriter::flush_end`].
    written: u64,
    /// The endpoint's `wMaxPacketSize`, from the descriptors.
    max_packet: Option<u16>,
}

/// Whether a transfer of `written` bytes ends on a packet boundary, so that
/// the device needs a zero-length packet to see its end. `max_packet` is the
/// endpoint's packet size; nothing written means nothing to terminate.
pub(crate) fn needs_zero_packet(written: u64, max_packet: u16) -> bool {
    max_packet > 0 && written > 0 && written.is_multiple_of(u64::from(max_packet))
}

impl BulkWriter {
    pub(crate) fn new(device: UsbDevice, endpoint: u8, chunk: usize, depth: usize) -> Self {
        let max_packet = device.endpoint_max_packet_size(endpoint);
        Self {
            device,
            endpoint,
            chunk: chunk.max(1),
            depth: depth.max(1),
            queue: VecDeque::new(),
            spare: Vec::new(),
            current: None,
            filled: 0,
            written: 0,
            max_packet,
        }
    }

    /// The buffer to fill next: a recycled one (restored to a full chunk), or a new one.
    fn next_buffer(&mut self) -> TransferBuffer {
        match self.spare.pop() {
            Some(mut buffer) => {
                buffer.set_len(self.chunk);
                buffer
            }
            None => self.device.allocate_buffer(self.chunk),
        }
    }

    /// Sends the gathered bytes as one transfer, waiting first while `depth`
    /// transfers are in flight.
    fn send_current(&mut self) -> Result<()> {
        if self.filled == 0 {
            return Ok(());
        }
        while self.queue.len() >= self.depth {
            self.retire_oldest()?;
        }
        let mut buffer = self.current.take().unwrap_or_else(|| self.next_buffer());
        buffer.set_len(self.filled);
        self.filled = 0;
        let transfer = self.device.submit_bulk_out_buffer(self.endpoint, buffer)?;
        self.queue.push_back(transfer);
        Ok(())
    }

    /// Waits for the oldest transfer and keeps its buffer for reuse.
    fn retire_oldest(&mut self) -> Result<()> {
        if let Some(transfer) = self.queue.pop_front() {
            let outcome = transfer.wait()?;
            let status = outcome.status;
            self.spare.push(outcome.into_buffer());
            if let Some(error) = status.to_error("bulk write") {
                return Err(error);
            }
        }
        Ok(())
    }

    /// Flushes, then ends the transfer for the device: when the bytes written
    /// since the last `flush_end` are a whole number of packets, a zero-length
    /// packet follows them, as nusb's `flush_end_async` does. A device that
    /// reads until a short packet needs this, or its read never returns. The
    /// count starts again afterwards. Writing nothing sends nothing.
    pub fn flush_end(&mut self) -> io::Result<()> {
        self.flush()?;
        if self.written == 0 {
            return Ok(());
        }
        let max_packet = self.max_packet.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the endpoint is not in the descriptors, so its packet size is unknown",
            )
        })?;
        if needs_zero_packet(self.written, max_packet) {
            let outcome = self
                .device
                .submit_bulk_out(self.endpoint, Vec::new())
                .and_then(Transfer::wait)
                .map_err(to_io)?;
            let status = outcome.status;
            self.spare.push(outcome.into_buffer());
            if let Some(error) = status.to_error("zero-length packet") {
                return Err(to_io(error));
            }
        }
        self.written = 0;
        Ok(())
    }
}

impl Write for BulkWriter {
    fn write(&mut self, mut data: &[u8]) -> io::Result<usize> {
        let written = data.len();
        self.written += written as u64;
        while !data.is_empty() {
            if self.current.is_none() {
                self.current = Some(self.next_buffer());
            }
            let room = self.chunk - self.filled;
            let count = room.min(data.len());
            if let Some(buffer) = self.current.as_mut() {
                buffer.as_mut_slice()[self.filled..self.filled + count]
                    .copy_from_slice(&data[..count]);
            }
            self.filled += count;
            data = &data[count..];
            if self.filled == self.chunk {
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

/// One completed isochronous transfer, borrowed from the reader: its data (the
/// bytes the packets moved, packet after packet) and the packets' results.
#[derive(Debug)]
pub struct IsoBlock<'a> {
    buffer: &'a [u8],
    packets: &'a [IsoPacket],
}

impl<'a> IsoBlock<'a> {
    /// A block over a completed transfer's buffer and its packets.
    #[cfg(target_os = "linux")]
    pub(crate) fn new(buffer: &'a [u8], packets: &'a [IsoPacket]) -> Self {
        Self { buffer, packets }
    }

    /// The number of packets in the transfer.
    pub fn packet_count(&self) -> usize {
        self.packets.len()
    }

    /// Each packet with the bytes it moved, in packet order. The usbdevfs layout
    /// places packet `i` at the sum of the scheduled lengths of the packets before
    /// it, not at the sum of the bytes they moved: a short packet leaves a gap
    /// after its bytes, and the packets after it stay where they were scheduled.
    pub fn packets(&self) -> IsoPackets<'a> {
        IsoPackets {
            buffer: self.buffer,
            packets: self.packets.iter(),
            offset: 0,
        }
    }
}

/// The packets of an [`IsoBlock`], each with its bytes (see [`IsoBlock::packets`]).
#[derive(Debug, Clone)]
pub struct IsoPackets<'a> {
    buffer: &'a [u8],
    packets: std::slice::Iter<'a, IsoPacket>,
    offset: usize,
}

impl<'a> Iterator for IsoPackets<'a> {
    type Item = (&'a IsoPacket, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let packet = self.packets.next()?;
        let start = self.offset.min(self.buffer.len());
        let end = start
            .saturating_add(packet.actual_length)
            .min(self.buffer.len());
        self.offset = self.offset.saturating_add(packet.length);
        Some((packet, &self.buffer[start..end]))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.packets.size_hint()
    }
}

impl ExactSizeIterator for IsoPackets<'_> {}

/// Reads an isochronous IN endpoint: `depth` transfers of `packets` packets of
/// `packet_len` bytes stay in flight, and each completed transfer is handed out
/// as an [`IsoBlock`] until the next call, which recycles its buffer and its
/// packet table. The packet lengths are computed once; buffers and packet tables
/// are reused, so a steady stream allocates nothing per transfer.
#[cfg(target_os = "linux")]
pub struct IsoReader {
    device: UsbDevice,
    endpoint: u8,
    packet_len: usize,
    lengths: Vec<u32>,
    depth: usize,
    queue: VecDeque<Transfer>,
    spare: Vec<TransferBuffer>,
    current: Option<TransferOutcome>,
}

#[cfg(target_os = "linux")]
impl IsoReader {
    pub(crate) fn new(
        device: UsbDevice,
        endpoint: u8,
        packet_len: usize,
        packets: usize,
        depth: usize,
    ) -> Result<Self> {
        if packet_len == 0 || packets == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "isochronous reads need a packet length and a packet count",
            ));
        }
        let total = packet_len * packets;
        let lengths = iso_packet_lengths(total, packet_len);
        Ok(Self {
            device,
            endpoint,
            packet_len,
            lengths,
            depth: depth.max(1),
            queue: VecDeque::new(),
            spare: Vec::new(),
            current: None,
        })
    }

    /// The bytes of one transfer: `packets` times the packet length.
    pub fn transfer_len(&self) -> usize {
        self.lengths.iter().map(|&len| len as usize).sum()
    }

    /// The packet length this reader was built for.
    pub fn packet_len(&self) -> usize {
        self.packet_len
    }

    /// Keeps `depth` transfers in flight.
    fn top_up(&mut self) -> Result<()> {
        while self.queue.len() < self.depth {
            let buffer = match self.spare.pop() {
                Some(buffer) => buffer,
                None => self.device.allocate_buffer(self.transfer_len()),
            };
            let transfer =
                self.device
                    .submit_isochronous_packets(self.endpoint, buffer, &self.lengths)?;
            self.queue.push_back(transfer);
        }
        Ok(())
    }

    /// Waits for the next completed transfer and returns it, without a deadline.
    /// The previous block's buffer is recycled first, and the next transfer is
    /// already in flight. In caller-driven reaping mode this reaps itself, so it
    /// completes; it does not wait for a thread that is not there.
    pub fn next_block(&mut self) -> Result<IsoBlock<'_>> {
        match self.next_block_until(None)? {
            Some(block) => Ok(block),
            None => Err(Error::new(
                ErrorKind::Other,
                "no isochronous transfer completed",
            )),
        }
    }

    /// As [`IsoReader::next_block`], but gives up at `deadline` (`None`: never):
    /// returns `Ok(None)` then, with the next transfer still in flight. Both
    /// reaping modes work: a caller-driven device is reaped here (see
    /// [`UsbDevice::reap_until`]), and a device with a reaper thread is waited on.
    pub fn next_block_until(&mut self, deadline: Option<Instant>) -> Result<Option<IsoBlock<'_>>> {
        if let Some(done) = self.current.take() {
            let (buffer, packets) = done.into_parts();
            self.spare.push(buffer);
            self.device.recycle_packets(packets);
        }
        self.top_up()?;
        loop {
            let ready = self
                .queue
                .front()
                .is_some_and(|transfer| transfer.wait_complete(Some(Duration::ZERO)));
            if ready {
                let transfer = self.queue.pop_front().ok_or_else(|| {
                    Error::new(ErrorKind::Other, "no isochronous transfer queued")
                })?;
                let outcome = transfer.wait()?;
                let outcome = self.current.insert(outcome);
                return Ok(Some(IsoBlock::new(
                    outcome.buffer.as_slice(),
                    &outcome.packets,
                )));
            }
            let slice = match deadline {
                None => WAIT_SLICE,
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(None);
                    }
                    (deadline - now).min(WAIT_SLICE)
                }
            };
            if self.device.caller_driven() {
                self.device.reap_until(Instant::now() + slice)?;
            } else if let Some(transfer) = self.queue.front() {
                transfer.wait_complete(Some(slice));
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for IsoReader {
    fn drop(&mut self) {
        for transfer in &self.queue {
            transfer.cancel();
        }
    }
}
