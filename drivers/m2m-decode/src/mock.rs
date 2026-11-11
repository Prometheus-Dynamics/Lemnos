//! A node that plays a stateless memory-to-memory device back, for the tests: queues, buffers,
//! requests and events, with failures to order. Its steady-state calls allocate nothing, so a
//! test can count the session's and the decoder's allocations through it.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lemnos_linux_sys::poll;
use lemnos_linux_sys::v4l2::device::ExtControl;
use lemnos_linux_sys::v4l2::{
    self as sys, BufType, BufferInfo, CAP_DEVICE_CAPS, CAP_STREAMING, CAP_VIDEO_M2M,
    CAP_VIDEO_M2M_MPLANE, Capability, Dequeued, Event, Format, MAX_PLANES, Memory, PlaneInfo, Rect,
};

use crate::node::{Node, PlaneMap, Queued};

/// One call the device saw, for checking the order of calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    SetFormat(BufType),
    Format(BufType),
    RequestBuffers(BufType, u32),
    StreamOn(BufType),
    StreamOff(BufType),
    Subscribe(u32),
    QueueRequest,
    ReinitRequest,
    StopDecoder,
}

/// What the device does wrong next, until cleared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Faults {
    /// Opening the node fails.
    pub open: bool,
    /// Queueing a request fails.
    pub queue_request: bool,
    /// Requests never complete.
    pub hang: bool,
    /// The next decoded frame is flagged in error.
    pub frame_error: bool,
    /// The driver answers `S_FMT` with this many planes.
    pub planes: Option<u32>,
    /// `REQBUFS` grants this many.
    pub grant: Option<u32>,
    /// `DQBUF` names this index.
    pub index: Option<u32>,
    /// `poll` reports `POLLERR`.
    pub poll_error: bool,
}

/// A queued buffer: its index, whether its first plane was a dma-buf, and that plane's length,
/// bytes used and data offset.
pub type LastQueued = (u32, bool, u32, u32, u32);

/// The device's state, shared by every node opened on it.
pub struct Device {
    pub m2m: bool,
    pub multiplanar: bool,
    /// The capture pixel format the driver answers with, whatever is asked.
    pub capture_fourcc: Option<u32>,
    pub faults: Faults,
    pub opens: u32,
    pub log: [Option<Call>; 64],
    pub logged: usize,
    pub formats: [Option<Format>; 2],
    pub queued: [u64; 2],
    pub granted: [u32; 2],
    /// Capture buffers holding a frame, oldest first, by index.
    ready: [u32; 64],
    ready_len: usize,
    /// Output buffers the device is done with.
    done_output: u64,
    /// The output buffer in the open request, and whether the request ran.
    in_request: Option<u32>,
    request_done: bool,
    pub requests: u32,
    pub controls: [(u32, u32); 8],
    pub control_count: usize,
    pub events: Vec<Event>,
    pub last_on_next: bool,
    pub sequence: u32,
    /// The last buffer queued on each side: its index, whether its first plane was a dma-buf,
    /// and that plane's length, bytes used and data offset.
    pub last_queued: [Option<LastQueued>; 2],
}

impl Device {
    fn record(&mut self, call: Call) {
        if let Some(slot) = self.log.get_mut(self.logged) {
            *slot = Some(call);
        }
        self.logged += 1;
    }

    /// The calls so far, in order (the first 64).
    pub fn calls(&self) -> impl Iterator<Item = Call> + '_ {
        self.log.iter().flatten().copied()
    }

    pub fn clear_log(&mut self) {
        self.log = [None; 64];
        self.logged = 0;
    }

    fn side(buf_type: BufType) -> usize {
        usize::from(!buf_type.is_output())
    }

    /// Marks capture buffer `index` as holding a frame.
    pub fn produce(&mut self, index: u32) {
        if let Some(slot) = self.ready.get_mut(self.ready_len) {
            *slot = index;
            self.ready_len += 1;
        }
    }

    fn complete_request(&mut self) {
        if let Some(index) = self.in_request.take() {
            self.done_output |= 1 << index;
            let free = self.queued[1] & !self.ready_mask();
            if free != 0 {
                self.produce(free.trailing_zeros());
            }
        }
        self.request_done = true;
    }

    fn ready_mask(&self) -> u64 {
        self.ready[..self.ready_len]
            .iter()
            .fold(0, |m, &i| m | 1 << i)
    }
}

/// A handle on a mock device; each node opened on it is another handle.
#[derive(Clone)]
pub struct Mock(pub Arc<Mutex<Device>>);

impl Mock {
    pub fn new(multiplanar: bool) -> Self {
        Self(Arc::new(Mutex::new(Device {
            m2m: true,
            multiplanar,
            capture_fourcc: None,
            faults: Faults::default(),
            opens: 0,
            log: [None; 64],
            logged: 0,
            formats: [None; 2],
            queued: [0; 2],
            granted: [0; 2],
            ready: [0; 64],
            ready_len: 0,
            done_output: 0,
            in_request: None,
            request_done: false,
            requests: 0,
            controls: [(0, 0); 8],
            control_count: 0,
            events: Vec::new(),
            last_on_next: false,
            sequence: 0,
            last_queued: [None; 2],
        })))
    }

    /// Opens a node on the device (or fails, if ordered to).
    pub fn open(&self) -> io::Result<MockNode> {
        let mut d = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if d.faults.open {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        d.opens += 1;
        // A new open starts from nothing, as the kernel's per-file state does.
        d.queued = [0; 2];
        d.granted = [0; 2];
        d.ready_len = 0;
        d.done_output = 0;
        d.in_request = None;
        Ok(MockNode(self.clone()))
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut Device) -> T) -> T {
        f(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// One open node of a mock device.
pub struct MockNode(Mock);

/// A plane's memory: filled at map time with the byte `offset` names, so a test knows which
/// buffer and plane it is reading.
pub struct MockMap(Box<[u8]>);

impl PlaneMap for MockMap {
    fn bytes(&self) -> &[u8] {
        &self.0
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

/// The byte a mapped plane is filled with.
pub fn fill_byte(capture: bool, index: u32, plane: u32) -> u8 {
    (u32::from(capture) * 0x80 + index * 8 + plane) as u8
}

impl MockNode {
    fn dev(&self) -> std::sync::MutexGuard<'_, Device> {
        self.0.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Bytes of a plane with no size in its format.
const PLANE_BYTES: u32 = 4096;

impl Node for MockNode {
    type Request = u32;
    type Map = MockMap;

    fn capability(&self) -> io::Result<Capability> {
        let d = self.dev();
        let caps = match (d.m2m, d.multiplanar) {
            (false, _) => 0,
            (true, false) => CAP_VIDEO_M2M,
            (true, true) => CAP_VIDEO_M2M_MPLANE,
        };
        Ok(Capability {
            driver: "mock".into(),
            card: "mock".into(),
            bus_info: "platform:mock".into(),
            version: 0,
            capabilities: caps | CAP_STREAMING | CAP_DEVICE_CAPS,
            device_caps: caps | CAP_STREAMING,
        })
    }

    fn set_format(&self, buf_type: BufType, format: &Format) -> io::Result<Format> {
        let mut d = self.dev();
        d.record(Call::SetFormat(buf_type));
        let mut kept = *format;
        if let Format::Multi(m) = &mut kept {
            if let Some(n) = d.faults.planes {
                m.num_planes = n;
            }
            if !buf_type.is_output()
                && let Some(fourcc) = d.capture_fourcc
            {
                m.pixelformat = fourcc;
            }
            for p in m.planes.iter_mut().take(m.num_planes as usize) {
                if p.size_image == 0 {
                    p.size_image = PLANE_BYTES;
                }
            }
        }
        d.formats[Device::side(buf_type)] = Some(kept);
        Ok(kept)
    }

    fn format(&self, buf_type: BufType) -> io::Result<Format> {
        let mut d = self.dev();
        d.record(Call::Format(buf_type));
        d.formats[Device::side(buf_type)]
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))
    }

    fn set_selection(&self, _: BufType, _: u32, _: Rect) -> io::Result<Rect> {
        Err(io::Error::from_raw_os_error(25)) // ENOTTY, as the boards answer
    }

    fn selection(&self, _: BufType, _: u32) -> io::Result<Rect> {
        Err(io::Error::from_raw_os_error(25))
    }

    fn request_buffers(&self, buf_type: BufType, _: Memory, count: u32) -> io::Result<u32> {
        let mut d = self.dev();
        d.record(Call::RequestBuffers(buf_type, count));
        let granted = d.faults.grant.unwrap_or(count);
        let side = Device::side(buf_type);
        d.granted[side] = granted;
        d.queued[side] = 0;
        Ok(granted)
    }

    fn query_buffer(&self, buf_type: BufType, index: u32, planes: u32) -> io::Result<BufferInfo> {
        let d = self.dev();
        let side = Device::side(buf_type);
        let format = d.formats[side].ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut info = BufferInfo {
            index,
            memory: Memory::Mmap as u32,
            flags: 0,
            planes: [PlaneInfo::default(); MAX_PLANES],
            num_planes: planes,
        };
        for (p, plane) in info.planes.iter_mut().enumerate().take(planes as usize) {
            let length = match format {
                Format::Multi(m) => m.planes[p].size_image,
                Format::Single(s) if s.size_image != 0 => s.size_image,
                Format::Single(_) => PLANE_BYTES,
            };
            plane.length = length;
            plane.offset = fill_byte(side == 1, index, p as u32).into();
        }
        Ok(info)
    }

    fn map(&self, plane: &PlaneInfo) -> io::Result<MockMap> {
        Ok(MockMap(
            vec![plane.offset as u8; plane.length as usize].into_boxed_slice(),
        ))
    }

    fn queue(&self, buffer: &Queued<'_, u32>) -> io::Result<()> {
        let mut d = self.dev();
        let side = Device::side(buffer.buf_type);
        if buffer.index >= d.granted[side] {
            return Err(io::Error::from_raw_os_error(22));
        }
        d.queued[side] |= 1 << buffer.index;
        if let Some(plane) = buffer.planes.first() {
            let dma_buf = matches!(plane.memory, sys::PlaneMemory::Dmabuf(_));
            d.last_queued[side] = Some((
                buffer.index,
                dma_buf,
                plane.length,
                plane.bytes_used,
                plane.data_offset,
            ));
        }
        if buffer.request.is_some() {
            d.in_request = Some(buffer.index);
        }
        Ok(())
    }

    fn dequeue(&self, buf_type: BufType, _: Memory, planes: u32) -> io::Result<Option<Dequeued>> {
        let mut d = self.dev();
        let side = Device::side(buf_type);
        let index = if side == 0 {
            if d.done_output == 0 {
                return Ok(None);
            }
            let index = d.done_output.trailing_zeros();
            d.done_output &= !(1 << index);
            index
        } else {
            if d.ready_len == 0 {
                return Ok(None);
            }
            let index = d.ready[0];
            let len = d.ready_len;
            d.ready.copy_within(1..len, 0);
            d.ready_len -= 1;
            index
        };
        d.queued[side] &= !(1 << index);
        let mut flags = 0;
        if side == 1 && d.faults.frame_error {
            d.faults.frame_error = false;
            flags |= sys::BUF_FLAG_ERROR;
        }
        if side == 1 && d.last_on_next {
            d.last_on_next = false;
            flags |= sys::BUF_FLAG_LAST;
        }
        let mut bytes_used = [0; MAX_PLANES];
        if let Some(format) = d.formats[side] {
            for (p, used) in bytes_used.iter_mut().enumerate().take(planes as usize) {
                *used = match format {
                    Format::Multi(m) => m.planes[p].size_image,
                    Format::Single(s) if s.size_image != 0 => s.size_image,
                    Format::Single(_) => PLANE_BYTES,
                };
            }
        }
        d.sequence += 1;
        Ok(Some(Dequeued {
            index: d.faults.index.unwrap_or(index),
            flags,
            field: 1,
            timestamp: Duration::from_millis(5),
            sequence: d.sequence,
            bytes_used,
            data_offset: [0; MAX_PLANES],
            num_planes: planes,
        }))
    }

    fn stream_on(&self, buf_type: BufType) -> io::Result<()> {
        self.dev().record(Call::StreamOn(buf_type));
        Ok(())
    }

    fn stream_off(&self, buf_type: BufType) -> io::Result<()> {
        let mut d = self.dev();
        d.record(Call::StreamOff(buf_type));
        d.queued[Device::side(buf_type)] = 0;
        Ok(())
    }

    fn stop_encoder(&self) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(25))
    }

    fn stop_decoder(&self) -> io::Result<()> {
        let mut d = self.dev();
        d.record(Call::StopDecoder);
        d.last_on_next = true;
        Ok(())
    }

    fn subscribe(&self, event_type: u32, _: u32) -> io::Result<()> {
        self.dev().record(Call::Subscribe(event_type));
        Ok(())
    }

    fn event(&self) -> io::Result<Option<Event>> {
        Ok(self.dev().events.pop())
    }

    fn set_control(&self, _: u32, value: i32) -> io::Result<i32> {
        Ok(value)
    }

    fn control(&self, _: u32) -> io::Result<i32> {
        Ok(0)
    }

    fn poll(&self, events: i16, _: Option<Duration>) -> io::Result<i16> {
        let d = self.dev();
        if d.faults.poll_error {
            return Ok(poll::POLLERR);
        }
        let mut revents = 0;
        if d.ready_len > 0 {
            revents |= poll::POLLIN;
        }
        if d.queued[0].count_ones() < d.granted[0] {
            revents |= poll::POLLOUT;
        }
        if !d.events.is_empty() {
            revents |= poll::POLLPRI;
        }
        Ok(revents & events)
    }

    fn new_request(&self) -> io::Result<u32> {
        let mut d = self.dev();
        d.requests += 1;
        Ok(d.requests)
    }

    fn set_request_controls(&self, _: &u32, controls: &mut [ExtControl<'_>]) -> io::Result<()> {
        let mut d = self.dev();
        d.control_count = 0;
        for c in controls.iter() {
            let n = d.control_count;
            if let Some(slot) = d.controls.get_mut(n) {
                *slot = (c.id(), c.size());
                d.control_count += 1;
            }
        }
        Ok(())
    }

    fn queue_request(&self, _: &u32) -> io::Result<()> {
        let mut d = self.dev();
        d.record(Call::QueueRequest);
        if d.faults.queue_request {
            return Err(io::Error::from_raw_os_error(22));
        }
        d.request_done = false;
        if !d.faults.hang {
            d.complete_request();
        }
        Ok(())
    }

    fn reinit_request(&self, _: &u32) -> io::Result<()> {
        let mut d = self.dev();
        d.record(Call::ReinitRequest);
        d.request_done = false;
        Ok(())
    }

    fn wait_request(&self, _: &u32, _: Option<Duration>) -> io::Result<bool> {
        Ok(self.dev().request_done)
    }
}
