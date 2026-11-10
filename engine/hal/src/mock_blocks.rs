//! Mocks of the traits in [`crate::blocks`]. Clones share state, so a test
//! keeps one to feed and inspect what the code under test did with another.

use core::time::Duration;

use super::alloc::collections::{BTreeMap, VecDeque};
use super::alloc::vec;
use super::alloc::vec::Vec;
use super::{Shared, with};
use crate::ErrorKind;
use crate::blocks::{
    Clocks, ControlSetup, DmaBuffer, DmaBuffers, Interrupt, InterruptEvent, IsoPacket, Registers,
    Reset, StreamEndpoint, StreamFrame, Uart, UsbDevice, UsbDeviceId, UsbDeviceInfo, UsbFunction,
    UsbFunctionEvent, UsbHost,
};

fn copy(from: &[u8], to: &mut [u8]) -> usize {
    let n = from.len().min(to.len());
    to[..n].copy_from_slice(&from[..n]);
    n
}

#[derive(Debug, Default)]
struct RegistersState {
    values: BTreeMap<usize, u32>,
    writes: Vec<(usize, u32)>,
    size: usize,
}

/// A register window that remembers its values and every write.
#[derive(Debug, Clone, Default)]
pub struct MockRegisters {
    state: Shared<RegistersState>,
}

impl MockRegisters {
    pub fn new(size: usize) -> Self {
        let mock = Self::default();
        with(&mock.state, |s| s.size = size);
        mock
    }

    /// Sets a value as the hardware would, without recording a write.
    pub fn set(&self, offset: usize, value: u32) {
        with(&self.state, |s| {
            s.values.insert(offset, value);
        });
    }

    pub fn writes(&self) -> Vec<(usize, u32)> {
        with(&self.state, |s| s.writes.clone())
    }
}

impl Registers for MockRegisters {
    type Error = ErrorKind;

    fn size(&self) -> usize {
        with(&self.state, |s| s.size)
    }

    fn read32(&mut self, offset: usize) -> Result<u32, ErrorKind> {
        with(&self.state, |s| {
            if offset + 4 > s.size {
                return Err(ErrorKind::InvalidInput);
            }
            Ok(s.values.get(&offset).copied().unwrap_or(0))
        })
    }

    fn write32(&mut self, offset: usize, value: u32) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            if offset + 4 > s.size {
                return Err(ErrorKind::InvalidInput);
            }
            s.values.insert(offset, value);
            s.writes.push((offset, value));
            Ok(())
        })
    }
}

#[derive(Debug, Default)]
struct InterruptState {
    enabled: bool,
    pending: VecDeque<InterruptEvent>,
}

/// An interrupt line a test fires by hand. `wait` never blocks.
#[derive(Debug, Clone, Default)]
pub struct MockInterrupt {
    state: Shared<InterruptState>,
}

impl MockInterrupt {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fire(&self, timestamp_ns: u64) {
        with(&self.state, |s| {
            if s.enabled {
                s.pending.push_back(InterruptEvent {
                    count: 1,
                    timestamp_ns,
                });
            }
        });
    }

    pub fn enabled(&self) -> bool {
        with(&self.state, |s| s.enabled)
    }
}

impl Interrupt for MockInterrupt {
    type Error = ErrorKind;

    fn enable(&mut self) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.enabled = true);
        Ok(())
    }

    fn disable(&mut self) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.enabled = false);
        Ok(())
    }

    fn wait(&mut self, _timeout: Option<Duration>) -> Result<Option<InterruptEvent>, ErrorKind> {
        Ok(with(&self.state, |s| s.pending.pop_front()))
    }
}

#[derive(Debug, Default)]
struct DmaState {
    next: u32,
    buffers: BTreeMap<u32, Vec<u8>>,
    owned_by_device: BTreeMap<u32, bool>,
}

/// DMA memory backed by the heap. A CPU access to a buffer the device holds
/// fails, so a missing sync shows up in a test.
#[derive(Debug, Clone, Default)]
pub struct MockDma {
    state: Shared<DmaState>,
}

impl MockDma {
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes as the block would, whoever holds the buffer.
    pub fn device_write(&self, id: u32, offset: usize, data: &[u8]) {
        with(&self.state, |s| {
            if let Some(buffer) = s.buffers.get_mut(&id) {
                buffer[offset..offset + data.len()].copy_from_slice(data);
            }
        });
    }

    pub fn live_buffers(&self) -> usize {
        with(&self.state, |s| s.buffers.len())
    }
}

impl DmaState {
    fn range(&mut self, id: u32, offset: u64, len: usize) -> Result<&mut [u8], ErrorKind> {
        if self.owned_by_device.get(&id).copied().unwrap_or(false) {
            return Err(ErrorKind::Busy);
        }
        let buffer = self.buffers.get_mut(&id).ok_or(ErrorKind::NotFound)?;
        let start = usize::try_from(offset).map_err(|_| ErrorKind::InvalidInput)?;
        let end = start.checked_add(len).ok_or(ErrorKind::InvalidInput)?;
        buffer.get_mut(start..end).ok_or(ErrorKind::InvalidInput)
    }

    fn set_owner(&mut self, id: u32, device: bool) -> Result<(), ErrorKind> {
        if !self.buffers.contains_key(&id) {
            return Err(ErrorKind::NotFound);
        }
        self.owned_by_device.insert(id, device);
        Ok(())
    }
}

impl DmaBuffers for MockDma {
    type Error = ErrorKind;

    fn allocate(&mut self, len: u64) -> Result<DmaBuffer, ErrorKind> {
        with(&self.state, |s| {
            let size = usize::try_from(len).map_err(|_| ErrorKind::InvalidInput)?;
            s.next += 1;
            let id = s.next;
            s.buffers.insert(id, vec![0; size]);
            Ok(DmaBuffer {
                id,
                len,
                device_address: u64::from(id) << 32,
            })
        })
    }

    fn release(&mut self, id: u32) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            s.owned_by_device.remove(&id);
            s.buffers.remove(&id).map(|_| ()).ok_or(ErrorKind::NotFound)
        })
    }

    fn write(&mut self, id: u32, offset: u64, data: &[u8]) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            s.range(id, offset, data.len())?.copy_from_slice(data);
            Ok(())
        })
    }

    fn read(&mut self, id: u32, offset: u64, out: &mut [u8]) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            out.copy_from_slice(s.range(id, offset, out.len())?);
            Ok(())
        })
    }

    fn sync_for_device(&mut self, id: u32) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.set_owner(id, true))
    }

    fn sync_for_cpu(&mut self, id: u32) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.set_owner(id, false))
    }
}

#[derive(Debug, Default)]
struct ClocksState {
    enabled: BTreeMap<u32, bool>,
    rates: BTreeMap<u32, u64>,
}

/// Clocks that take any rate they are given.
#[derive(Debug, Clone, Default)]
pub struct MockClocks {
    state: Shared<ClocksState>,
}

impl MockClocks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enabled(&self, clock: u32) -> bool {
        with(&self.state, |s| {
            s.enabled.get(&clock).copied().unwrap_or(false)
        })
    }
}

impl Clocks for MockClocks {
    type Error = ErrorKind;

    fn enable(&mut self, clock: u32) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.enabled.insert(clock, true));
        Ok(())
    }

    fn disable(&mut self, clock: u32) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.enabled.insert(clock, false));
        Ok(())
    }

    fn rate_hz(&mut self, clock: u32) -> Result<u64, ErrorKind> {
        Ok(with(&self.state, |s| {
            s.rates.get(&clock).copied().unwrap_or(0)
        }))
    }

    fn set_rate_hz(&mut self, clock: u32, hz: u64) -> Result<u64, ErrorKind> {
        with(&self.state, |s| s.rates.insert(clock, hz));
        Ok(hz)
    }
}

/// Reset lines that record each change as `(line, asserted)`.
#[derive(Debug, Clone, Default)]
pub struct MockReset {
    state: Shared<Vec<(u32, bool)>>,
}

impl MockReset {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn history(&self) -> Vec<(u32, bool)> {
        with(&self.state, |s| s.clone())
    }
}

impl Reset for MockReset {
    type Error = ErrorKind;

    fn assert(&mut self, line: u32) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.push((line, true)));
        Ok(())
    }

    fn deassert(&mut self, line: u32) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.push((line, false)));
        Ok(())
    }
}

#[derive(Debug, Default)]
struct UartState {
    baud: u32,
    incoming: VecDeque<u8>,
    sent: Vec<u8>,
}

/// A serial port with a test at the far end.
#[derive(Debug, Clone, Default)]
pub struct MockUart {
    state: Shared<UartState>,
}

impl MockUart {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn baud(&self) -> u32 {
        with(&self.state, |s| s.baud)
    }

    pub fn receive(&self, data: &[u8]) {
        with(&self.state, |s| s.incoming.extend(data.iter().copied()));
    }

    pub fn sent(&self) -> Vec<u8> {
        with(&self.state, |s| s.sent.clone())
    }
}

impl Uart for MockUart {
    type Error = ErrorKind;

    fn set_baud(&mut self, baud: u32) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.baud = baud);
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, ErrorKind> {
        with(&self.state, |s| s.sent.extend_from_slice(data));
        Ok(data.len())
    }

    fn read(&mut self, out: &mut [u8], _timeout: Duration) -> Result<usize, ErrorKind> {
        Ok(with(&self.state, |s| {
            let n = out.len().min(s.incoming.len());
            for (slot, byte) in out.iter_mut().zip(s.incoming.drain(..n)) {
                *slot = byte;
            }
            n
        }))
    }
}

#[derive(Debug, Default)]
struct StreamState {
    buffers: u32,
    published: VecDeque<StreamFrame>,
    returned: VecDeque<u32>,
    jobs: VecDeque<Vec<u8>>,
    /// What each buffer holds, and whether the producer has it.
    contents: Vec<Vec<u8>>,
    held: Vec<bool>,
}

/// A stream with a test playing the consumer.
#[derive(Debug, Clone, Default)]
pub struct MockStream {
    state: Shared<StreamState>,
}

impl MockStream {
    /// Starts with every buffer free. The buffers hold no bytes, so nothing can be written.
    pub fn new(buffers: u32) -> Self {
        Self::with_bytes(buffers, 0)
    }

    /// Starts with every buffer free and `buffer_bytes` long, for a producer that writes them.
    pub fn with_bytes(buffers: u32, buffer_bytes: usize) -> Self {
        let mock = Self::default();
        with(&mock.state, |s| {
            s.buffers = buffers;
            s.returned.extend(0..buffers);
            s.contents = vec![vec![0; buffer_bytes]; buffers as usize];
            s.held = vec![false; buffers as usize];
        });
        mock
    }

    /// Puts `data` at the start of `buffer`, as a consumer handing the producer input would.
    /// Nothing happens when it doesn't fit.
    pub fn fill(&self, buffer: u32, data: &[u8]) {
        with(&self.state, |s| {
            let to = s.contents.get_mut(buffer as usize);
            if let Some(to) = to.and_then(|to| to.get_mut(..data.len())) {
                to.copy_from_slice(data);
            }
        });
    }

    /// What `buffer` holds now.
    pub fn contents(&self, buffer: u32) -> Vec<u8> {
        with(&self.state, |s| {
            s.contents.get(buffer as usize).cloned().unwrap_or_default()
        })
    }

    pub fn take_frame(&self) -> Option<StreamFrame> {
        with(&self.state, |s| s.published.pop_front())
    }

    pub fn return_buffer(&self, buffer: u32) {
        with(&self.state, |s| s.returned.push_back(buffer));
    }

    pub fn post_job(&self, job: &[u8]) {
        with(&self.state, |s| s.jobs.push_back(job.to_vec()));
    }
}

impl StreamEndpoint for MockStream {
    type Error = ErrorKind;

    fn buffer_count(&self) -> u32 {
        with(&self.state, |s| s.buffers)
    }

    fn publish(&mut self, frame: StreamFrame) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            if frame.buffer >= s.buffers {
                return Err(ErrorKind::InvalidInput);
            }
            s.held[frame.buffer as usize] = false;
            s.published.push_back(frame);
            Ok(())
        })
    }

    fn next_buffer(&mut self) -> Result<Option<u32>, ErrorKind> {
        Ok(with(&self.state, |s| {
            let buffer = s.returned.pop_front()?;
            if let Some(held) = s.held.get_mut(buffer as usize) {
                *held = true;
            }
            Some(buffer)
        }))
    }

    fn next_job(&mut self, out: &mut [u8]) -> Result<Option<usize>, ErrorKind> {
        with(&self.state, |s| match s.jobs.front() {
            None => Ok(None),
            Some(job) if job.len() > out.len() => Err(ErrorKind::InvalidInput),
            Some(job) => {
                let n = copy(job, out);
                s.jobs.pop_front();
                Ok(Some(n))
            }
        })
    }

    fn write(&mut self, buffer: u32, offset: usize, data: &[u8]) -> Result<bool, ErrorKind> {
        with(&self.state, |s| {
            if s.held.get(buffer as usize) != Some(&true) {
                return Err(ErrorKind::InvalidInput);
            }
            let end = offset.checked_add(data.len());
            let range = end.and_then(|end| s.contents[buffer as usize].get_mut(offset..end));
            range.ok_or(ErrorKind::InvalidInput)?.copy_from_slice(data);
            Ok(true)
        })
    }

    fn read(&mut self, buffer: u32, offset: usize, out: &mut [u8]) -> Result<bool, ErrorKind> {
        with(&self.state, |s| {
            if s.held.get(buffer as usize) != Some(&true) {
                return Err(ErrorKind::InvalidInput);
            }
            let end = offset.checked_add(out.len());
            let range = end.and_then(|end| s.contents[buffer as usize].get(offset..end));
            out.copy_from_slice(range.ok_or(ErrorKind::InvalidInput)?);
            Ok(true)
        })
    }
}

#[derive(Debug, Default)]
struct UsbDeviceState {
    descriptor: Vec<u8>,
    claimed: Vec<u8>,
    alternates: BTreeMap<u8, u8>,
    in_data: VecDeque<Vec<u8>>,
    iso: VecDeque<Vec<u8>>,
    written: Vec<(u8, Vec<u8>)>,
    controls: Vec<ControlSetup>,
}

/// A USB device a test feeds. A read with nothing queued times out.
#[derive(Debug, Clone, Default)]
pub struct MockUsbDevice {
    state: Shared<UsbDeviceState>,
}

impl MockUsbDevice {
    pub fn new(descriptor: &[u8]) -> Self {
        let mock = Self::default();
        with(&mock.state, |s| s.descriptor = descriptor.to_vec());
        mock
    }

    pub fn queue_in(&self, data: &[u8]) {
        with(&self.state, |s| s.in_data.push_back(data.to_vec()));
    }

    /// Queues one packet for the next isochronous transfers.
    pub fn queue_iso(&self, packet: &[u8]) {
        with(&self.state, |s| s.iso.push_back(packet.to_vec()));
    }

    pub fn written(&self) -> Vec<(u8, Vec<u8>)> {
        with(&self.state, |s| s.written.clone())
    }

    pub fn controls(&self) -> Vec<ControlSetup> {
        with(&self.state, |s| s.controls.clone())
    }

    pub fn claimed(&self) -> Vec<u8> {
        with(&self.state, |s| s.claimed.clone())
    }

    pub fn alternate(&self, interface: u8) -> Option<u8> {
        with(&self.state, |s| s.alternates.get(&interface).copied())
    }

    fn pop_in(&self, data: &mut [u8]) -> Result<usize, ErrorKind> {
        with(&self.state, |s| {
            let next = s.in_data.pop_front().ok_or(ErrorKind::Timeout)?;
            Ok(copy(&next, data))
        })
    }

    fn record_out(&self, endpoint: u8, data: &[u8]) -> usize {
        with(&self.state, |s| s.written.push((endpoint, data.to_vec())));
        data.len()
    }
}

impl UsbDevice for MockUsbDevice {
    type Error = ErrorKind;

    fn config_descriptor(&mut self, _index: u8, out: &mut [u8]) -> Result<usize, ErrorKind> {
        Ok(with(&self.state, |s| copy(&s.descriptor, out)))
    }

    fn claim_interface(&mut self, interface: u8) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            if s.claimed.contains(&interface) {
                return Err(ErrorKind::Busy);
            }
            s.claimed.push(interface);
            Ok(())
        })
    }

    fn release_interface(&mut self, interface: u8) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.claimed.retain(|i| *i != interface));
        Ok(())
    }

    fn set_alternate(&mut self, interface: u8, alternate: u8) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.alternates.insert(interface, alternate));
        Ok(())
    }

    fn control_in(
        &mut self,
        setup: ControlSetup,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        with(&self.state, |s| s.controls.push(setup));
        self.pop_in(data)
    }

    fn control_out(
        &mut self,
        setup: ControlSetup,
        data: &[u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        with(&self.state, |s| s.controls.push(setup));
        Ok(self.record_out(0, data))
    }

    fn bulk_in(
        &mut self,
        _endpoint: u8,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.pop_in(data)
    }

    fn bulk_out(
        &mut self,
        endpoint: u8,
        data: &[u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        Ok(self.record_out(endpoint, data))
    }

    fn interrupt_in(
        &mut self,
        _endpoint: u8,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.pop_in(data)
    }

    fn iso_in(
        &mut self,
        _endpoint: u8,
        data: &mut [u8],
        packets: &mut [IsoPacket],
        _timeout: Duration,
    ) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            let mut offset = 0;
            for packet in packets {
                let capacity = packet.capacity as usize;
                let end = offset + capacity;
                let slot = data.get_mut(offset..end).ok_or(ErrorKind::InvalidInput)?;
                match s.iso.pop_front() {
                    Some(bytes) => {
                        packet.received = copy(&bytes, slot) as u32;
                        packet.ok = true;
                    }
                    None => {
                        packet.received = 0;
                        packet.ok = false;
                    }
                }
                offset = end;
            }
            Ok(())
        })
    }
}

/// A USB host with devices a test plugs in.
#[derive(Debug, Clone, Default)]
pub struct MockUsbHost {
    state: Shared<Vec<(UsbDeviceInfo, MockUsbDevice)>>,
}

impl MockUsbHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn plug(&self, info: UsbDeviceInfo, device: MockUsbDevice) {
        with(&self.state, |s| s.push((info, device)));
    }

    pub fn unplug(&self, id: UsbDeviceId) {
        with(&self.state, |s| s.retain(|(info, _)| info.id != id));
    }
}

impl UsbHost for MockUsbHost {
    type Error = ErrorKind;
    type Device = MockUsbDevice;

    fn enumerate(&mut self, out: &mut [UsbDeviceInfo]) -> Result<usize, ErrorKind> {
        Ok(with(&self.state, |s| {
            for (slot, (info, _)) in out.iter_mut().zip(s.iter()) {
                *slot = *info;
            }
            s.len()
        }))
    }

    fn open(&mut self, id: UsbDeviceId) -> Result<MockUsbDevice, ErrorKind> {
        with(&self.state, |s| {
            s.iter()
                .find(|(info, _)| info.id == id)
                .map(|(_, device)| device.clone())
                .ok_or(ErrorKind::NotFound)
        })
    }
}

#[derive(Debug, Default)]
struct UsbFunctionState {
    descriptors: Option<Vec<u8>>,
    events: VecDeque<UsbFunctionEvent>,
    from_host: VecDeque<Vec<u8>>,
    to_host: Vec<(u8, Vec<u8>)>,
}

/// This board as a USB device, with a test playing the host.
#[derive(Debug, Clone, Default)]
pub struct MockUsbFunction {
    state: Shared<UsbFunctionState>,
}

impl MockUsbFunction {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn descriptors(&self) -> Option<Vec<u8>> {
        with(&self.state, |s| s.descriptors.clone())
    }

    pub fn event(&self, event: UsbFunctionEvent) {
        with(&self.state, |s| s.events.push_back(event));
    }

    pub fn host_sends(&self, data: &[u8]) {
        with(&self.state, |s| s.from_host.push_back(data.to_vec()));
    }

    pub fn host_received(&self) -> Vec<(u8, Vec<u8>)> {
        with(&self.state, |s| s.to_host.clone())
    }
}

impl UsbFunction for MockUsbFunction {
    type Error = ErrorKind;

    fn enable(&mut self, descriptors: &[u8]) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.descriptors = Some(descriptors.to_vec()));
        Ok(())
    }

    fn disable(&mut self) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.descriptors = None);
        Ok(())
    }

    fn next_event(&mut self) -> Result<Option<UsbFunctionEvent>, ErrorKind> {
        Ok(with(&self.state, |s| s.events.pop_front()))
    }

    fn read(
        &mut self,
        _endpoint: u8,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        with(&self.state, |s| {
            let next = s.from_host.pop_front().ok_or(ErrorKind::Timeout)?;
            Ok(copy(&next, data))
        })
    }

    fn write(&mut self, endpoint: u8, data: &[u8], _timeout: Duration) -> Result<usize, ErrorKind> {
        with(&self.state, |s| s.to_host.push((endpoint, data.to_vec())));
        Ok(data.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WAIT: Duration = Duration::from_millis(1);

    #[test]
    fn registers_read_back_refuse_out_of_range_and_modify() {
        let mut regs = MockRegisters::new(0x10);
        let probe = regs.clone();
        probe.set(4, 0b1100);
        regs.modify32(4, 0b0100, 0b0001).unwrap();
        assert_eq!(regs.read32(4), Ok(0b1001));
        assert_eq!(regs.read32(0x10), Err(ErrorKind::InvalidInput));
        regs.write_block(8, &[1, 2]).unwrap();
        assert_eq!(probe.writes(), [(4, 0b1001), (8, 1), (12, 2)]);
    }

    #[test]
    fn interrupts_arrive_only_while_enabled() {
        let mut irq = MockInterrupt::new();
        let line = irq.clone();
        line.fire(1);
        assert_eq!(irq.wait(Some(WAIT)), Ok(None));
        irq.enable().unwrap();
        assert!(line.enabled());
        line.fire(2);
        let event = irq.wait(None).unwrap().unwrap();
        assert_eq!((event.count, event.timestamp_ns), (1, 2));
        irq.disable().unwrap();
        line.fire(3);
        assert_eq!(irq.wait(Some(WAIT)), Ok(None));
    }

    #[test]
    fn dma_needs_a_sync_before_the_cpu_can_touch_it() {
        let mut dma = MockDma::new();
        let block = dma.clone();
        let buffer = dma.allocate(4).unwrap();
        dma.write(buffer.id, 0, &[1, 2]).unwrap();
        dma.sync_for_device(buffer.id).unwrap();
        block.device_write(buffer.id, 2, &[3, 4]);
        let mut out = [0; 4];
        assert_eq!(dma.read(buffer.id, 0, &mut out), Err(ErrorKind::Busy));
        dma.sync_for_cpu(buffer.id).unwrap();
        dma.read(buffer.id, 0, &mut out).unwrap();
        assert_eq!(out, [1, 2, 3, 4]);
        assert_eq!(
            dma.write(buffer.id, 3, &[0, 0]),
            Err(ErrorKind::InvalidInput)
        );
        dma.release(buffer.id).unwrap();
        assert_eq!(block.live_buffers(), 0);
        assert_eq!(dma.release(buffer.id), Err(ErrorKind::NotFound));
    }

    #[test]
    fn clocks_hold_state_and_rate() {
        let mut clocks = MockClocks::new();
        clocks.enable(2).unwrap();
        assert!(clocks.enabled(2));
        assert_eq!(clocks.set_rate_hz(2, 24_000_000), Ok(24_000_000));
        assert_eq!(clocks.rate_hz(2), Ok(24_000_000));
        clocks.disable(2).unwrap();
        assert!(!clocks.enabled(2));
    }

    #[test]
    fn a_uart_moves_bytes_both_ways() {
        let mut uart = MockUart::new();
        let far = uart.clone();
        uart.set_baud(115_200).unwrap();
        assert_eq!(far.baud(), 115_200);
        assert_eq!(uart.write(&[1, 2]), Ok(2));
        assert_eq!(far.sent(), [1, 2]);
        let mut out = [0; 4];
        assert_eq!(uart.read(&mut out, WAIT), Ok(0));
        far.receive(&[3, 4]);
        assert_eq!(uart.read(&mut out[..1], WAIT), Ok(1));
        assert_eq!(uart.read(&mut out, WAIT), Ok(1));
        assert_eq!(out[0], 4);
    }

    #[test]
    fn reset_records_order() {
        let mut reset = MockReset::new();
        reset.assert(1).unwrap();
        reset.deassert(1).unwrap();
        assert_eq!(reset.history(), [(1, true), (1, false)]);
    }

    #[test]
    fn a_stream_cycles_buffers_and_jobs() {
        let mut stream = MockStream::new(2);
        let consumer = stream.clone();
        assert_eq!(stream.buffer_count(), 2);
        let buffer = stream.next_buffer().unwrap().unwrap();
        let frame = StreamFrame {
            buffer,
            bytes: 16,
            sequence: 1,
            identity: 7,
            timestamp_ns: 9,
        };
        stream.publish(frame).unwrap();
        assert_eq!(consumer.take_frame(), Some(frame));
        consumer.post_job(&[5, 6]);
        let mut small = [0; 1];
        assert_eq!(stream.next_job(&mut small), Err(ErrorKind::InvalidInput));
        let mut job = [0; 4];
        assert_eq!(stream.next_job(&mut job), Ok(Some(2)));
        assert_eq!(stream.next_job(&mut job), Ok(None));
        let bad = StreamFrame { buffer: 2, ..frame };
        assert_eq!(stream.publish(bad), Err(ErrorKind::InvalidInput));
    }

    #[test]
    fn a_stream_keeps_what_the_producer_writes_into_a_held_buffer() {
        let mut stream = MockStream::with_bytes(2, 8);
        let consumer = stream.clone();
        assert_eq!(stream.write(0, 0, &[1]), Err(ErrorKind::InvalidInput));
        let buffer = stream.next_buffer().unwrap().unwrap();
        assert_eq!(stream.write(buffer, 6, &[7, 8]), Ok(true));
        assert_eq!(
            stream.write(buffer, 7, &[7, 8]),
            Err(ErrorKind::InvalidInput)
        );
        assert_eq!(
            stream.write(buffer, usize::MAX, &[1]),
            Err(ErrorKind::InvalidInput)
        );
        assert_eq!(consumer.contents(buffer), [0, 0, 0, 0, 0, 0, 7, 8]);
        let frame = StreamFrame {
            buffer,
            bytes: 8,
            sequence: 1,
            identity: 0,
            timestamp_ns: 0,
        };
        stream.publish(frame).unwrap();
        assert_eq!(stream.write(buffer, 0, &[1]), Err(ErrorKind::InvalidInput));
        assert_eq!(MockStream::new(1).contents(0), []);
    }

    #[test]
    fn a_producer_reads_what_the_consumer_put_in_a_buffer_it_holds() {
        let mut stream = MockStream::with_bytes(2, 4);
        let consumer = stream.clone();
        consumer.fill(0, &[1, 2, 3, 4]);
        consumer.fill(1, &[0; 5]);
        let mut out = [0; 2];
        assert_eq!(stream.read(0, 0, &mut out), Err(ErrorKind::InvalidInput));
        let buffer = stream.next_buffer().unwrap().unwrap();
        assert_eq!(stream.read(buffer, 2, &mut out), Ok(true));
        assert_eq!(out, [3, 4]);
        assert_eq!(
            stream.read(buffer, 3, &mut out),
            Err(ErrorKind::InvalidInput)
        );
        assert_eq!(
            stream.read(buffer, usize::MAX, &mut out),
            Err(ErrorKind::InvalidInput)
        );
        assert_eq!(consumer.contents(1), [0; 4]);
    }

    fn info(address: u8) -> UsbDeviceInfo {
        UsbDeviceInfo {
            id: UsbDeviceId { bus: 1, address },
            vendor: 0x1234,
            product: 0x5678,
            class: 0,
            speed: crate::blocks::UsbSpeed::High,
        }
    }

    #[test]
    fn a_usb_host_enumerates_and_opens() {
        let mut host = MockUsbHost::new();
        let plugged = MockUsbDevice::new(&[9, 2]);
        host.plug(info(3), plugged.clone());
        host.plug(info(4), MockUsbDevice::new(&[]));
        let mut one = [info(0); 1];
        assert_eq!(host.enumerate(&mut one), Ok(2));
        assert_eq!(one[0].id.address, 3);

        let mut device = host.open(one[0].id).unwrap();
        let mut descriptor = [0; 8];
        assert_eq!(device.config_descriptor(0, &mut descriptor), Ok(2));
        device.claim_interface(1).unwrap();
        assert_eq!(device.claim_interface(1), Err(ErrorKind::Busy));
        device.set_alternate(1, 2).unwrap();
        assert_eq!(
            (plugged.claimed(), plugged.alternate(1)),
            ([1].to_vec(), Some(2))
        );
        device.release_interface(1).unwrap();

        host.unplug(one[0].id);
        assert_eq!(host.open(one[0].id).err(), Some(ErrorKind::NotFound));
    }

    #[test]
    fn a_usb_device_transfers_in_every_form() {
        let mut device = MockUsbDevice::new(&[]);
        let probe = device.clone();
        let setup = ControlSetup {
            request_type: 0x80,
            request: 6,
            value: 0,
            index: 0,
        };
        let mut data = [0; 4];
        assert_eq!(device.bulk_in(1, &mut data, WAIT), Err(ErrorKind::Timeout));
        probe.queue_in(&[1, 2]);
        assert_eq!(device.control_in(setup, &mut data, WAIT), Ok(2));
        probe.queue_in(&[3]);
        assert_eq!(device.bulk_in(1, &mut data, WAIT), Ok(1));
        probe.queue_in(&[4]);
        assert_eq!(device.interrupt_in(2, &mut data, WAIT), Ok(1));
        assert_eq!(device.bulk_out(3, &[8], WAIT), Ok(1));
        assert_eq!(device.control_out(setup, &[9], WAIT), Ok(1));
        assert_eq!(probe.written(), [(3, [8].to_vec()), (0, [9].to_vec())]);
        assert_eq!(probe.controls(), [setup, setup]);

        probe.queue_iso(&[7, 7]);
        let mut iso = [0; 4];
        let mut packets = [IsoPacket {
            capacity: 2,
            ..IsoPacket::default()
        }; 2];
        device.iso_in(4, &mut iso, &mut packets, WAIT).unwrap();
        assert_eq!((packets[0].received, packets[0].ok), (2, true));
        assert_eq!((packets[1].received, packets[1].ok), (0, false));
        assert_eq!(iso, [7, 7, 0, 0]);
    }

    #[test]
    fn a_usb_function_connects_and_moves_data() {
        let mut function = MockUsbFunction::new();
        let host = function.clone();
        function.enable(&[1, 2]).unwrap();
        assert_eq!(host.descriptors(), Some([1, 2].to_vec()));
        host.event(UsbFunctionEvent::Configured);
        assert_eq!(
            function.next_event(),
            Ok(Some(UsbFunctionEvent::Configured))
        );
        assert_eq!(function.next_event(), Ok(None));
        host.host_sends(&[5]);
        let mut data = [0; 2];
        assert_eq!(function.read(1, &mut data, WAIT), Ok(1));
        assert_eq!(function.read(1, &mut data, WAIT), Err(ErrorKind::Timeout));
        assert_eq!(function.write(2, &[6], WAIT), Ok(1));
        assert_eq!(host.host_received(), [(2, [6].to_vec())]);
        function.disable().unwrap();
        assert_eq!(host.descriptors(), None);
    }
}
