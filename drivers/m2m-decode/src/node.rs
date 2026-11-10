//! The device calls a session makes, as a trait, and the Linux node that makes them.
//!
//! A session never calls the kernel itself: it asks a [`Node`]. On Linux that is a
//! [`LinuxNode`], one open video node and its media device, over `lemnos-linux-sys`; the tests
//! use a node that plays a device back. Every call is one ioctl (or `mmap`, or `poll`) and none
//! allocates, so a session's steady state allocates nothing on either.

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::poll;
use lemnos_linux_sys::v4l2::device::ExtControl;
use lemnos_linux_sys::v4l2::{
    self as sys, BufType, BufferInfo, Capability, Dequeued, Event, Format, Memory, PlaneInfo,
    QueuePlane, QueueRequest, Rect,
};

use crate::request::{Request, media_device_for};

/// One buffer to queue.
#[derive(Clone, Copy, Debug)]
pub struct Queued<'a, R> {
    /// The queue.
    pub buf_type: BufType,
    /// The queue's memory kind.
    pub memory: Memory,
    /// The buffer's index.
    pub index: u32,
    /// Its planes, one per memory plane of the queue's format.
    pub planes: &'a [QueuePlane<'a>],
    /// Goes with the buffer; a decoder copies an output buffer's to the frame it makes.
    pub timestamp: Duration,
    /// The request the buffer joins, for a stateless codec; `None` queues it directly.
    pub request: Option<&'a R>,
}

/// A mapped plane of an MMAP buffer.
pub trait PlaneMap {
    /// The plane's bytes.
    fn bytes(&self) -> &[u8];
    /// The plane's bytes, for writing a source frame.
    fn bytes_mut(&mut self) -> &mut [u8];
}

impl PlaneMap for Mapping {
    fn bytes(&self) -> &[u8] {
        self.as_slice()
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        self.as_mut_slice()
    }
}

/// One memory-to-memory device: the calls a session makes on it. Every number a node returns
/// comes from the kernel and is checked by the session before it indexes with it.
pub trait Node {
    /// A media request on the device's media node.
    type Request;
    /// A mapped plane of an MMAP buffer.
    type Map: PlaneMap;

    /// `VIDIOC_QUERYCAP`.
    fn capability(&self) -> io::Result<Capability>;
    /// `VIDIOC_S_FMT`: the format the driver took.
    fn set_format(&self, buf_type: BufType, format: &Format) -> io::Result<Format>;
    /// `VIDIOC_G_FMT`.
    fn format(&self, buf_type: BufType) -> io::Result<Format>;
    /// `VIDIOC_S_SELECTION`: the rectangle the driver kept.
    fn set_selection(&self, buf_type: BufType, target: u32, rect: Rect) -> io::Result<Rect>;
    /// `VIDIOC_G_SELECTION`.
    fn selection(&self, buf_type: BufType, target: u32) -> io::Result<Rect>;
    /// `VIDIOC_REQBUFS`: how many buffers were granted (0 frees them).
    fn request_buffers(&self, buf_type: BufType, memory: Memory, count: u32) -> io::Result<u32>;
    /// `VIDIOC_QUERYBUF` for an MMAP buffer with room for `planes` planes.
    fn query_buffer(&self, buf_type: BufType, index: u32, planes: u32) -> io::Result<BufferInfo>;
    /// Maps one plane `QUERYBUF` described, readable and writable.
    fn map(&self, plane: &PlaneInfo) -> io::Result<Self::Map>;
    /// `VIDIOC_QBUF`.
    fn queue(&self, buffer: &Queued<'_, Self::Request>) -> io::Result<()>;
    /// `VIDIOC_DQBUF`, non-blocking: `None` when nothing is ready, and after a drain's LAST
    /// buffer.
    fn dequeue(
        &self,
        buf_type: BufType,
        memory: Memory,
        planes: u32,
    ) -> io::Result<Option<Dequeued>>;
    /// `VIDIOC_STREAMON`.
    fn stream_on(&self, buf_type: BufType) -> io::Result<()>;
    /// `VIDIOC_STREAMOFF`.
    fn stream_off(&self, buf_type: BufType) -> io::Result<()>;
    /// `VIDIOC_ENCODER_CMD` with `ENC_CMD_STOP`.
    fn stop_encoder(&self) -> io::Result<()>;
    /// `VIDIOC_DECODER_CMD` with `DEC_CMD_STOP`.
    fn stop_decoder(&self) -> io::Result<()>;
    /// `VIDIOC_SUBSCRIBE_EVENT`.
    fn subscribe(&self, event_type: u32, id: u32) -> io::Result<()>;
    /// `VIDIOC_DQEVENT`, non-blocking: `None` when none is pending.
    fn event(&self) -> io::Result<Option<Event>>;
    /// `VIDIOC_S_CTRL`: the value the driver kept.
    fn set_control(&self, id: u32, value: i32) -> io::Result<i32>;
    /// `VIDIOC_G_CTRL`.
    fn control(&self, id: u32) -> io::Result<i32>;
    /// Waits for `events` (`poll`'s `POLLIN`, `POLLOUT`, `POLLPRI`) or `timeout`, and returns
    /// what was reported (0 on timeout).
    fn poll(&self, events: i16, timeout: Option<Duration>) -> io::Result<i16>;
    /// `MEDIA_IOC_REQUEST_ALLOC` on the device's media node.
    fn new_request(&self) -> io::Result<Self::Request>;
    /// `VIDIOC_S_EXT_CTRLS` with the request's values, over payloads the caller borrows.
    fn set_request_controls(
        &self,
        request: &Self::Request,
        controls: &mut [ExtControl<'_>],
    ) -> io::Result<()>;
    /// `MEDIA_REQUEST_IOC_QUEUE`.
    fn queue_request(&self, request: &Self::Request) -> io::Result<()>;
    /// `MEDIA_REQUEST_IOC_REINIT`.
    fn reinit_request(&self, request: &Self::Request) -> io::Result<()>;
    /// Waits until the request completes or `timeout` passes; whether it completed.
    fn wait_request(&self, request: &Self::Request, timeout: Option<Duration>) -> io::Result<bool>;
}

/// A video node of a memory-to-memory device, and its media device when it has one (a
/// stateless codec has; requests need it).
#[derive(Debug)]
pub struct LinuxNode {
    video: File,
    media: Option<File>,
}

impl LinuxNode {
    /// Opens the video node at `path` read-write and non-blocking, and the media device that
    /// lists it, if any ([`media_device_for`]).
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let video = sys::open_node(path)?;
        let media = match media_device_for(path)? {
            Some(media) => Some(File::open(media)?),
            None => None,
        };
        Ok(Self { video, media })
    }

    /// A node over an open video descriptor and, for requests, its media device. The video
    /// descriptor is made non-blocking. V4L2 memory-to-memory state is per file handle: a
    /// control set on the descriptor before this applies to the session built on it.
    pub fn from_fds(video: impl Into<OwnedFd>, media: Option<OwnedFd>) -> io::Result<Self> {
        let video = File::from(video.into());
        sys::set_nonblocking(video.as_fd())?;
        Ok(Self {
            video,
            media: media.map(File::from),
        })
    }

    /// The first `/dev/videoN` (in numeric order) that is a memory-to-memory node of the
    /// driver named `driver` (`QUERYCAP`'s driver name), or `None`. Each node is opened
    /// read-only to ask; nodes that cannot be opened or asked are passed over.
    pub fn find(driver: &str) -> io::Result<Option<PathBuf>> {
        let mut nodes: Vec<(u32, PathBuf)> = std::fs::read_dir("/dev")?
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name();
                let index = name.to_str()?.strip_prefix("video")?.parse::<u32>().ok()?;
                Some((index, e.path()))
            })
            .collect();
        nodes.sort_by_key(|(index, _)| *index);
        for (_, path) in nodes {
            let Ok(file) = poll::open_nonblocking(&path) else {
                continue;
            };
            let Ok(capability) = sys::querycap(file.as_fd()) else {
                continue;
            };
            if capability.is_m2m() && capability.driver == driver {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    fn media(&self) -> io::Result<BorrowedFd<'_>> {
        self.media.as_ref().map(AsFd::as_fd).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "the node has no media device, so no requests",
            )
        })
    }
}

impl AsFd for LinuxNode {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.video.as_fd()
    }
}

impl Node for LinuxNode {
    type Request = Request;
    type Map = Mapping;

    fn capability(&self) -> io::Result<Capability> {
        sys::querycap(self.video.as_fd())
    }

    fn set_format(&self, buf_type: BufType, format: &Format) -> io::Result<Format> {
        sys::s_fmt(self.video.as_fd(), buf_type, format)
    }

    fn format(&self, buf_type: BufType) -> io::Result<Format> {
        sys::g_fmt(self.video.as_fd(), buf_type)
    }

    fn set_selection(&self, buf_type: BufType, target: u32, rect: Rect) -> io::Result<Rect> {
        sys::s_selection(self.video.as_fd(), buf_type, target, rect)
    }

    fn selection(&self, buf_type: BufType, target: u32) -> io::Result<Rect> {
        sys::g_selection(self.video.as_fd(), buf_type, target)
    }

    fn request_buffers(&self, buf_type: BufType, memory: Memory, count: u32) -> io::Result<u32> {
        Ok(sys::reqbufs(self.video.as_fd(), buf_type, memory, count)?.count)
    }

    fn query_buffer(&self, buf_type: BufType, index: u32, planes: u32) -> io::Result<BufferInfo> {
        sys::querybuf(self.video.as_fd(), buf_type, index, planes)
    }

    fn map(&self, plane: &PlaneInfo) -> io::Result<Mapping> {
        let len = usize::try_from(plane.length)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        Mapping::new(self.video.as_fd(), len, u64::from(plane.offset), true)
    }

    fn queue(&self, buffer: &Queued<'_, Request>) -> io::Result<()> {
        sys::qbuf_buffer(
            self.video.as_fd(),
            &QueueRequest {
                buf_type: buffer.buf_type,
                memory: buffer.memory,
                index: buffer.index,
                planes: buffer.planes,
                timestamp: buffer.timestamp,
                field: 0,
                request: buffer.request.map(Request::as_fd),
            },
        )
    }

    fn dequeue(
        &self,
        buf_type: BufType,
        memory: Memory,
        planes: u32,
    ) -> io::Result<Option<Dequeued>> {
        sys::dqbuf(self.video.as_fd(), buf_type, memory, planes)
    }

    fn stream_on(&self, buf_type: BufType) -> io::Result<()> {
        sys::streamon(self.video.as_fd(), buf_type)
    }

    fn stream_off(&self, buf_type: BufType) -> io::Result<()> {
        sys::streamoff(self.video.as_fd(), buf_type)
    }

    fn stop_encoder(&self) -> io::Result<()> {
        sys::encoder_cmd(self.video.as_fd(), sys::ENC_CMD_STOP, 0)
    }

    fn stop_decoder(&self) -> io::Result<()> {
        sys::decoder_cmd(self.video.as_fd(), sys::DEC_CMD_STOP, 0)
    }

    fn subscribe(&self, event_type: u32, id: u32) -> io::Result<()> {
        sys::subscribe_event(self.video.as_fd(), event_type, id)
    }

    fn event(&self) -> io::Result<Option<Event>> {
        sys::dqevent(self.video.as_fd())
    }

    fn set_control(&self, id: u32, value: i32) -> io::Result<i32> {
        sys::s_ctrl(self.video.as_fd(), id, value)
    }

    fn control(&self, id: u32) -> io::Result<i32> {
        sys::g_ctrl(self.video.as_fd(), id)
    }

    fn poll(&self, events: i16, timeout: Option<Duration>) -> io::Result<i16> {
        poll::poll_one(self.video.as_fd(), events, timeout)
    }

    fn new_request(&self) -> io::Result<Request> {
        Request::allocate(self.media()?)
    }

    fn set_request_controls(
        &self,
        request: &Request,
        controls: &mut [ExtControl<'_>],
    ) -> io::Result<()> {
        sys::s_ext_ctrls_in_place(self.video.as_fd(), Some(request.as_fd()), controls)
    }

    fn queue_request(&self, request: &Request) -> io::Result<()> {
        request.queue()
    }

    fn reinit_request(&self, request: &Request) -> io::Result<()> {
        request.reinit()
    }

    fn wait_request(&self, request: &Request, timeout: Option<Duration>) -> io::Result<bool> {
        request.wait(timeout)
    }
}
