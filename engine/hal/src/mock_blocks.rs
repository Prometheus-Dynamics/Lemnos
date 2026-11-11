//! Mocks of the traits in [`crate::blocks`]. Clones share state, so a test
//! keeps one to feed and inspect what the code under test did with another.

use core::time::Duration;

use super::alloc::collections::{BTreeMap, VecDeque};
use super::alloc::vec;
use super::alloc::vec::Vec;
use super::{Shared, with};
use crate::ErrorKind;
use crate::blocks::{
    BufferHandle, BufferKind, Clocks, ControlSetup, DmaBuffer, DmaBuffers, Interrupt,
    InterruptEvent, IsoPacket, Registers, Reset, StreamEndpoint, StreamFrame, Uart, UsbDevice,
    UsbDeviceId, UsbDeviceInfo, UsbFunction, UsbFunctionEvent, UsbHost,
};
use crate::usb_descriptors::{self, UsbDescriptors, UsbEndpoint};

/// The descriptors behind a stream whose buffers have handles, one file per buffer.
#[cfg(all(feature = "std", unix))]
type Files = super::alloc::sync::Arc<[super::std::fs::File]>;

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
    /// What each buffer holds, and whether the producer has it. A buffer with a handle keeps
    /// its bytes behind the handle, and only its length here.
    contents: Vec<Vec<u8>>,
    held: Vec<bool>,
    /// Held since before the consumer changed: nothing is behind it until it is taken again.
    stale: Vec<bool>,
    /// Times the producer faulted its consumer.
    faults: u32,
}

/// A stream with a test playing the consumer.
#[derive(Debug, Clone, Default)]
pub struct MockStream {
    state: Shared<StreamState>,
    /// Set when the buffers have handles. Fixed when the stream is made, so a handle can be
    /// lent for as long as the stream is borrowed.
    #[cfg(all(feature = "std", unix))]
    files: Option<Files>,
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
            s.stale = vec![false; buffers as usize];
        });
        mock
    }

    /// Starts with every buffer free and behind a handle, as a stream of dma-bufs is: one
    /// descriptor per buffer, each as long as its file. `write`, `read`, [`fill`](Self::fill)
    /// and [`contents`](Self::contents) go to the same memory the handle names, by `pread` and
    /// `pwrite`. A sealed memfd stands in for a dma-buf, which takes neither call.
    #[cfg(all(feature = "std", unix))]
    pub fn with_handles(buffers: Vec<super::std::os::fd::OwnedFd>) -> Self {
        let files: Files = buffers
            .into_iter()
            .map(super::std::fs::File::from)
            .collect();
        let lengths = files.iter().map(|file| {
            let bytes = file.metadata().map_or(0, |m| m.len());
            usize::try_from(bytes).unwrap_or(usize::MAX)
        });
        let mut mock = Self::default();
        with(&mock.state, |s| {
            s.buffers = files.len() as u32;
            s.returned.extend(0..s.buffers);
            s.contents = lengths.map(|bytes| vec![0; bytes]).collect();
            s.held = vec![false; files.len()];
            s.stale = vec![false; files.len()];
        });
        mock.files = Some(files);
        mock
    }

    /// The file behind `buffer`, when the buffers have handles.
    #[cfg(all(feature = "std", unix))]
    fn file(&self, buffer: u32) -> Option<&super::std::fs::File> {
        self.files.as_ref()?.get(buffer as usize)
    }

    /// Copies `data` to `offset` of the memory behind `buffer`'s handle. `None` when the buffer
    /// has no handle, so its bytes are the ones kept here.
    #[cfg(all(feature = "std", unix))]
    fn store(&self, buffer: u32, offset: usize, data: &[u8]) -> Option<Result<(), ErrorKind>> {
        use super::std::os::unix::fs::FileExt;
        let file = self.file(buffer)?;
        Some(
            file.write_all_at(data, offset as u64)
                .map_err(|_| ErrorKind::Failed),
        )
    }

    #[cfg(not(all(feature = "std", unix)))]
    fn store(&self, _: u32, _: usize, _: &[u8]) -> Option<Result<(), ErrorKind>> {
        None
    }

    /// Copies from `offset` of the memory behind `buffer`'s handle into `out`. `None` when the
    /// buffer has no handle.
    #[cfg(all(feature = "std", unix))]
    fn load(&self, buffer: u32, offset: usize, out: &mut [u8]) -> Option<Result<(), ErrorKind>> {
        use super::std::os::unix::fs::FileExt;
        let file = self.file(buffer)?;
        Some(
            file.read_exact_at(out, offset as u64)
                .map_err(|_| ErrorKind::Failed),
        )
    }

    #[cfg(not(all(feature = "std", unix)))]
    fn load(&self, _: u32, _: usize, _: &mut [u8]) -> Option<Result<(), ErrorKind>> {
        None
    }

    /// The consumer goes and another takes its place, on memory of its own. Whatever the
    /// consumer had comes back free and its jobs are forgotten. A buffer the producer holds
    /// stays held with nothing behind it: it has no handle, and publishing it reaches no one
    /// and frees it.
    pub fn change_consumer(&self) {
        with(&self.state, |s| {
            s.published.clear();
            s.jobs.clear();
            for buffer in 0..s.buffers {
                let held = s.held.get(buffer as usize) == Some(&true);
                if let Some(stale) = s.stale.get_mut(buffer as usize) {
                    *stale = held;
                }
                if !held && !s.returned.contains(&buffer) {
                    s.returned.push_back(buffer);
                }
            }
        });
    }

    /// How many times the producer has faulted its consumer.
    pub fn faults(&self) -> u32 {
        with(&self.state, |s| s.faults)
    }

    /// Puts `data` at the start of `buffer`, as a consumer handing the producer input would.
    /// Nothing happens when it doesn't fit.
    pub fn fill(&self, buffer: u32, data: &[u8]) {
        let fits = with(&self.state, |s| {
            let to = s.contents.get_mut(buffer as usize);
            to.and_then(|to| to.get_mut(..data.len()))
                .map(|to| to.copy_from_slice(data))
                .is_some()
        });
        if fits {
            let _ = self.store(buffer, 0, data);
        }
    }

    /// What `buffer` holds now.
    pub fn contents(&self, buffer: u32) -> Vec<u8> {
        let mut contents = with(&self.state, |s| {
            s.contents.get(buffer as usize).cloned().unwrap_or_default()
        });
        let _ = self.load(buffer, 0, &mut contents);
        contents
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

    fn buffer_kind(&self) -> BufferKind {
        #[cfg(all(feature = "std", unix))]
        if self.files.is_some() {
            return BufferKind::Handles;
        }
        BufferKind::Plain
    }

    fn publish(&mut self, frame: StreamFrame) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            if frame.buffer >= s.buffers {
                return Err(ErrorKind::InvalidInput);
            }
            s.held[frame.buffer as usize] = false;
            if core::mem::take(&mut s.stale[frame.buffer as usize]) {
                // Taken under a consumer that has gone: it reaches no one and is free again.
                s.returned.push_back(frame.buffer);
            } else {
                s.published.push_back(frame);
            }
            Ok(())
        })
    }

    fn next_buffer(&mut self) -> Result<Option<u32>, ErrorKind> {
        Ok(with(&self.state, |s| {
            let buffer = s.returned.pop_front()?;
            if let Some(held) = s.held.get_mut(buffer as usize) {
                *held = true;
            }
            if let Some(stale) = s.stale.get_mut(buffer as usize) {
                *stale = false;
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
        })?;
        self.store(buffer, offset, data).transpose()?;
        Ok(true)
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
        })?;
        self.load(buffer, offset, out).transpose()?;
        Ok(true)
    }

    fn buffer_handle(&self, buffer: u32) -> Result<Option<BufferHandle<'_>>, ErrorKind> {
        let stale = with(&self.state, |s| {
            if s.held.get(buffer as usize) != Some(&true) {
                return Err(ErrorKind::InvalidInput);
            }
            Ok(s.stale.get(buffer as usize) == Some(&true))
        })?;
        #[cfg(all(feature = "std", unix))]
        if let Some(file) = self.file(buffer) {
            use super::std::os::fd::AsFd;
            return Ok((!stale).then(|| BufferHandle::from(file.as_fd())));
        }
        let _ = stale;
        Err(ErrorKind::Unsupported)
    }

    /// The faulted consumer is gone and the test plays the next one, as
    /// [`change_consumer`](Self::change_consumer) does.
    fn buffer_current(&self, buffer: u32) -> Result<bool, ErrorKind> {
        with(&self.state, |s| {
            if s.held.get(buffer as usize) != Some(&true) {
                return Err(ErrorKind::InvalidInput);
            }
            Ok(s.stale.get(buffer as usize) != Some(&true))
        })
    }

    fn fault_consumer(&mut self) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.faults += 1);
        self.change_consumer();
        Ok(())
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
    /// Interfaces a kernel driver holds.
    kernel_drivers: Vec<u8>,
    /// Every `detach_kernel_driver`, by interface.
    detaches: Vec<u8>,
    /// Resets asked for, whether or not they were carried out.
    resets: u32,
    reset_reenumerates: bool,
    reset_errors: VecDeque<ErrorKind>,
    /// The device this handle opened has left.
    gone: bool,
}

/// A USB device a test feeds. A read with nothing queued times out. Once it has left (see
/// [`reset_reenumerates`](Self::reset_reenumerates) and [`unplug`](Self::unplug)), every call
/// fails with `Unavailable`.
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

    /// Binds a kernel driver to `interface`: a claim finds it `Busy` until it is detached.
    pub fn bind_kernel_driver(&self, interface: u8) {
        with(&self.state, |s| {
            if !s.kernel_drivers.contains(&interface) {
                s.kernel_drivers.push(interface);
            }
        });
    }

    /// Whether a kernel driver holds `interface`.
    pub fn kernel_driver_bound(&self, interface: u8) -> bool {
        with(&self.state, |s| s.kernel_drivers.contains(&interface))
    }

    /// Every `detach_kernel_driver` so far, by interface, in order.
    pub fn detaches(&self) -> Vec<u8> {
        with(&self.state, |s| s.detaches.clone())
    }

    /// How many times `reset` was called, whatever it returned.
    pub fn resets(&self) -> u32 {
        with(&self.state, |s| s.resets)
    }

    /// Whether a reset makes the device come back as a different one, as a firmware load does:
    /// the reset then returns `Unavailable` and the handle is dead.
    pub fn reset_reenumerates(&self, reenumerates: bool) {
        with(&self.state, |s| s.reset_reenumerates = reenumerates);
    }

    /// The next reset fails with `error` and is not carried out. Queued errors go in order.
    pub fn fail_next_reset(&self, error: ErrorKind) {
        with(&self.state, |s| s.reset_errors.push_back(error));
    }

    /// The device leaves the bus: every call on the handle fails with `Unavailable` from now on.
    pub fn unplug(&self) {
        with(&self.state, |s| s.gone = true);
    }

    /// Whether the device this handle opened has left.
    pub fn is_gone(&self) -> bool {
        with(&self.state, |s| s.gone)
    }

    fn present(&self) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            if s.gone {
                Err(ErrorKind::Unavailable)
            } else {
                Ok(())
            }
        })
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
        self.present()?;
        Ok(with(&self.state, |s| copy(&s.descriptor, out)))
    }

    /// Claims stand across a reset that keeps the device, and alternate settings go back to
    /// none, the stricter of what hosts do.
    fn reset(&mut self) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            s.resets += 1;
            if s.gone {
                return Err(ErrorKind::Unavailable);
            }
            if let Some(error) = s.reset_errors.pop_front() {
                return Err(error);
            }
            s.alternates.clear();
            if s.reset_reenumerates {
                s.gone = true;
                return Err(ErrorKind::Unavailable);
            }
            Ok(())
        })
    }

    fn detach_kernel_driver(&mut self, interface: u8) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            if s.gone {
                return Err(ErrorKind::Unavailable);
            }
            s.detaches.push(interface);
            s.kernel_drivers.retain(|i| *i != interface);
            Ok(())
        })
    }

    fn claim_interface(&mut self, interface: u8) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            if s.gone {
                return Err(ErrorKind::Unavailable);
            }
            if s.claimed.contains(&interface) || s.kernel_drivers.contains(&interface) {
                return Err(ErrorKind::Busy);
            }
            s.claimed.push(interface);
            Ok(())
        })
    }

    fn release_interface(&mut self, interface: u8) -> Result<(), ErrorKind> {
        self.present()?;
        with(&self.state, |s| s.claimed.retain(|i| *i != interface));
        Ok(())
    }

    fn set_alternate(&mut self, interface: u8, alternate: u8) -> Result<(), ErrorKind> {
        self.present()?;
        with(&self.state, |s| s.alternates.insert(interface, alternate));
        Ok(())
    }

    fn control_in(
        &mut self,
        setup: ControlSetup,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.present()?;
        with(&self.state, |s| s.controls.push(setup));
        self.pop_in(data)
    }

    fn control_out(
        &mut self,
        setup: ControlSetup,
        data: &[u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.present()?;
        with(&self.state, |s| s.controls.push(setup));
        Ok(self.record_out(0, data))
    }

    fn bulk_in(
        &mut self,
        _endpoint: u8,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.present()?;
        self.pop_in(data)
    }

    fn bulk_out(
        &mut self,
        endpoint: u8,
        data: &[u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.present()?;
        Ok(self.record_out(endpoint, data))
    }

    fn interrupt_in(
        &mut self,
        _endpoint: u8,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.present()?;
        self.pop_in(data)
    }

    fn iso_in(
        &mut self,
        _endpoint: u8,
        data: &mut [u8],
        packets: &mut [IsoPacket],
        _timeout: Duration,
    ) -> Result<(), ErrorKind> {
        self.present()?;
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

/// How a [`MockUsbFunction`] dealt with a control request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlAnswer {
    /// The data stage it sent.
    In(Vec<u8>),
    /// It took the host's data stage.
    Out,
    Stalled,
}

#[derive(Debug, Default)]
struct UsbFunctionState {
    descriptors: Option<Vec<u8>>,
    configured: bool,
    /// The controller let go: `next_event` fails until the function is enabled again.
    released: bool,
    refuse_enable: Option<ErrorKind>,
    enables: u32,
    events: VecDeque<(UsbFunctionEvent, Vec<u8>)>,
    /// The request being answered, its `wLength`, and the data stage the host sends with it.
    pending: Option<(ControlSetup, u16, Vec<u8>)>,
    answers: Vec<(ControlSetup, ControlAnswer)>,
    halted: Vec<u8>,
    /// IN endpoints still busy with what they were handed before: a `write` times out.
    busy: Vec<u8>,
    from_host: VecDeque<(u8, Vec<u8>)>,
    to_host: Vec<(u8, Vec<u8>)>,
}

impl UsbFunctionState {
    fn named(&self, endpoint: u8) -> Option<UsbEndpoint> {
        let set = UsbDescriptors::parse(self.descriptors.as_deref()?).ok()?;
        usb_descriptors::endpoints(set.full_speed()).find(|e| e.address == endpoint)
    }

    /// The endpoint, if the function is in a state to use it.
    fn usable(&self, endpoint: u8) -> Result<UsbEndpoint, ErrorKind> {
        if self.descriptors.is_none() {
            return Err(ErrorKind::Unavailable);
        }
        let named = self.named(endpoint).ok_or(ErrorKind::InvalidInput)?;
        if !self.configured {
            return Err(ErrorKind::Unavailable);
        }
        Ok(named)
    }

    fn answer(&mut self, answer: ControlAnswer) {
        if let Some((setup, _, _)) = self.pending.take() {
            self.answers.push((setup, answer));
        }
    }

    /// The `wLength` of the pending request, if it is one whose data goes to the host
    /// (`to_host`) or from it.
    fn pending(&self, to_host: bool) -> Result<u16, ErrorKind> {
        if self.descriptors.is_none() {
            return Err(ErrorKind::Unavailable);
        }
        match &self.pending {
            Some((setup, length, _)) if setup.is_in() == to_host => Ok(*length),
            _ => Err(ErrorKind::InvalidInput),
        }
    }
}

/// This board as a USB device, with a test playing the host. It keeps to what
/// [`UsbFunction`] says of states, pending requests and halts, and checks the descriptors.
#[derive(Debug, Clone, Default)]
pub struct MockUsbFunction {
    state: Shared<UsbFunctionState>,
}

impl MockUsbFunction {
    pub fn new() -> Self {
        Self::default()
    }

    /// The descriptors the function is enabled with.
    pub fn descriptors(&self) -> Option<Vec<u8>> {
        with(&self.state, |s| s.descriptors.clone())
    }

    /// How many times `enable` has succeeded.
    pub fn enables(&self) -> u32 {
        with(&self.state, |s| s.enables)
    }

    /// Makes `enable` fail with `kind` until this is called with `None`.
    pub fn refuse_enable(&self, kind: Option<ErrorKind>) {
        with(&self.state, |s| s.refuse_enable = kind);
    }

    /// The controller lets go of the function: `next_event` fails with `Unavailable` until it
    /// is disabled and enabled again.
    pub fn release(&self) {
        with(&self.state, |s| s.released = true);
    }

    /// Something happens on the wire. Use [`request`](Self::request) for a control request
    /// that carries data from the host.
    pub fn event(&self, event: UsbFunctionEvent) {
        with(&self.state, |s| s.events.push_back((event, Vec::new())));
    }

    /// The host makes a control request with a `wLength` of `length`, and `data` as its data
    /// stage if it sends one.
    pub fn request(&self, setup: ControlSetup, length: u16, data: &[u8]) {
        let event = UsbFunctionEvent::Setup { setup, length };
        with(&self.state, |s| s.events.push_back((event, data.to_vec())));
    }

    /// Every control request the function has dealt with, and how.
    pub fn answers(&self) -> Vec<(ControlSetup, ControlAnswer)> {
        with(&self.state, |s| s.answers.clone())
    }

    pub fn is_configured(&self) -> bool {
        with(&self.state, |s| s.configured)
    }

    pub fn is_halted(&self, endpoint: u8) -> bool {
        with(&self.state, |s| s.halted.contains(&endpoint))
    }

    /// The host sends `data` to the OUT endpoint `endpoint`.
    pub fn host_sends(&self, endpoint: u8, data: &[u8]) {
        with(&self.state, |s| {
            s.from_host.push_back((endpoint, data.to_vec()));
        });
    }

    pub fn host_received(&self) -> Vec<(u8, Vec<u8>)> {
        with(&self.state, |s| s.to_host.clone())
    }

    /// The host stops (`busy` true) or goes back to taking what the IN endpoint `endpoint` is
    /// handed. While it is stopped a `write` to the endpoint times out and takes nothing.
    pub fn set_busy(&self, endpoint: u8, busy: bool) {
        with(&self.state, |s| {
            s.busy.retain(|e| *e != endpoint);
            if busy {
                s.busy.push(endpoint);
            }
        });
    }
}

impl UsbFunction for MockUsbFunction {
    type Error = ErrorKind;

    fn enable(&mut self, descriptors: &[u8]) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            if s.descriptors.is_some() {
                return Err(ErrorKind::Busy);
            }
            UsbDescriptors::parse(descriptors)?;
            if let Some(kind) = s.refuse_enable {
                return Err(kind);
            }
            s.descriptors = Some(descriptors.to_vec());
            s.released = false;
            s.enables += 1;
            Ok(())
        })
    }

    fn disable(&mut self) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            s.descriptors = None;
            s.configured = false;
            s.pending = None;
            s.halted.clear();
            s.events.clear();
        });
        Ok(())
    }

    fn next_event(&mut self) -> Result<Option<UsbFunctionEvent>, ErrorKind> {
        with(&self.state, |s| {
            if s.descriptors.is_none() || s.released {
                return Err(ErrorKind::Unavailable);
            }
            s.answer(ControlAnswer::Stalled);
            let Some((event, data)) = s.events.pop_front() else {
                return Ok(None);
            };
            match event {
                UsbFunctionEvent::Configured => {
                    s.configured = true;
                    s.halted.clear();
                }
                UsbFunctionEvent::Deconfigured => {
                    s.configured = false;
                    s.halted.clear();
                }
                UsbFunctionEvent::Setup { setup, length } => {
                    s.pending = Some((setup, length, data));
                }
                UsbFunctionEvent::Suspended | UsbFunctionEvent::Resumed => {}
            }
            Ok(Some(event))
        })
    }

    fn control_in(&mut self, data: &[u8], _timeout: Duration) -> Result<usize, ErrorKind> {
        with(&self.state, |s| {
            let length = s.pending(true)?;
            let sent = &data[..data.len().min(usize::from(length))];
            s.answer(ControlAnswer::In(sent.to_vec()));
            Ok(sent.len())
        })
    }

    fn control_out(&mut self, data: &mut [u8], _timeout: Duration) -> Result<usize, ErrorKind> {
        with(&self.state, |s| {
            let length = usize::from(s.pending(false)?);
            if data.len() < length {
                return Err(ErrorKind::InvalidInput);
            }
            let sent = s.pending.as_ref().map_or(&[][..], |(_, _, sent)| sent);
            let n = copy(&sent[..sent.len().min(length)], data);
            s.answer(ControlAnswer::Out);
            Ok(n)
        })
    }

    fn control_stall(&mut self) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            if s.descriptors.is_none() {
                return Err(ErrorKind::Unavailable);
            }
            if s.pending.is_none() {
                return Err(ErrorKind::InvalidInput);
            }
            s.answer(ControlAnswer::Stalled);
            Ok(())
        })
    }

    fn set_halt(&mut self, endpoint: u8, halted: bool) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            if s.usable(endpoint)?.attributes & 3 == 1 {
                return Err(ErrorKind::Unsupported);
            }
            s.halted.retain(|e| *e != endpoint);
            if halted {
                s.halted.push(endpoint);
            }
            Ok(())
        })
    }

    fn read(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        with(&self.state, |s| {
            if s.usable(endpoint)?.address & 0x80 != 0 {
                return Err(ErrorKind::InvalidInput);
            }
            let at = s.from_host.iter().position(|(e, _)| *e == endpoint);
            let (_, next) = at
                .and_then(|at| s.from_host.remove(at))
                .ok_or(ErrorKind::Timeout)?;
            Ok(copy(&next, data))
        })
    }

    fn write(&mut self, endpoint: u8, data: &[u8], _timeout: Duration) -> Result<usize, ErrorKind> {
        with(&self.state, |s| {
            if s.usable(endpoint)?.address & 0x80 == 0 {
                return Err(ErrorKind::InvalidInput);
            }
            if s.busy.contains(&endpoint) {
                return Err(ErrorKind::Timeout);
            }
            s.to_host.push((endpoint, data.to_vec()));
            Ok(data.len())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::std;
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

    #[test]
    fn a_stream_without_handles_says_so() {
        let mut stream = MockStream::with_bytes(1, 8);
        assert_eq!(
            stream.buffer_handle(0).map(|_| ()),
            Err(ErrorKind::InvalidInput),
            "not held"
        );
        let buffer = stream.next_buffer().unwrap().unwrap();
        assert_eq!(
            stream.buffer_handle(buffer).map(|_| ()),
            Err(ErrorKind::Unsupported)
        );
    }

    /// A file of `len` bytes with no name left, standing in for a dma-buf.
    #[cfg(unix)]
    fn backing(len: u64) -> std::os::fd::OwnedFd {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name = std::format!(
            "lemnos-hal-mock-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(name);
        let file = std::fs::File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        file.set_len(len).unwrap();
        file.into()
    }

    #[cfg(unix)]
    fn through(handle: BufferHandle<'_>) -> std::fs::File {
        handle.fd().try_clone_to_owned().unwrap().into()
    }

    #[cfg(unix)]
    #[test]
    fn a_stream_lends_the_handle_of_a_buffer_the_producer_holds() {
        use std::os::unix::fs::FileExt;
        let mut stream = MockStream::with_handles(vec![backing(8), backing(16)]);
        let consumer = stream.clone();
        assert_eq!(stream.buffer_count(), 2);
        assert_eq!(
            stream.buffer_handle(0).map(|_| ()),
            Err(ErrorKind::InvalidInput),
            "not held"
        );
        assert_eq!(
            stream.buffer_handle(2).map(|_| ()),
            Err(ErrorKind::InvalidInput),
            "no such buffer"
        );
        let first = stream.next_buffer().unwrap().unwrap();
        let second = stream.next_buffer().unwrap().unwrap();
        // Two at once, as a block with an input and an output needs.
        let a = stream.buffer_handle(first).unwrap().unwrap();
        let b = stream.buffer_handle(second).unwrap().unwrap();
        assert_eq!(through(a).metadata().unwrap().len(), 8);
        assert_eq!(through(b).metadata().unwrap().len(), 16);
        // What hardware writes through the handle is what the consumer and `read` see.
        through(a).write_all_at(&[1, 2, 3], 5).unwrap();
        assert_eq!(consumer.contents(first), [0, 0, 0, 0, 0, 1, 2, 3]);
        let mut out = [0; 3];
        assert_eq!(stream.read(first, 5, &mut out), Ok(true));
        assert_eq!(out, [1, 2, 3]);
        // And what `write` and the consumer put in is behind the handle.
        assert_eq!(stream.write(first, 0, &[9, 8]), Ok(true));
        consumer.fill(second, &[7; 16]);
        let handle = stream.buffer_handle(first).unwrap().unwrap();
        through(handle).read_exact_at(&mut out, 0).unwrap();
        assert_eq!(out, [9, 8, 0]);
        let handle = stream.buffer_handle(second).unwrap().unwrap();
        through(handle).read_exact_at(&mut out, 13).unwrap();
        assert_eq!(out, [7; 3]);
        assert_eq!(
            stream.write(first, 7, &[1, 2]),
            Err(ErrorKind::InvalidInput),
            "the range check is the one a stream without handles makes"
        );
        // Published, it isn't the producer's to lend.
        let frame = StreamFrame {
            buffer: first,
            bytes: 8,
            sequence: 1,
            identity: 0,
            timestamp_ns: 0,
        };
        stream.publish(frame).unwrap();
        assert_eq!(
            stream.buffer_handle(first).map(|_| ()),
            Err(ErrorKind::InvalidInput)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_buffer_held_across_a_change_of_consumer_has_no_handle_until_taken_again() {
        let mut stream = MockStream::with_handles(vec![backing(8), backing(8)]);
        let consumer = stream.clone();
        let kept = stream.next_buffer().unwrap().unwrap();
        let sent = stream.next_buffer().unwrap().unwrap();
        let frame = StreamFrame {
            buffer: sent,
            bytes: 8,
            sequence: 1,
            identity: 0,
            timestamp_ns: 0,
        };
        stream.publish(frame).unwrap();
        consumer.post_job(&[1]);
        consumer.change_consumer();
        assert!(stream.buffer_handle(kept).unwrap().is_none());
        assert_eq!(consumer.take_frame(), None, "the old consumer's frame");
        assert_eq!(stream.next_job(&mut [0; 4]), Ok(None), "and its job");
        // What the old consumer had is free again, and has a handle when taken.
        assert_eq!(stream.next_buffer(), Ok(Some(sent)));
        assert!(stream.buffer_handle(sent).unwrap().is_some());
        // Publishing the stale buffer reaches no one and frees it.
        stream
            .publish(StreamFrame {
                buffer: kept,
                ..frame
            })
            .unwrap();
        assert_eq!(consumer.take_frame(), None);
        assert_eq!(stream.next_buffer(), Ok(Some(kept)));
        assert!(stream.buffer_handle(kept).unwrap().is_some());
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
    fn a_kernel_driver_holds_an_interface_until_it_is_detached_and_is_not_put_back() {
        let mut device = MockUsbDevice::new(&[]);
        let probe = device.clone();
        probe.bind_kernel_driver(1);
        assert_eq!(device.claim_interface(1), Err(ErrorKind::Busy));
        assert_eq!(device.detach_kernel_driver(1), Ok(()));
        assert!(!probe.kernel_driver_bound(1));
        device.claim_interface(1).unwrap();
        // Nothing bound: still fine, and it doesn't take the claim away.
        assert_eq!(device.detach_kernel_driver(1), Ok(()));
        assert_eq!(device.detach_kernel_driver(0), Ok(()));
        assert_eq!(probe.claimed(), [1]);
        assert_eq!(probe.detaches(), [1, 1, 0]);
        device.release_interface(1).unwrap();
        assert!(!probe.kernel_driver_bound(1), "release re-attaches nothing");
    }

    #[test]
    fn a_reset_that_keeps_the_device_keeps_the_handle_and_its_claims() {
        let mut device = MockUsbDevice::new(&[9, 2]);
        let probe = device.clone();
        device.claim_interface(0).unwrap();
        device.set_alternate(0, 3).unwrap();
        probe.fail_next_reset(ErrorKind::Timeout);
        assert_eq!(device.reset(), Err(ErrorKind::Timeout));
        assert_eq!(
            probe.alternate(0),
            Some(3),
            "a failed reset changes nothing"
        );
        assert_eq!(device.reset(), Ok(()));
        assert_eq!(probe.resets(), 2);
        assert_eq!(probe.claimed(), [0]);
        assert_eq!(probe.alternate(0), None);
        assert_eq!(device.config_descriptor(0, &mut [0; 2]), Ok(2));
    }

    #[test]
    fn a_reset_that_re_enumerates_leaves_the_handle_dead_for_every_call() {
        let mut device = MockUsbDevice::new(&[9, 2]);
        let probe = device.clone();
        probe.reset_reenumerates(true);
        probe.queue_in(&[1]);
        probe.queue_iso(&[1]);
        assert_eq!(device.reset(), Err(ErrorKind::Unavailable));
        assert!(probe.is_gone());

        let gone = Err(ErrorKind::Unavailable);
        let setup = ControlSetup {
            request_type: 0x80,
            request: 6,
            value: 0,
            index: 0,
        };
        let mut data = [0; 4];
        let mut packets = [IsoPacket {
            capacity: 2,
            ..IsoPacket::default()
        }];
        assert_eq!(device.reset(), Err(ErrorKind::Unavailable));
        assert_eq!(device.config_descriptor(0, &mut data), gone);
        assert_eq!(device.detach_kernel_driver(0), Err(ErrorKind::Unavailable));
        assert_eq!(device.claim_interface(0), Err(ErrorKind::Unavailable));
        assert_eq!(device.release_interface(0), Err(ErrorKind::Unavailable));
        assert_eq!(device.set_alternate(0, 1), Err(ErrorKind::Unavailable));
        assert_eq!(device.control_in(setup, &mut data, WAIT), gone);
        assert_eq!(device.control_out(setup, &[1], WAIT), gone);
        assert_eq!(device.bulk_in(1, &mut data, WAIT), gone);
        assert_eq!(device.bulk_out(1, &[1], WAIT), gone);
        assert_eq!(device.interrupt_in(1, &mut data, WAIT), gone);
        assert_eq!(
            device.iso_in(1, &mut data, &mut packets, WAIT),
            Err(ErrorKind::Unavailable)
        );
        assert_eq!(probe.resets(), 2);
        assert_eq!(probe.written(), []);
    }

    /// One vendor interface with a bulk endpoint each way, at full speed, and no strings.
    const GADGET: [u8; 50] = [
        18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x09, 0x12, 0x01, 0x00, 0x00, 0x01, 0, 0, 0,
        1, // device
        9, 2, 32, 0, 1, 1, 0, 0x80, 50, // configuration
        9, 4, 0, 0, 2, 0xff, 0, 0, 0, // interface
        7, 5, 0x81, 2, 64, 0, 0, // bulk IN 1
        7, 5, 0x02, 2, 64, 0, 0, // bulk OUT 2
    ];

    fn vendor_request(request_type: u8, request: u8) -> ControlSetup {
        ControlSetup {
            request_type,
            request,
            value: 2,
            index: 0,
        }
    }

    #[test]
    fn a_usb_function_connects_and_moves_data_only_while_configured() {
        let mut function = MockUsbFunction::new();
        let host = function.clone();
        let mut data = [0; 2];
        let off = Err(ErrorKind::Unavailable);
        assert_eq!(function.next_event(), Err(ErrorKind::Unavailable));
        assert_eq!(function.read(2, &mut data, WAIT), off);
        assert_eq!(function.enable(&[1, 2]), Err(ErrorKind::InvalidInput));
        function.enable(&GADGET).unwrap();
        assert_eq!(function.enable(&GADGET), Err(ErrorKind::Busy));
        assert_eq!(host.descriptors(), Some(GADGET.to_vec()));

        assert_eq!(function.write(0x81, &[6], WAIT), off, "not configured yet");
        host.event(UsbFunctionEvent::Configured);
        assert_eq!(
            function.next_event(),
            Ok(Some(UsbFunctionEvent::Configured))
        );
        assert_eq!(function.next_event(), Ok(None));
        host.host_sends(2, &[5]);
        assert_eq!(function.read(2, &mut data, WAIT), Ok(1));
        assert_eq!(function.read(2, &mut data, WAIT), Err(ErrorKind::Timeout));
        assert_eq!(function.write(0x81, &[6], WAIT), Ok(1));
        assert_eq!(host.host_received(), [(0x81, [6].to_vec())]);
        let wrong = Err(ErrorKind::InvalidInput);
        assert_eq!(
            function.read(0x81, &mut data, WAIT),
            wrong,
            "an IN endpoint"
        );
        assert_eq!(function.write(2, &[1], WAIT), wrong, "an OUT endpoint");
        assert_eq!(function.write(0x83, &[1], WAIT), wrong, "not in the set");

        function.set_halt(0x81, true).unwrap();
        assert!(host.is_halted(0x81));
        function.set_halt(0x81, false).unwrap();
        assert!(!host.is_halted(0x81));
        function.set_halt(2, true).unwrap();
        assert_eq!(function.set_halt(3, true), Err(ErrorKind::InvalidInput));

        // A reset or a pull: no configuration, no halts, and no need to enable again.
        host.event(UsbFunctionEvent::Deconfigured);
        assert_eq!(
            function.next_event(),
            Ok(Some(UsbFunctionEvent::Deconfigured))
        );
        assert!(!host.is_halted(2));
        assert_eq!(function.write(0x81, &[6], WAIT), off);
        assert_eq!(function.set_halt(2, true), Err(ErrorKind::Unavailable));
        host.event(UsbFunctionEvent::Configured);
        function.next_event().unwrap();
        assert_eq!(function.write(0x81, &[7], WAIT), Ok(1));

        function.disable().unwrap();
        assert_eq!(function.disable(), Ok(()));
        assert_eq!(host.descriptors(), None);
        assert_eq!(function.write(0x81, &[6], WAIT), off);
        function.enable(&GADGET).unwrap();
        assert_eq!(host.enables(), 2);
    }

    #[test]
    fn a_usb_function_answers_takes_or_stalls_a_control_request() {
        let mut function = MockUsbFunction::new();
        let host = function.clone();
        function.enable(&GADGET).unwrap();
        let none = Err(ErrorKind::InvalidInput);
        assert_eq!(function.control_in(&[1], WAIT), none);
        assert_eq!(function.control_stall(), Err(ErrorKind::InvalidInput));

        let to_host = vendor_request(0xc0, 1);
        host.request(to_host, 2, &[]);
        assert_eq!(
            function.next_event(),
            Ok(Some(UsbFunctionEvent::Setup {
                setup: to_host,
                length: 2
            }))
        );
        assert_eq!(function.control_out(&mut [0; 2], WAIT), none, "wrong way");
        assert_eq!(
            function.control_in(&[1, 2, 3], WAIT),
            Ok(2),
            "wLength bounds it"
        );
        assert_eq!(function.control_in(&[1], WAIT), none, "answered once");

        let from_host = vendor_request(0x40, 2);
        host.request(from_host, 3, &[7, 8, 9]);
        function.next_event().unwrap();
        let mut data = [0; 4];
        assert_eq!(function.control_in(&[1], WAIT), none, "wrong way");
        assert_eq!(
            function.control_out(&mut data[..2], WAIT),
            none,
            "too small"
        );
        assert_eq!(function.control_out(&mut data, WAIT), Ok(3));
        assert_eq!(data, [7, 8, 9, 0]);

        let no_data = vendor_request(0x40, 3);
        host.request(no_data, 0, &[]);
        function.next_event().unwrap();
        assert_eq!(function.control_out(&mut [], WAIT), Ok(0));

        host.request(to_host, 2, &[]);
        function.next_event().unwrap();
        assert_eq!(function.control_stall(), Ok(()));

        // Reading on without answering stalls the request.
        host.request(from_host, 3, &[1, 2, 3]);
        function.next_event().unwrap();
        assert_eq!(function.next_event(), Ok(None));
        assert_eq!(function.control_out(&mut data, WAIT), none);

        assert_eq!(
            host.answers(),
            [
                (to_host, ControlAnswer::In([1, 2].to_vec())),
                (from_host, ControlAnswer::Out),
                (no_data, ControlAnswer::Out),
                (to_host, ControlAnswer::Stalled),
                (from_host, ControlAnswer::Stalled),
            ]
        );
    }

    #[test]
    fn a_usb_function_the_controller_let_go_of_fails_until_it_is_enabled_again() {
        let mut function = MockUsbFunction::new();
        let host = function.clone();
        host.refuse_enable(Some(ErrorKind::NotFound));
        assert_eq!(function.enable(&GADGET), Err(ErrorKind::NotFound));
        assert_eq!(
            host.descriptors(),
            None,
            "nothing is left of a failed enable"
        );
        host.refuse_enable(None);
        function.enable(&GADGET).unwrap();
        host.release();
        assert_eq!(function.next_event(), Err(ErrorKind::Unavailable));
        assert_eq!(function.next_event(), Err(ErrorKind::Unavailable));
        function.disable().unwrap();
        function.enable(&GADGET).unwrap();
        assert_eq!(function.next_event(), Ok(None));
    }
}
