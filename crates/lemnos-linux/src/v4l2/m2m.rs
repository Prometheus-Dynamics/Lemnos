//! Memory-to-memory sessions: one open device that takes frames on its
//! *output* queue (the source) and returns them, converted or encoded or
//! decoded, on its *capture* queue (the destination). Scalers and colour
//! converters, stateful encoders and decoders all work this way.
//!
//! A session holds one device node with both queues on it. Each queue's
//! buffers are either the driver's MMAP memory (copied into and read in place
//! through the mapping) or dma-bufs the caller passes at every queue call (the
//! device reads or writes them where they are: no copy). Frames are described
//! by index: a buffer index belongs to the caller while it is dequeued, and to
//! the device between `queue_*` and `dequeue_*`.
//!
//! ```no_run
//! use lemnos_linux::v4l2::m2m::{M2mSession, QueueSetup};
//! use lemnos_linux::v4l2::{Format, Memory, PixFormat, PlaneMemory, QueuePlane, fourcc};
//! use std::time::Duration;
//!
//! let rgb = Format::Single(PixFormat {
//!     width: 64,
//!     height: 48,
//!     pixelformat: fourcc(b"RGB3"),
//!     bytes_per_line: 192,
//!     size_image: 9216,
//!     ..Default::default()
//! });
//! let setup = |memory| QueueSetup { format: rgb, memory, buffers: 2, crop: None };
//! let mut s = M2mSession::open("/dev/video0", setup(Memory::Mmap), setup(Memory::Mmap))?;
//! let frame = [0u8; 9216];
//! let mmap = [QueuePlane { memory: PlaneMemory::Mmap, length: 0, bytes_used: 0, data_offset: 0 }];
//! for index in 0..2 {
//!     s.queue_capture(index, &mmap)?;
//! }
//! s.stream_on()?;
//! let out = s.free_output().expect("a free source buffer");
//! s.output_plane_mut(out, 0).expect("mapped").copy_from_slice(&frame);
//! let used = [QueuePlane { bytes_used: 9216, ..mmap[0] }];
//! s.queue_output(out, &used, Duration::from_millis(1))?;
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! Order matters: the capture buffers are queued before [`M2mSession::stream_on`]
//! (the device needs somewhere to put frames), and [`M2mSession::stream_off`]
//! gives every buffer back. A decoder or encoder is drained with
//! [`M2mSession::drain`]: the frames already queued finish, and the last capture
//! buffer comes back flagged [`BUF_FLAG_LAST`](super::BUF_FLAG_LAST).
//!
//! Waiting: a device's output queue is "ready" when a source buffer is free,
//! so waiting for the capture side ([`Ready::capture`]) is what says a frame
//! is out. Dequeues never block: they return `None` when nothing is ready.

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;
use std::time::Duration;

use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::poll::{self, PollFd};
use lemnos_linux_sys::v4l2 as sys;

use super::{BufType, Capability, Dequeued, Event, Format, Memory, PlaneMemory, QueuePlane, Rect};

/// Most buffers one queue may have (the session tracks them in a 64-bit mask).
pub const MAX_BUFFERS: u32 = 64;

/// How one queue is set up when a session opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueSetup {
    /// The format to set; its single- or multi-planar layout gives the queue's type.
    pub format: Format,
    /// Where the queue's buffers live.
    pub memory: Memory,
    /// Buffers to ask for (at least 1; the driver may grant more or fewer).
    pub buffers: u32,
    /// The queue's crop rectangle (`VIDIOC_S_SELECTION`, `V4L2_SEL_TGT_CROP`),
    /// set after the format and before the buffers are requested. `None`
    /// leaves the driver's default. The rectangle the driver kept is read
    /// back with [`M2mSession::output_crop`] or [`M2mSession::capture_crop`].
    /// A device that does not crop this queue refuses the open.
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

/// One queue of a session: its layout, its buffers, and which of them the
/// device holds.
struct Queue {
    buf_type: BufType,
    memory: Memory,
    format: Format,
    count: u32,
    /// Bit `i` set: buffer `i` is queued (the device holds it).
    queued: u64,
    /// For MMAP queues: per buffer, one mapping per plane.
    maps: Vec<Vec<Mapping>>,
    /// The crop rectangle the driver kept, when one was set.
    crop: Option<Rect>,
}

impl Queue {
    fn open(fd: BorrowedFd<'_>, setup: &QueueSetup, output: bool) -> io::Result<Self> {
        let buf_type = queue_type(&setup.format, output);
        let format = sys::s_fmt(fd, buf_type, &setup.format)?;
        if format.num_planes() != setup.format.num_planes() {
            return Err(invalid("the driver changed a queue's plane count".into()));
        }
        let crop = match setup.crop {
            Some(rect) => Some(sys::s_selection(fd, buf_type, sys::SEL_TGT_CROP, rect)?),
            None => None,
        };
        let wanted = setup.buffers.max(1);
        let granted = sys::reqbufs(fd, buf_type, setup.memory, wanted)?.count;
        if granted == 0 || granted > MAX_BUFFERS {
            return Err(invalid(format!(
                "{granted} buffers granted (1 to {MAX_BUFFERS} supported)"
            )));
        }
        let mut maps = Vec::new();
        if setup.memory == Memory::Mmap {
            let planes = format.num_planes() as u32;
            maps.reserve(granted as usize);
            for index in 0..granted {
                let info = sys::querybuf(fd, buf_type, index, planes)?;
                let mut buffer = Vec::with_capacity(planes as usize);
                for plane in info.planes.iter().take(planes as usize) {
                    buffer.push(Mapping::new(
                        fd,
                        plane.length as usize,
                        u64::from(plane.offset),
                        true,
                    )?);
                }
                maps.push(buffer);
            }
        }
        Ok(Self {
            buf_type,
            memory: setup.memory,
            format,
            count: granted,
            queued: 0,
            maps,
            crop,
        })
    }

    fn planes(&self) -> u32 {
        self.format.num_planes() as u32
    }

    fn check_index(&self, index: u32) -> io::Result<()> {
        if index >= self.count {
            return Err(invalid(format!("buffer {index} of {}", self.count)));
        }
        Ok(())
    }

    fn check_queueable(&self, index: u32, planes: &[QueuePlane<'_>]) -> io::Result<()> {
        self.check_index(index)?;
        if self.queued & (1 << index) != 0 {
            return Err(invalid(format!("buffer {index} is already queued")));
        }
        if planes.len() != self.format.num_planes() {
            return Err(invalid(format!(
                "{} planes for a {}-plane queue",
                planes.len(),
                self.format.num_planes()
            )));
        }
        let want_mmap = self.memory == Memory::Mmap;
        if planes
            .iter()
            .any(|p| matches!(p.memory, PlaneMemory::Mmap) != want_mmap)
        {
            return Err(invalid(format!(
                "plane memory does not match the queue's {:?} buffers",
                self.memory
            )));
        }
        Ok(())
    }

    fn is_queued(&self, index: u32) -> bool {
        self.queued & (1 << index) != 0
    }

    fn free(&self) -> Option<u32> {
        (0..self.count).find(|&i| !self.is_queued(i))
    }
}

/// The queue type a format takes: output for the source side, capture for the
/// destination, single- or multi-planar from the format.
fn queue_type(format: &Format, output: bool) -> BufType {
    match (format.is_multiplanar(), output) {
        (false, false) => BufType::VideoCapture,
        (false, true) => BufType::VideoOutput,
        (true, false) => BufType::VideoCaptureMplane,
        (true, true) => BufType::VideoOutputMplane,
    }
}

/// One memory-to-memory device with both queues open. Closing the session
/// closes the device; the kernel stops streaming and frees the buffers.
pub struct M2mSession {
    file: File,
    capabilities: Capability,
    output: Queue,
    capture: Queue,
}

impl M2mSession {
    /// Opens the memory-to-memory device at `path` and sets up both queues:
    /// the capture format first (encoders take the size of their coded buffers
    /// from the source format, which is set after it), then the output format,
    /// with the buffers asked for.
    ///
    /// Fails with [`io::ErrorKind::InvalidInput`] when the node is not a
    /// memory-to-memory device, or when a queue's setup is refused.
    pub fn open(
        path: impl AsRef<Path>,
        output: QueueSetup,
        capture: QueueSetup,
    ) -> io::Result<Self> {
        let file = sys::open_node(path.as_ref())?;
        let capabilities = sys::querycap(file.as_fd())?;
        let node = capabilities.node_caps();
        if node & (sys::CAP_VIDEO_M2M | sys::CAP_VIDEO_M2M_MPLANE) == 0 {
            return Err(invalid(format!(
                "{} is not a memory-to-memory device",
                path.as_ref().display()
            )));
        }
        let capture_queue = Queue::open(file.as_fd(), &capture, false)?;
        let output_queue = Queue::open(file.as_fd(), &output, true)?;
        Ok(Self {
            file,
            capabilities,
            output: output_queue,
            capture: capture_queue,
        })
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

    /// The source queue's crop rectangle as the driver kept it, when one was
    /// set at open ([`QueueSetup::crop`]) or by [`Self::set_output_crop`].
    pub fn output_crop(&self) -> Option<Rect> {
        self.output.crop
    }

    /// The destination queue's crop rectangle as the driver kept it, when one
    /// was set at open.
    pub fn capture_crop(&self) -> Option<Rect> {
        self.capture.crop
    }

    /// Sets the source queue's crop rectangle (`VIDIOC_S_SELECTION`) and returns
    /// the one the driver kept. Changing it between frames is how a scaler is
    /// asked for a new source window. The device refuses the change while the
    /// queue streams (`EBUSY` on most drivers).
    pub fn set_output_crop(&mut self, rect: Rect) -> io::Result<Rect> {
        let kept = sys::s_selection(
            self.file.as_fd(),
            self.output.buf_type,
            sys::SEL_TGT_CROP,
            rect,
        )?;
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

    /// Plane `plane` of source buffer `index`, for writing before it is queued.
    /// `None` for dma-buf queues, for a buffer the device holds, or for a bad index.
    pub fn output_plane_mut(&mut self, index: u32, plane: usize) -> Option<&mut [u8]> {
        if self.output.memory != Memory::Mmap || self.output.is_queued(index) {
            return None;
        }
        self.output
            .maps
            .get_mut(index as usize)?
            .get_mut(plane)
            .map(Mapping::as_mut_slice)
    }

    /// Plane `plane` of destination buffer `index`, for reading after it was
    /// dequeued. `None` for dma-buf queues, for a buffer the device holds, or
    /// for a bad index. The data is `bytes_used` long ([`Dequeued::bytes_used`]).
    pub fn capture_plane(&self, index: u32, plane: usize) -> Option<&[u8]> {
        if self.capture.memory != Memory::Mmap || self.capture.is_queued(index) {
            return None;
        }
        self.capture
            .maps
            .get(index as usize)?
            .get(plane)
            .map(Mapping::as_slice)
    }

    /// Queues source buffer `index` with its planes and the frame's timestamp.
    /// The device reads the frame and may keep the buffer until it is dequeued;
    /// for dma-bufs the caller may close its descriptors now (the device holds
    /// its own reference).
    pub fn queue_output(
        &mut self,
        index: u32,
        planes: &[QueuePlane<'_>],
        timestamp: Duration,
    ) -> io::Result<()> {
        self.output.check_queueable(index, planes)?;
        sys::qbuf(
            self.file.as_fd(),
            self.output.buf_type,
            self.output.memory,
            index,
            planes,
            timestamp,
        )?;
        self.output.queued |= 1 << index;
        Ok(())
    }

    /// Queues destination buffer `index` with its planes, for the device to
    /// fill. For MMAP the planes are [`PlaneMemory::Mmap`]; for dma-bufs the
    /// buffer is the caller's.
    pub fn queue_capture(&mut self, index: u32, planes: &[QueuePlane<'_>]) -> io::Result<()> {
        self.capture.check_queueable(index, planes)?;
        sys::qbuf(
            self.file.as_fd(),
            self.capture.buf_type,
            self.capture.memory,
            index,
            planes,
            Duration::ZERO,
        )?;
        self.capture.queued |= 1 << index;
        Ok(())
    }

    /// Takes back a source buffer the device has finished reading, or `None`
    /// when none is done. The buffer is the caller's again.
    pub fn dequeue_output(&mut self) -> io::Result<Option<Dequeued>> {
        dequeue(self.file.as_fd(), &mut self.output)
    }

    /// Takes back a destination buffer holding a frame, or `None` when none
    /// is ready. Read it with [`Self::capture_plane`], then queue it again.
    pub fn dequeue_capture(&mut self) -> io::Result<Option<Dequeued>> {
        dequeue(self.file.as_fd(), &mut self.capture)
    }

    /// Waits until one of the readiness flags in `want` holds or `timeout`
    /// passes (`None`: forever), and reports which of them hold. Returns no
    /// flags on timeout. Device errors and hang-ups are errors.
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
        let mut fds = [PollFd::new(self.file.as_fd(), events)];
        poll::poll_many(&mut fds, timeout)?;
        let revents = fds[0].revents();
        if revents & (poll::POLLERR | poll::POLLHUP | poll::POLLNVAL) != 0 {
            return Err(io::Error::other("the device reported an error or hung up"));
        }
        Ok(Ready {
            output: want.output && revents & poll::POLLOUT != 0,
            capture: want.capture && revents & poll::POLLIN != 0,
            event: want.event && revents & poll::POLLPRI != 0,
        })
    }

    /// Starts streaming both queues. The destination queue starts first, so
    /// the device has somewhere to put the first frame.
    pub fn stream_on(&mut self) -> io::Result<()> {
        sys::streamon(self.file.as_fd(), self.capture.buf_type)?;
        sys::streamon(self.file.as_fd(), self.output.buf_type)
    }

    /// Stops both queues. Every buffer the device held comes back to the
    /// caller (dequeue them, or let them be dropped with the session).
    pub fn stream_off(&mut self) -> io::Result<()> {
        sys::streamoff(self.file.as_fd(), self.output.buf_type)?;
        sys::streamoff(self.file.as_fd(), self.capture.buf_type)?;
        self.output.queued = 0;
        self.capture.queued = 0;
        Ok(())
    }

    /// Asks the device to finish the frames queued so far. The last capture
    /// buffer comes back flagged [`BUF_FLAG_LAST`](super::BUF_FLAG_LAST), and an
    /// [`EVENT_EOS`](super::EVENT_EOS) event follows it.
    pub fn drain(&self, which: Drain) -> io::Result<()> {
        match which {
            Drain::Encoder => sys::encoder_cmd(self.file.as_fd(), sys::ENC_CMD_STOP, 0),
            Drain::Decoder => sys::decoder_cmd(self.file.as_fd(), sys::DEC_CMD_STOP, 0),
        }
    }

    /// Subscribes to events of `event_type` (e.g. [`EVENT_EOS`](super::EVENT_EOS),
    /// [`EVENT_SOURCE_CHANGE`](super::EVENT_SOURCE_CHANGE)). Control events take
    /// the control's id in `id`; other events take 0. A driver may refuse an
    /// event type it does not produce (`EINVAL`).
    pub fn subscribe(&self, event_type: u32, id: u32) -> io::Result<()> {
        sys::subscribe_event(self.file.as_fd(), event_type, id)
    }

    /// Takes the next pending event, or `None` when there is none.
    pub fn dequeue_event(&self) -> io::Result<Option<Event>> {
        sys::dqevent(self.file.as_fd())
    }
}

impl AsFd for M2mSession {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

fn dequeue(fd: BorrowedFd<'_>, queue: &mut Queue) -> io::Result<Option<Dequeued>> {
    let done = sys::dqbuf(fd, queue.buf_type, queue.memory, queue.planes())?;
    if let Some(d) = &done {
        queue.queued &= !(1 << d.index);
    }
    Ok(done)
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests;
