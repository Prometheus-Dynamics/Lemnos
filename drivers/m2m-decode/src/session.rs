//! Memory-to-memory sessions: one open device that takes frames on its *output* queue (the
//! source) and returns them, converted or encoded or decoded, on its *capture* queue (the
//! destination). Stateless and stateful decoders, encoders and scalers all work this way.
//!
//! A session holds one device node with both queues on it. Each queue's buffers are either the
//! driver's MMAP memory (written and read in place through the mapping) or dma-bufs the caller
//! passes at every queue call (the device reads or writes them where they are: no copy). Frames
//! are described by index: a buffer index belongs to the caller while it is dequeued, and to the
//! device between `queue_*` and `dequeue_*`.
//!
//! Order matters: the capture buffers are queued before [`M2mSession::stream_on`] (the device
//! needs somewhere to put frames), and [`M2mSession::stream_off`] gives every buffer back. A
//! decoder or encoder is drained with [`M2mSession::drain`]: the frames already queued finish,
//! and the last capture buffer comes back flagged `BUF_FLAG_LAST`.
//!
//! Waiting: a device's output queue is "ready" when a source buffer is free, so waiting for the
//! capture side ([`Ready::capture`]) is what says a frame is out. Dequeues never block: they
//! return `None` when nothing is ready.
//!
//! Everything the device reports (buffer indices, plane counts, plane sizes) is checked against
//! what the session holds before it is used to index or slice. Mappings and queue records are
//! made when buffers are requested; queueing, dequeueing and waiting allocate nothing.

use std::io;
use std::time::Duration;

use lemnos_linux_sys::poll;
use lemnos_linux_sys::v4l2::device::ExtControl;
use lemnos_linux_sys::v4l2::{
    self as sys, BufType, Capability, Dequeued, Event, Format, MAX_PLANES, Memory, PlaneMemory,
    QueuePlane, Rect,
};

use crate::node::{Node, PlaneMap, Queued};

/// A source-change event: the stream's format changed. A stateful decoder reports a new coded
/// size this way (see [`M2mSession::take_source_change`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceChange {
    /// The `changes` flags of the event (`SRC_CH_*`).
    pub changes: u32,
}

impl SourceChange {
    /// Whether the resolution changed (the capture buffers must be re-requested).
    pub fn resolution(&self) -> bool {
        self.changes & sys::SRC_CH_RESOLUTION != 0
    }
}

/// Most buffers one queue may have (the session tracks them in a 64-bit mask).
pub const MAX_BUFFERS: u32 = 64;

/// How one queue is set up when a session opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueSetup {
    /// The format to set; its single- or multi-planar layout gives the queue's type.
    pub format: Format,
    /// Where the queue's buffers live: [`Memory::Mmap`] or [`Memory::DmaBuf`].
    pub memory: Memory,
    /// Buffers to ask for (at least 1; the driver may grant more or fewer). A capture queue
    /// asked for 0 is deferred until the stream says its format (a stateful decoder).
    pub buffers: u32,
    /// The queue's crop rectangle (`VIDIOC_S_SELECTION`, `V4L2_SEL_TGT_CROP`), set after the
    /// format and before the buffers are requested. `None` leaves the driver's default. A
    /// device that does not crop this queue refuses the open.
    pub crop: Option<Rect>,
}

/// What a drain asks the device to finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Drain {
    /// An encoder (`VIDIOC_ENCODER_CMD`, `ENC_CMD_STOP`).
    Encoder,
    /// A decoder (`VIDIOC_DECODER_CMD`, `DEC_CMD_STOP`).
    Decoder,
}

/// Which readiness to wait for, and what [`M2mSession::wait`] reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ready {
    /// A source buffer is free (the device has finished with one).
    pub output: bool,
    /// A destination buffer holds a frame.
    pub capture: bool,
    /// An event is pending ([`M2mSession::dequeue_event`]).
    pub event: bool,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// The memory planes of `format`, refused outside 1 to [`MAX_PLANES`] (the kernel's count is
/// untrusted).
fn planes_of(format: &Format) -> io::Result<u32> {
    let n = format.num_planes();
    if (1..=MAX_PLANES).contains(&n) {
        Ok(n as u32)
    } else {
        Err(invalid_data("a format with no planes or more than 8"))
    }
}

/// One queue of a session: its layout, its buffers, and which of them the device holds.
struct Queue<M> {
    buf_type: BufType,
    memory: Memory,
    format: Format,
    planes: u32,
    count: u32,
    /// Bit `i` set: buffer `i` is queued (the device holds it).
    queued: u64,
    /// For MMAP queues: per buffer, one mapping per plane.
    maps: Vec<Vec<M>>,
    /// The crop rectangle the driver kept, when one was set.
    crop: Option<Rect>,
}

impl<M: PlaneMap> Queue<M> {
    /// Sets the queue's format (and crop). No buffers yet: see [`Queue::allocate`].
    fn set_format<N: Node<Map = M>>(
        node: &N,
        setup: &QueueSetup,
        output: bool,
    ) -> io::Result<Self> {
        if setup.memory == Memory::UserPtr {
            return Err(invalid(
                "user-pointer buffers are not supported: use Memory::Mmap or Memory::DmaBuf",
            ));
        }
        let wanted = planes_of(&setup.format)?;
        let buf_type = queue_type(&setup.format, output);
        let format = node.set_format(buf_type, &setup.format)?;
        if planes_of(&format)? != wanted {
            return Err(invalid_data("the driver changed a queue's plane count"));
        }
        let crop = match setup.crop {
            Some(rect) => Some(node.set_selection(buf_type, sys::SEL_TGT_CROP, rect)?),
            None => None,
        };
        Ok(Self {
            buf_type,
            memory: setup.memory,
            format,
            planes: wanted,
            count: 0,
            queued: 0,
            maps: Vec::new(),
            crop,
        })
    }

    /// Reads the format back from the driver, after the other queue's format was set: an
    /// encoder resizes its coded buffers (`sizeimage`) from the source format, and a decoder's
    /// capture format follows the stream.
    fn refresh_format<N: Node<Map = M>>(&mut self, node: &N) -> io::Result<()> {
        let format = node.format(self.buf_type)?;
        if planes_of(&format)? != self.planes {
            return Err(invalid_data("the driver changed a queue's plane count"));
        }
        self.format = format;
        Ok(())
    }

    /// Requests `buffers` buffers of the queue's memory kind and, for MMAP, maps them.
    fn allocate<N: Node<Map = M>>(&mut self, node: &N, buffers: u32) -> io::Result<()> {
        let granted = node.request_buffers(self.buf_type, self.memory, buffers.max(1))?;
        if granted == 0 || granted > MAX_BUFFERS {
            return Err(invalid_data(
                "the driver granted no buffers, or more than 64",
            ));
        }
        self.count = granted;
        self.maps.clear();
        if self.memory == Memory::Mmap {
            self.maps.reserve(granted as usize);
            for index in 0..granted {
                let info = node.query_buffer(self.buf_type, index, self.planes)?;
                if info.num_planes != self.planes {
                    return Err(invalid_data("QUERYBUF gave another plane count"));
                }
                let mut buffer = Vec::with_capacity(self.planes as usize);
                for plane in info.planes.iter().take(self.planes as usize) {
                    buffer.push(node.map(plane)?);
                }
                self.maps.push(buffer);
            }
        }
        Ok(())
    }

    fn check_queueable(&self, index: u32, planes: &[QueuePlane<'_>]) -> io::Result<()> {
        if index >= self.count {
            return Err(invalid("a buffer index past the queue"));
        }
        if self.is_queued(index) {
            return Err(invalid("the buffer is already queued"));
        }
        if planes.len() != self.planes as usize {
            return Err(invalid("a plane count other than the queue's"));
        }
        let want_mmap = self.memory == Memory::Mmap;
        if planes
            .iter()
            .any(|p| matches!(p.memory, PlaneMemory::Mmap) != want_mmap)
        {
            return Err(invalid("plane memory does not match the queue's buffers"));
        }
        Ok(())
    }

    fn is_queued(&self, index: u32) -> bool {
        index < MAX_BUFFERS && self.queued & (1 << index) != 0
    }

    fn free(&self) -> Option<u32> {
        (0..self.count).find(|&i| !self.is_queued(i))
    }

    fn dequeue<N: Node<Map = M>>(&mut self, node: &N) -> io::Result<Option<Dequeued>> {
        let Some(done) = node.dequeue(self.buf_type, self.memory, self.planes)? else {
            return Ok(None);
        };
        let index = done.checked_index(self.count as usize)?;
        self.queued &= !(1 << index);
        Ok(Some(done))
    }
}

/// The queue type a format takes: output for the source side, capture for the destination,
/// single- or multi-planar from the format.
pub fn queue_type(format: &Format, output: bool) -> BufType {
    match (format.is_multiplanar(), output) {
        (false, false) => BufType::VideoCapture,
        (false, true) => BufType::VideoOutput,
        (true, false) => BufType::VideoCaptureMplane,
        (true, true) => BufType::VideoOutputMplane,
    }
}

/// One memory-to-memory device with both queues open. Dropping the session closes the node; the
/// kernel stops streaming and frees the buffers.
pub struct M2mSession<N: Node> {
    node: N,
    capabilities: Capability,
    output: Queue<N::Map>,
    capture: Queue<N::Map>,
    /// The capture queue has no buffers yet: its format follows the stream (a stateful
    /// decoder), and [`M2mSession::adopt_capture`] allocates them once the source is known.
    capture_deferred: bool,
    /// The capture queue was streamed on (by `stream_on` or `start_capture`).
    capture_streaming: bool,
    /// The LAST buffer of a drain has been dequeued. A resolution change is reconfigured only
    /// after this (the drain comes first).
    last_seen: bool,
    /// The visible rectangle of a capture frame (`SEL_TGT_COMPOSE`), when the driver reports one.
    capture_visible: Option<Rect>,
}

#[cfg(target_os = "linux")]
impl M2mSession<crate::LinuxNode> {
    /// Opens the memory-to-memory device at `path` (with its media device, for requests) and
    /// sets up both queues (see [`M2mSession::new`]).
    pub fn open(
        path: impl AsRef<std::path::Path>,
        output: QueueSetup,
        capture: QueueSetup,
    ) -> io::Result<Self> {
        Self::new(crate::LinuxNode::open(path)?, output, capture)
    }
}

impl<N: Node> M2mSession<N> {
    /// Sets up both queues on `node`.
    ///
    /// Order: the output (source) format, then the capture format, then the capture format is
    /// read back (an encoder resizes its coded buffers from the source), and only then are the
    /// buffers requested. An encoder refuses a source format (`EBUSY`) once its capture buffers
    /// exist, so the source comes first. The formats the driver kept are
    /// [`Self::output_format`] and [`Self::capture_format`]: `S_FMT` adjusts a format it does
    /// not take rather than failing, so check the pixel format before relying on it.
    ///
    /// Fails with [`io::ErrorKind::InvalidInput`] when the node is not a memory-to-memory
    /// device, or when a queue's setup is refused.
    pub fn new(node: N, output: QueueSetup, capture: QueueSetup) -> io::Result<Self> {
        let capabilities = node.capability()?;
        if !capabilities.is_m2m() {
            return Err(invalid("the node is not a memory-to-memory device"));
        }
        let deferred = capture.buffers == 0;
        let mut output_queue = Queue::set_format(&node, &output, true)?;
        let mut capture_queue = Queue::set_format(&node, &capture, false)?;
        capture_queue.refresh_format(&node)?;
        if !deferred {
            capture_queue.allocate(&node, capture.buffers)?;
        }
        output_queue.allocate(&node, output.buffers)?;
        // Source changes are reported for any driver that has them; a driver without them
        // refuses the subscription, which is no error.
        let _ = node.subscribe(sys::EVENT_SOURCE_CHANGE, 0);
        let capture_visible = if deferred {
            None
        } else {
            visible_rect(&node, capture_queue.buf_type)
        };
        Ok(Self {
            node,
            capabilities,
            output: output_queue,
            capture: capture_queue,
            capture_deferred: deferred,
            capture_streaming: false,
            last_seen: false,
            capture_visible,
        })
    }

    /// The node the session runs on.
    pub fn node(&self) -> &N {
        &self.node
    }

    /// Sets control `id` on the session's node (`VIDIOC_S_CTRL`) and returns the value the
    /// driver kept. Controls are per file handle, so this is the handle the queues are on.
    pub fn set_control(&mut self, id: u32, value: i32) -> io::Result<i32> {
        self.node.set_control(id, value)
    }

    /// Reads control `id` on the session's node (`VIDIOC_G_CTRL`).
    pub fn control(&self, id: u32) -> io::Result<i32> {
        self.node.control(id)
    }

    /// The device's capabilities, as `QUERYCAP` reported them.
    pub fn capabilities(&self) -> &Capability {
        &self.capabilities
    }

    /// The source queue's format, as the driver took it.
    pub fn output_format(&self) -> &Format {
        &self.output.format
    }

    /// The destination queue's format, as the driver took it.
    pub fn capture_format(&self) -> &Format {
        &self.capture.format
    }

    /// The source queue's crop rectangle as the driver kept it, when one was set.
    pub fn output_crop(&self) -> Option<Rect> {
        self.output.crop
    }

    /// The destination queue's crop rectangle as the driver kept it, when one was set.
    pub fn capture_crop(&self) -> Option<Rect> {
        self.capture.crop
    }

    /// Sets the source queue's crop rectangle (`VIDIOC_S_SELECTION`) and returns the one the
    /// driver kept. The device refuses the change while the queue streams (`EBUSY` on most).
    pub fn set_output_crop(&mut self, rect: Rect) -> io::Result<Rect> {
        let kept = self
            .node
            .set_selection(self.output.buf_type, sys::SEL_TGT_CROP, rect)?;
        self.output.crop = Some(kept);
        Ok(kept)
    }

    /// The source queue's buffer count.
    pub fn output_buffers(&self) -> u32 {
        self.output.count
    }

    /// The destination queue's buffer count.
    pub fn capture_buffers(&self) -> u32 {
        self.capture.count
    }

    /// The source queue's memory planes per buffer.
    pub fn output_planes(&self) -> u32 {
        self.output.planes
    }

    /// The destination queue's memory planes per buffer.
    pub fn capture_planes(&self) -> u32 {
        self.capture.planes
    }

    /// The source queue's memory kind.
    pub fn output_memory(&self) -> Memory {
        self.output.memory
    }

    /// The destination queue's memory kind.
    pub fn capture_memory(&self) -> Memory {
        self.capture.memory
    }

    /// The lowest source buffer the device does not hold, if any.
    pub fn free_output(&self) -> Option<u32> {
        self.output.free()
    }

    /// The lowest destination buffer the device does not hold, if any.
    pub fn free_capture(&self) -> Option<u32> {
        self.capture.free()
    }

    /// Plane `plane` of source buffer `index`, for writing before it is queued. `None` for
    /// dma-buf queues, for a buffer the device holds, or for a bad index.
    pub fn output_plane_mut(&mut self, index: u32, plane: usize) -> Option<&mut [u8]> {
        if self.output.memory != Memory::Mmap || self.output.is_queued(index) {
            return None;
        }
        self.output
            .maps
            .get_mut(index as usize)?
            .get_mut(plane)
            .map(PlaneMap::bytes_mut)
    }

    /// Plane `plane` of source buffer `index`, for reading back what was written. `None` as
    /// for [`Self::output_plane_mut`].
    pub fn output_plane(&self, index: u32, plane: usize) -> Option<&[u8]> {
        if self.output.memory != Memory::Mmap || self.output.is_queued(index) {
            return None;
        }
        self.output
            .maps
            .get(index as usize)?
            .get(plane)
            .map(PlaneMap::bytes)
    }

    /// Plane `plane` of destination buffer `index`, for reading after it was dequeued. `None`
    /// for dma-buf queues, for a buffer the device holds, or for a bad index. The data is the
    /// dequeued buffer's `bytes_used` long ([`sys::payload_range`] checks it).
    pub fn capture_plane(&self, index: u32, plane: usize) -> Option<&[u8]> {
        if self.capture.memory != Memory::Mmap || self.capture.is_queued(index) {
            return None;
        }
        self.capture
            .maps
            .get(index as usize)?
            .get(plane)
            .map(PlaneMap::bytes)
    }

    /// Queues source buffer `index` with its planes and the frame's timestamp. For dma-bufs the
    /// caller may close its descriptors afterwards (the device holds its own reference).
    pub fn queue_output(
        &mut self,
        index: u32,
        planes: &[QueuePlane<'_>],
        timestamp: Duration,
    ) -> io::Result<()> {
        self.queue_output_with(index, planes, timestamp, None)
    }

    /// Queues source buffer `index` as part of `request` (a stateless codec's frame): the
    /// device runs it when the request is queued, and the buffer comes back by
    /// [`Self::dequeue_output`] once the request has completed. Set the request's controls with
    /// [`Self::set_request_controls`].
    pub fn queue_output_in_request(
        &mut self,
        index: u32,
        planes: &[QueuePlane<'_>],
        timestamp: Duration,
        request: &N::Request,
    ) -> io::Result<()> {
        self.queue_output_with(index, planes, timestamp, Some(request))
    }

    fn queue_output_with(
        &mut self,
        index: u32,
        planes: &[QueuePlane<'_>],
        timestamp: Duration,
        request: Option<&N::Request>,
    ) -> io::Result<()> {
        self.output.check_queueable(index, planes)?;
        self.node.queue(&Queued {
            buf_type: self.output.buf_type,
            memory: self.output.memory,
            index,
            planes,
            timestamp,
            request,
        })?;
        self.output.queued |= 1 << index;
        Ok(())
    }

    /// Sets controls in `request` (`VIDIOC_S_EXT_CTRLS` on this session's node, with the
    /// request's values): they apply when the request runs. The payloads are the caller's,
    /// borrowed for the call, so nothing is copied or allocated.
    pub fn set_request_controls(
        &self,
        request: &N::Request,
        controls: &mut [ExtControl<'_>],
    ) -> io::Result<()> {
        self.node.set_request_controls(request, controls)
    }

    /// Queues destination buffer `index` with its planes, for the device to fill.
    pub fn queue_capture(&mut self, index: u32, planes: &[QueuePlane<'_>]) -> io::Result<()> {
        self.capture.check_queueable(index, planes)?;
        self.node.queue(&Queued {
            buf_type: self.capture.buf_type,
            memory: self.capture.memory,
            index,
            planes,
            timestamp: Duration::ZERO,
            request: None,
        })?;
        self.capture.queued |= 1 << index;
        Ok(())
    }

    /// Takes back a source buffer the device has finished reading, or `None` when none is done.
    pub fn dequeue_output(&mut self) -> io::Result<Option<Dequeued>> {
        self.output.dequeue(&self.node)
    }

    /// Takes back a destination buffer holding a frame, or `None` when none is ready. After a
    /// drain, `None` also comes once the LAST buffer has been taken. Read a frame with
    /// [`Self::capture_plane`], then queue it again.
    pub fn dequeue_capture(&mut self) -> io::Result<Option<Dequeued>> {
        let done = self.capture.dequeue(&self.node)?;
        if let Some(d) = &done {
            self.last_seen |= d.flags & sys::BUF_FLAG_LAST != 0;
        }
        Ok(done)
    }

    /// Takes the pending events and says whether one of them reports a source change. Other
    /// events taken here are consumed (subscribe only to what you use).
    pub fn take_source_change(&self) -> io::Result<Option<SourceChange>> {
        let mut change = None;
        while let Some(event) = self.node.event()? {
            if event.event_type == sys::EVENT_SOURCE_CHANGE {
                change = Some(SourceChange {
                    changes: event.changes,
                });
            }
        }
        Ok(change)
    }

    /// Whether the LAST buffer of a drain has been taken: the stream is drained, and
    /// [`Self::reconfigure_capture`] may run.
    pub fn capture_drained(&self) -> bool {
        self.last_seen
    }

    /// Whether the capture queue is still waiting for its buffers (see
    /// [`Self::adopt_capture`]).
    pub fn capture_pending(&self) -> bool {
        self.capture_deferred
    }

    /// The visible rectangle of a capture frame, when the driver reports one.
    pub fn capture_visible(&self) -> Option<Rect> {
        self.capture_visible
    }

    /// Reads the capture format the driver now has (after a source change) and allocates
    /// `buffers` capture buffers for it. The capture queue must be pending; queue the buffers,
    /// then [`Self::start_capture`].
    pub fn adopt_capture(&mut self, buffers: u32) -> io::Result<()> {
        if !self.capture_deferred {
            return Err(invalid("the capture buffers are allocated already"));
        }
        self.capture.refresh_format(&self.node)?;
        self.capture_visible = visible_rect(&self.node, self.capture.buf_type);
        self.capture.allocate(&self.node, buffers)?;
        self.capture_deferred = false;
        Ok(())
    }

    /// Starts the capture queue (after its buffers are queued): when it was deferred, or to
    /// resume it after [`Self::reconfigure_capture`].
    pub fn start_capture(&mut self) -> io::Result<()> {
        if self.capture_deferred {
            return Err(invalid("the capture buffers are not allocated yet"));
        }
        self.node.stream_on(self.capture.buf_type)?;
        self.capture_streaming = true;
        self.last_seen = false;
        Ok(())
    }

    /// Reconfigures the capture queue after a resolution change, without touching the output
    /// queue: stops the capture queue, frees its buffers, reads the new format and allocates
    /// `buffers`. The stream must have been drained first (the LAST buffer dequeued). Then queue
    /// the buffers and call [`Self::start_capture`].
    pub fn reconfigure_capture(&mut self, buffers: u32) -> io::Result<()> {
        if !self.last_seen {
            return Err(invalid(
                "drain the stream first: take its LAST capture buffer before reconfiguring",
            ));
        }
        if self.capture_streaming {
            self.node.stream_off(self.capture.buf_type)?;
        }
        self.capture_streaming = false;
        self.capture.queued = 0;
        self.capture.maps.clear();
        self.node
            .request_buffers(self.capture.buf_type, self.capture.memory, 0)?;
        self.capture.count = 0;
        self.capture.refresh_format(&self.node)?;
        self.capture_visible = visible_rect(&self.node, self.capture.buf_type);
        self.capture.allocate(&self.node, buffers)?;
        self.capture_deferred = false;
        self.last_seen = false;
        Ok(())
    }

    /// Waits until one of the readiness flags in `want` holds or `timeout` passes (`None`:
    /// forever), and reports which of them hold. No flags on timeout. Device errors and hang-ups
    /// are errors.
    pub fn wait(&self, want: Ready, timeout: Option<Duration>) -> io::Result<Ready> {
        let mut events = 0;
        if want.output {
            events |= poll::POLLOUT;
        }
        if want.capture {
            events |= poll::POLLIN;
        }
        if want.event {
            events |= poll::POLLPRI;
        }
        let revents = self.node.poll(events, timeout)?;
        if revents & (poll::POLLERR | poll::POLLHUP | poll::POLLNVAL) != 0 {
            return Err(io::Error::other("the device reported an error or hung up"));
        }
        Ok(Ready {
            output: want.output && revents & poll::POLLOUT != 0,
            capture: want.capture && revents & poll::POLLIN != 0,
            event: want.event && revents & poll::POLLPRI != 0,
        })
    }

    /// Starts streaming both queues, the destination first so the device has somewhere to put
    /// the first frame. A deferred capture queue is not started here: only the output queue
    /// starts, and [`Self::start_capture`] starts the capture queue once its buffers exist.
    pub fn stream_on(&mut self) -> io::Result<()> {
        if !self.capture_deferred {
            self.node.stream_on(self.capture.buf_type)?;
            self.capture_streaming = true;
        }
        self.node.stream_on(self.output.buf_type)
    }

    /// Stops both queues. Every buffer the device held comes back to the caller.
    pub fn stream_off(&mut self) -> io::Result<()> {
        self.node.stream_off(self.output.buf_type)?;
        self.node.stream_off(self.capture.buf_type)?;
        self.capture_streaming = false;
        self.output.queued = 0;
        self.capture.queued = 0;
        Ok(())
    }

    /// Asks the device to finish the frames queued so far. The last capture buffer comes back
    /// flagged `BUF_FLAG_LAST`, and an `EVENT_EOS` event follows it.
    pub fn drain(&self, which: Drain) -> io::Result<()> {
        match which {
            Drain::Encoder => self.node.stop_encoder(),
            Drain::Decoder => self.node.stop_decoder(),
        }
    }

    /// Subscribes to events of `event_type`. Control events take the control's id in `id`;
    /// other events take 0. A driver may refuse an event type it does not produce (`EINVAL`).
    pub fn subscribe(&self, event_type: u32, id: u32) -> io::Result<()> {
        self.node.subscribe(event_type, id)
    }

    /// Takes the next pending event, or `None` when there is none.
    pub fn dequeue_event(&self) -> io::Result<Option<Event>> {
        self.node.event()
    }
}

/// The visible rectangle of a queue (`SEL_TGT_COMPOSE`), or `None` when the driver does not
/// report one.
fn visible_rect<N: Node>(node: &N, buf_type: BufType) -> Option<Rect> {
    node.selection(buf_type, sys::SEL_TGT_COMPOSE).ok()
}

#[cfg(test)]
mod tests;
