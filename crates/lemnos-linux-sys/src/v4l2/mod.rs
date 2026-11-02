//! V4L2 (`/dev/videoN`): capabilities, formats, buffer queues (MMAP and DMA-buf
//! memory), streaming, encoder and decoder commands, and events. Generic
//! device access only: the memory-to-memory session that uses these calls is
//! in `lemnos-linux`.
//!
//! Single-planar and multi-planar queues share one set of functions; the
//! [`BufType`] says which layout a call uses.

use crate::ioctl::ioctl_ptr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

mod uapi;

use uapi::Pod;

/// The most planes one buffer has (`VIDEO_MAX_PLANES`).
pub const MAX_PLANES: usize = 8;

/// `V4L2_CAP_VIDEO_M2M_MPLANE`: a memory-to-memory device with multi-planar queues.
pub const CAP_VIDEO_M2M_MPLANE: u32 = 0x0000_4000;
/// `V4L2_CAP_VIDEO_M2M`: a memory-to-memory device with single-planar queues.
pub const CAP_VIDEO_M2M: u32 = 0x0000_8000;
/// `V4L2_CAP_STREAMING`: the device streams buffers.
pub const CAP_STREAMING: u32 = 0x0400_0000;
/// `V4L2_CAP_DEVICE_CAPS`: `device_caps` holds the caps of this node.
pub const CAP_DEVICE_CAPS: u32 = 0x8000_0000;
/// `V4L2_BUF_CAP_SUPPORTS_DMABUF`: the queue accepts DMA-buf memory.
pub const BUF_CAP_SUPPORTS_DMABUF: u32 = 0x0000_0004;

/// `V4L2_SEL_TGT_CROP`: the crop rectangle of a queue (the part of the source
/// the device reads, or of a capture the device fills).
pub const SEL_TGT_CROP: u32 = 0x0000;

/// `V4L2_BUF_FLAG_KEYFRAME`.
pub const BUF_FLAG_KEYFRAME: u32 = 0x0000_0008;
/// `V4L2_BUF_FLAG_PFRAME`.
pub const BUF_FLAG_PFRAME: u32 = 0x0000_0010;
/// `V4L2_BUF_FLAG_BFRAME`.
pub const BUF_FLAG_BFRAME: u32 = 0x0000_0020;
/// `V4L2_BUF_FLAG_ERROR`.
pub const BUF_FLAG_ERROR: u32 = 0x0000_0040;
/// `V4L2_BUF_FLAG_LAST`: the last buffer of a drained stream.
pub const BUF_FLAG_LAST: u32 = 0x0010_0000;

/// `V4L2_EVENT_EOS`: the stream ended (after a drain).
pub const EVENT_EOS: u32 = 2;
/// `V4L2_EVENT_CTRL`.
pub const EVENT_CTRL: u32 = 3;
/// `V4L2_EVENT_SOURCE_CHANGE`: the stream's format changed.
pub const EVENT_SOURCE_CHANGE: u32 = 5;

/// `V4L2_ENC_CMD_STOP`: finish encoding what is queued.
pub const ENC_CMD_STOP: u32 = 1;
/// `V4L2_DEC_CMD_STOP`: finish decoding what is queued.
pub const DEC_CMD_STOP: u32 = 1;

/// A four-character pixel format code (`V4L2_PIX_FMT_*`), e.g. `fourcc(b"NV12")`.
pub const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

/// The kind of a buffer queue (`enum v4l2_buf_type`), the queues this module drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum BufType {
    /// Single-planar capture (the destination of a memory-to-memory device).
    VideoCapture = 1,
    /// Single-planar output (the source of a memory-to-memory device).
    VideoOutput = 2,
    /// Multi-planar capture.
    VideoCaptureMplane = 9,
    /// Multi-planar output.
    VideoOutputMplane = 10,
}

impl BufType {
    /// Whether the queue is multi-planar.
    pub const fn is_multiplanar(self) -> bool {
        matches!(self, Self::VideoCaptureMplane | Self::VideoOutputMplane)
    }

    /// Whether the queue is an output (source) queue.
    pub const fn is_output(self) -> bool {
        matches!(self, Self::VideoOutput | Self::VideoOutputMplane)
    }
}

/// Where a queue's buffers live (`enum v4l2_memory`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Memory {
    /// Buffers the driver allocates and the caller maps (`mmap`).
    Mmap = 1,
    /// Buffers the caller passes as dma-bufs (`V4L2_MEMORY_DMABUF`).
    Dmabuf = 4,
}

/// The capabilities of a device node (`struct v4l2_capability`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capability {
    /// The driver's name.
    pub driver: String,
    /// The device's name.
    pub card: String,
    /// The bus the device is on.
    pub bus_info: String,
    /// The driver's version, `major << 16 | minor << 8 | patch`.
    pub version: u32,
    /// The capabilities of the whole device.
    pub capabilities: u32,
    /// The capabilities of this node, when `CAP_DEVICE_CAPS` says they differ.
    pub device_caps: u32,
}

impl Capability {
    /// The capabilities of this node: `device_caps` when the driver reports
    /// them, else `capabilities`.
    pub fn node_caps(&self) -> u32 {
        if self.capabilities & CAP_DEVICE_CAPS != 0 {
            self.device_caps
        } else {
            self.capabilities
        }
    }

    /// Whether this node is a memory-to-memory device (either queue layout).
    pub fn is_m2m(&self) -> bool {
        self.node_caps() & (CAP_VIDEO_M2M | CAP_VIDEO_M2M_MPLANE) != 0
    }

    /// Whether this node is a memory-to-memory device with multi-planar queues.
    /// `false` for a single-planar device and for a node that is not M2M.
    pub fn is_m2m_multiplanar(&self) -> bool {
        self.is_m2m() && self.node_caps() & CAP_VIDEO_M2M_MPLANE != 0
    }
}

/// A rectangle in pixels (`struct v4l2_rect`): an offset and a size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    /// `left`: the x offset.
    pub left: i32,
    /// `top`: the y offset.
    pub top: i32,
    /// `width`.
    pub width: u32,
    /// `height`.
    pub height: u32,
}

/// Single-planar pixel format (`struct v4l2_pix_format`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PixFormat {
    /// Picture width in pixels.
    pub width: u32,
    /// Picture height in lines.
    pub height: u32,
    /// The pixel format, a [`fourcc`].
    pub pixelformat: u32,
    /// Field order (`V4L2_FIELD_*`; 0 lets the driver choose).
    pub field: u32,
    /// Bytes from one row to the next.
    pub bytes_per_line: u32,
    /// Bytes one frame occupies.
    pub size_image: u32,
    /// Colour space (`V4L2_COLORSPACE_*`).
    pub colorspace: u32,
}

/// One memory plane of a multi-planar format (`struct v4l2_plane_pix_format`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaneFormat {
    /// Bytes one frame occupies in this plane.
    pub size_image: u32,
    /// Bytes from one row to the next in this plane.
    pub bytes_per_line: u32,
}

/// Multi-planar pixel format (`struct v4l2_pix_format_mplane`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PixFormatMplane {
    /// Picture width in pixels.
    pub width: u32,
    /// Picture height in lines.
    pub height: u32,
    /// The pixel format, a [`fourcc`].
    pub pixelformat: u32,
    /// Field order (`V4L2_FIELD_*`).
    pub field: u32,
    /// Colour space (`V4L2_COLORSPACE_*`).
    pub colorspace: u32,
    /// The memory planes; the first `num_planes` are in use.
    pub planes: [PlaneFormat; MAX_PLANES],
    /// How many memory planes the format has (1 to [`MAX_PLANES`]).
    pub num_planes: u32,
}

/// A queue's format: single-planar or multi-planar. The queue's [`BufType`]
/// follows from the variant (with its direction).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Single-planar.
    Single(PixFormat),
    /// Multi-planar.
    Multi(PixFormatMplane),
}

impl Format {
    /// The number of memory planes buffers of this format have.
    pub fn num_planes(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Multi(m) => m.num_planes as usize,
        }
    }

    /// Whether the format is multi-planar.
    pub fn is_multiplanar(&self) -> bool {
        matches!(self, Self::Multi(_))
    }
}

/// A pixel format a device lists (`struct v4l2_fmtdesc`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatDesc {
    /// The format's [`fourcc`].
    pub pixelformat: u32,
    /// `V4L2_FMT_FLAG_*`.
    pub flags: u32,
    /// The format's name, e.g. `"NV12"`.
    pub description: String,
}

/// What a queue's `REQBUFS` granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requested {
    /// Buffers granted (may be fewer or more than asked).
    pub count: u32,
    /// `V4L2_BUF_CAP_*`: what the queue supports.
    pub capabilities: u32,
}

/// One memory plane of an MMAP buffer, as `QUERYBUF` reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaneInfo {
    /// The plane's size in bytes.
    pub length: u32,
    /// The offset to pass to `mmap` on the device.
    pub offset: u32,
}

/// An MMAP buffer's planes (`QUERYBUF`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferInfo {
    /// The planes; the first `num_planes` are in use.
    pub planes: [PlaneInfo; MAX_PLANES],
    /// How many planes the buffer has.
    pub num_planes: u32,
}

/// Where a plane of a buffer being queued lives.
#[derive(Clone, Copy, Debug)]
pub enum PlaneMemory<'a> {
    /// The buffer's MMAP memory (the driver's, by index).
    Mmap,
    /// A dma-buf the caller holds. The device keeps its own reference from
    /// the queue call, so the caller may close the descriptor afterwards.
    Dmabuf(BorrowedFd<'a>),
}

/// One plane of a buffer to queue.
#[derive(Clone, Copy, Debug)]
pub struct QueuePlane<'a> {
    /// Where the plane's memory is.
    pub memory: PlaneMemory<'a>,
    /// The plane's size in bytes: the dma-buf's size for [`PlaneMemory::Dmabuf`],
    /// ignored for MMAP (the driver knows it).
    pub length: u32,
    /// Bytes of data in the plane (the device sets it on capture buffers).
    pub bytes_used: u32,
    /// Where the data starts in the plane. Only multi-planar queues take an offset.
    pub data_offset: u32,
}

/// A buffer the device returned (`DQBUF`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dequeued {
    /// The buffer's index in its queue.
    pub index: u32,
    /// `V4L2_BUF_FLAG_*` (`LAST`, `KEYFRAME`, `ERROR`, ...).
    pub flags: u32,
    /// The timestamp the buffer was queued with (a capture buffer carries its
    /// source frame's).
    pub timestamp: Duration,
    /// The frame sequence number.
    pub sequence: u32,
    /// Bytes of data in each plane; the first `num_planes` are in use.
    pub bytes_used: [u32; MAX_PLANES],
    /// How many planes the buffer has.
    pub num_planes: u32,
}

/// A pending event (`struct v4l2_event`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    /// The event's type (`EVENT_*`).
    pub event_type: u32,
    /// The event's id (for a control, its control id).
    pub id: u32,
    /// Events of this type still queued after this one.
    pub pending: u32,
    /// The event's sequence number.
    pub sequence: u32,
    /// When the event happened, on the monotonic clock.
    pub timestamp: Duration,
}

fn checked_planes(num_planes: u32) -> io::Result<usize> {
    match usize::try_from(num_planes) {
        Ok(n) if (1..=MAX_PLANES).contains(&n) => Ok(n),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{num_planes} planes (1 to {MAX_PLANES} allowed)"),
        )),
    }
}

fn timeval(time: Duration) -> libc::timeval {
    libc::timeval {
        tv_sec: libc::time_t::try_from(time.as_secs()).unwrap_or(libc::time_t::MAX),
        tv_usec: time.subsec_micros() as libc::suseconds_t,
    }
}

/// A time from its seconds and sub-second count (microseconds or nanoseconds,
/// per `subsec_scale`), clamped to zero when negative.
fn duration_of(sec: impl Into<i64>, subsec: impl Into<i64>, subsec_scale: u32) -> Duration {
    let secs = u64::try_from(sec.into()).unwrap_or(0);
    let units = u64::try_from(subsec.into()).unwrap_or(0);
    Duration::from_secs(secs) + Duration::from_nanos(units * u64::from(subsec_scale))
}

fn text(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Opens a video node read-write, non-blocking and close-on-exec. Dequeues
/// then return at once when nothing is ready ([`dqbuf`] gives `None`), so a
/// caller waits with `poll` first.
pub fn open_node(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

/// Queries the node's capabilities (`VIDIOC_QUERYCAP`).
pub fn querycap(fd: BorrowedFd<'_>) -> io::Result<Capability> {
    let mut raw = uapi::Capability::zeroed();
    // SAFETY: `QUERYCAP` takes a `v4l2_capability`, which `raw` is.
    unsafe { ioctl_ptr(fd, uapi::QUERYCAP, &raw mut raw)? };
    Ok(Capability {
        driver: text(&raw.driver),
        card: text(&raw.card),
        bus_info: text(&raw.bus_info),
        version: raw.version,
        capabilities: raw.capabilities,
        device_caps: raw.device_caps,
    })
}

/// Lists the formats a queue offers (`VIDIOC_ENUM_FMT`) at `index`, or `None`
/// past the last one.
pub fn enum_fmt(
    fd: BorrowedFd<'_>,
    buf_type: BufType,
    index: u32,
) -> io::Result<Option<FormatDesc>> {
    let mut raw = uapi::FmtDesc::zeroed();
    raw.index = index;
    raw.buf_type = buf_type as u32;
    // SAFETY: `ENUM_FMT` takes a `v4l2_fmtdesc`, which `raw` is.
    match unsafe { ioctl_ptr(fd, uapi::ENUM_FMT, &raw mut raw) } {
        Ok(_) => Ok(Some(FormatDesc {
            pixelformat: raw.pixelformat,
            flags: raw.flags,
            description: text(&raw.description),
        })),
        Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(None),
        Err(e) => Err(e),
    }
}

fn decode(buf_type: BufType, raw: &uapi::Format) -> Format {
    let b = &raw.fmt.0;
    if buf_type.is_multiplanar() {
        let mut planes = [PlaneFormat::default(); MAX_PLANES];
        for (i, plane) in planes.iter_mut().enumerate() {
            let at = 20 + 20 * i;
            *plane = PlaneFormat {
                size_image: uapi::get_u32(b, at),
                bytes_per_line: uapi::get_u32(b, at + 4),
            };
        }
        Format::Multi(PixFormatMplane {
            width: uapi::get_u32(b, 0),
            height: uapi::get_u32(b, 4),
            pixelformat: uapi::get_u32(b, 8),
            field: uapi::get_u32(b, 12),
            colorspace: uapi::get_u32(b, 16),
            planes,
            num_planes: u32::from(b[180]),
        })
    } else {
        Format::Single(PixFormat {
            width: uapi::get_u32(b, 0),
            height: uapi::get_u32(b, 4),
            pixelformat: uapi::get_u32(b, 8),
            field: uapi::get_u32(b, 12),
            bytes_per_line: uapi::get_u32(b, 16),
            size_image: uapi::get_u32(b, 20),
            colorspace: uapi::get_u32(b, 24),
        })
    }
}

fn encode(buf_type: BufType, format: &Format) -> io::Result<uapi::Format> {
    let mut raw = uapi::Format::zeroed();
    raw.buf_type = buf_type as u32;
    let b = &mut raw.fmt.0;
    match (format, buf_type.is_multiplanar()) {
        (Format::Single(p), false) => {
            uapi::put_u32(b, 0, p.width);
            uapi::put_u32(b, 4, p.height);
            uapi::put_u32(b, 8, p.pixelformat);
            uapi::put_u32(b, 12, p.field);
            uapi::put_u32(b, 16, p.bytes_per_line);
            uapi::put_u32(b, 20, p.size_image);
            uapi::put_u32(b, 24, p.colorspace);
        }
        (Format::Multi(m), true) => {
            checked_planes(m.num_planes)?;
            uapi::put_u32(b, 0, m.width);
            uapi::put_u32(b, 4, m.height);
            uapi::put_u32(b, 8, m.pixelformat);
            uapi::put_u32(b, 12, m.field);
            uapi::put_u32(b, 16, m.colorspace);
            for (i, p) in m.planes.iter().enumerate() {
                let at = 20 + 20 * i;
                uapi::put_u32(b, at, p.size_image);
                uapi::put_u32(b, at + 4, p.bytes_per_line);
            }
            b[180] = m.num_planes as u8;
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "format layout does not match the queue type",
            ));
        }
    }
    Ok(raw)
}

/// Reads a queue's current format (`VIDIOC_G_FMT`).
pub fn g_fmt(fd: BorrowedFd<'_>, buf_type: BufType) -> io::Result<Format> {
    let mut raw = uapi::Format::zeroed();
    raw.buf_type = buf_type as u32;
    // SAFETY: `G_FMT` takes a `v4l2_format`, which `raw` is.
    unsafe { ioctl_ptr(fd, uapi::G_FMT, &raw mut raw)? };
    Ok(decode(buf_type, &raw))
}

/// Sets a queue's format (`VIDIOC_S_FMT`) and returns the one the driver
/// took, which may differ (sizes and strides are rounded). `format` must match
/// `buf_type`'s layout.
pub fn s_fmt(fd: BorrowedFd<'_>, buf_type: BufType, format: &Format) -> io::Result<Format> {
    let mut raw = encode(buf_type, format)?;
    // SAFETY: `S_FMT` takes a `v4l2_format`, which `raw` is.
    unsafe { ioctl_ptr(fd, uapi::S_FMT, &raw mut raw)? };
    Ok(decode(buf_type, &raw))
}

/// Reads a queue's selection rectangle for `target` (`VIDIOC_G_SELECTION`),
/// e.g. [`SEL_TGT_CROP`].
pub fn g_selection(fd: BorrowedFd<'_>, buf_type: BufType, target: u32) -> io::Result<Rect> {
    let mut raw = uapi::Selection::zeroed();
    raw.buf_type = buf_type as u32;
    raw.target = target;
    // SAFETY: `G_SELECTION` takes a `v4l2_selection`, which `raw` is.
    unsafe { ioctl_ptr(fd, uapi::G_SELECTION, &raw mut raw)? };
    Ok(rect_of(&raw))
}

/// Sets a queue's selection rectangle for `target` (`VIDIOC_S_SELECTION`) and
/// returns the one the driver kept: it may clamp or align `rect` to what the
/// hardware can do. The device refuses the call while the queue streams.
pub fn s_selection(
    fd: BorrowedFd<'_>,
    buf_type: BufType,
    target: u32,
    rect: Rect,
) -> io::Result<Rect> {
    let mut raw = uapi::Selection::zeroed();
    raw.buf_type = buf_type as u32;
    raw.target = target;
    raw.left = rect.left;
    raw.top = rect.top;
    raw.width = rect.width;
    raw.height = rect.height;
    // SAFETY: `S_SELECTION` takes a `v4l2_selection`, which `raw` is.
    unsafe { ioctl_ptr(fd, uapi::S_SELECTION, &raw mut raw)? };
    Ok(rect_of(&raw))
}

fn rect_of(raw: &uapi::Selection) -> Rect {
    Rect {
        left: raw.left,
        top: raw.top,
        width: raw.width,
        height: raw.height,
    }
}

/// Asks for `count` buffers of `memory` on a queue (`VIDIOC_REQBUFS`). Asking
/// for 0 frees the queue's buffers.
pub fn reqbufs(
    fd: BorrowedFd<'_>,
    buf_type: BufType,
    memory: Memory,
    count: u32,
) -> io::Result<Requested> {
    let mut raw = uapi::RequestBuffers::zeroed();
    raw.count = count;
    raw.buf_type = buf_type as u32;
    raw.memory = memory as u32;
    // SAFETY: `REQBUFS` takes a `v4l2_requestbuffers`, which `raw` is.
    unsafe { ioctl_ptr(fd, uapi::REQBUFS, &raw mut raw)? };
    Ok(Requested {
        count: raw.count,
        capabilities: raw.capabilities,
    })
}

/// Reads the planes of an MMAP buffer (`VIDIOC_QUERYBUF`): each plane's size
/// and the offset to map it at.
pub fn querybuf(
    fd: BorrowedFd<'_>,
    buf_type: BufType,
    index: u32,
    num_planes: u32,
) -> io::Result<BufferInfo> {
    let mut raw = uapi::Buffer::zeroed();
    raw.index = index;
    raw.buf_type = buf_type as u32;
    raw.memory = Memory::Mmap as u32;
    let mut info = BufferInfo {
        planes: [PlaneInfo::default(); MAX_PLANES],
        num_planes: 1,
    };
    if buf_type.is_multiplanar() {
        let n = checked_planes(num_planes)?;
        let mut planes = [uapi::Plane::zeroed(); MAX_PLANES];
        raw.length = n as u32;
        raw.m = uapi::Memory {
            planes: planes.as_mut_ptr(),
        };
        // SAFETY: `QUERYBUF` writes `length` planes into the array `raw.m`
        // points at, which lives until the call returns.
        unsafe { ioctl_ptr(fd, uapi::QUERYBUF, &raw mut raw)? };
        info.num_planes = n as u32;
        for (dst, src) in info.planes.iter_mut().zip(&planes).take(n) {
            *dst = PlaneInfo {
                length: src.length,
                // SAFETY: the kernel filled the plane's memory member for an
                // MMAP buffer, which is the offset.
                offset: unsafe { src.m.offset },
            };
        }
    } else {
        // SAFETY: `QUERYBUF` writes a `v4l2_buffer`, which `raw` is.
        unsafe { ioctl_ptr(fd, uapi::QUERYBUF, &raw mut raw)? };
        info.planes[0] = PlaneInfo {
            length: raw.length,
            // SAFETY: the kernel filled the memory member for an MMAP buffer,
            // which is the offset.
            offset: unsafe { raw.m.offset },
        };
    }
    Ok(info)
}

/// Exports one plane of an MMAP buffer as a dma-buf (`VIDIOC_EXPBUF`), so
/// another device can use the buffer's memory. Close-on-exec, read-write.
pub fn expbuf(
    fd: BorrowedFd<'_>,
    buf_type: BufType,
    index: u32,
    plane: u32,
) -> io::Result<OwnedFd> {
    let mut raw = uapi::ExportBuffer::zeroed();
    raw.buf_type = buf_type as u32;
    raw.index = index;
    raw.plane = plane;
    raw.flags = (libc::O_CLOEXEC | libc::O_RDWR) as u32;
    // SAFETY: `EXPBUF` takes a `v4l2_exportbuffer`, which `raw` is.
    unsafe { ioctl_ptr(fd, uapi::EXPBUF, &raw mut raw)? };
    // SAFETY: a successful export returns a new dma-buf descriptor, which no
    // other owner has.
    Ok(unsafe { OwnedFd::from_raw_fd(raw.fd as RawFd) })
}

/// Queues buffer `index` with its planes (`VIDIOC_QBUF`). `timestamp` goes with
/// it (an output buffer's is copied to the capture buffer it produces). The
/// device may keep the buffer until it is dequeued.
pub fn qbuf(
    fd: BorrowedFd<'_>,
    buf_type: BufType,
    memory: Memory,
    index: u32,
    planes: &[QueuePlane<'_>],
    timestamp: Duration,
) -> io::Result<()> {
    let mut raw = uapi::Buffer::zeroed();
    raw.index = index;
    raw.buf_type = buf_type as u32;
    raw.memory = memory as u32;
    raw.timestamp = timeval(timestamp);
    if buf_type.is_multiplanar() {
        checked_planes(planes.len() as u32)?;
        let mut kernel_planes = [uapi::Plane::zeroed(); MAX_PLANES];
        for (k, p) in kernel_planes.iter_mut().zip(planes) {
            k.length = p.length;
            k.bytesused = p.bytes_used;
            k.data_offset = p.data_offset;
            k.m = match p.memory {
                PlaneMemory::Mmap => uapi::Memory { offset: 0 },
                PlaneMemory::Dmabuf(fd) => uapi::Memory { fd: fd.as_raw_fd() },
            };
        }
        raw.length = planes.len() as u32;
        raw.m = uapi::Memory {
            planes: kernel_planes.as_mut_ptr(),
        };
        // SAFETY: `QBUF` reads `length` planes from the array `raw.m` points at,
        // which lives until the call returns.
        unsafe { ioctl_ptr(fd, uapi::QBUF, &raw mut raw)? };
    } else {
        let [p] = planes else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a single-planar buffer has one plane",
            ));
        };
        if p.data_offset != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a single-planar buffer has no data offset",
            ));
        }
        raw.length = p.length;
        raw.bytesused = p.bytes_used;
        raw.m = match p.memory {
            PlaneMemory::Mmap => uapi::Memory { offset: 0 },
            PlaneMemory::Dmabuf(fd) => uapi::Memory { fd: fd.as_raw_fd() },
        };
        // SAFETY: `QBUF` takes a `v4l2_buffer`, which `raw` is.
        unsafe { ioctl_ptr(fd, uapi::QBUF, &raw mut raw)? };
    }
    Ok(())
}

/// Takes back a buffer the device is done with (`VIDIOC_DQBUF`), or `None`
/// when none is ready (the descriptor is non-blocking). `num_planes` is the
/// queue's plane count.
pub fn dqbuf(
    fd: BorrowedFd<'_>,
    buf_type: BufType,
    memory: Memory,
    num_planes: u32,
) -> io::Result<Option<Dequeued>> {
    let mut raw = uapi::Buffer::zeroed();
    raw.buf_type = buf_type as u32;
    raw.memory = memory as u32;
    let mut kernel_planes = [uapi::Plane::zeroed(); MAX_PLANES];
    let n = if buf_type.is_multiplanar() {
        let n = checked_planes(num_planes)?;
        raw.length = n as u32;
        raw.m = uapi::Memory {
            planes: kernel_planes.as_mut_ptr(),
        };
        n
    } else {
        1
    };
    // SAFETY: `DQBUF` writes a `v4l2_buffer`, and for a multi-planar queue
    // `length` planes into the array `raw.m` points at; both live for the call.
    match unsafe { ioctl_ptr(fd, uapi::DQBUF, &raw mut raw) } {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut bytes_used = [0u32; MAX_PLANES];
    if buf_type.is_multiplanar() {
        for (dst, src) in bytes_used.iter_mut().zip(&kernel_planes).take(n) {
            *dst = src.bytesused;
        }
    } else {
        bytes_used[0] = raw.bytesused;
    }
    Ok(Some(Dequeued {
        index: raw.index,
        flags: raw.flags,
        timestamp: duration_of(raw.timestamp.tv_sec, raw.timestamp.tv_usec, 1_000),
        sequence: raw.sequence,
        bytes_used,
        num_planes: n as u32,
    }))
}

/// Starts streaming a queue (`VIDIOC_STREAMON`).
pub fn streamon(fd: BorrowedFd<'_>, buf_type: BufType) -> io::Result<()> {
    let mut t = buf_type as i32;
    // SAFETY: `STREAMON` takes a pointer to the queue type as an `int`.
    unsafe { ioctl_ptr(fd, uapi::STREAMON, &raw mut t)? };
    Ok(())
}

/// Stops streaming a queue (`VIDIOC_STREAMOFF`); the device gives every queued
/// buffer back.
pub fn streamoff(fd: BorrowedFd<'_>, buf_type: BufType) -> io::Result<()> {
    let mut t = buf_type as i32;
    // SAFETY: `STREAMOFF` takes a pointer to the queue type as an `int`.
    unsafe { ioctl_ptr(fd, uapi::STREAMOFF, &raw mut t)? };
    Ok(())
}

/// Sends an encoder command (`VIDIOC_ENCODER_CMD`), e.g. [`ENC_CMD_STOP`].
pub fn encoder_cmd(fd: BorrowedFd<'_>, cmd: u32, flags: u32) -> io::Result<()> {
    let mut raw = uapi::EncoderCmd::zeroed();
    raw.cmd = cmd;
    raw.flags = flags;
    // SAFETY: `ENCODER_CMD` takes a `v4l2_encoder_cmd`, which `raw` is.
    unsafe { ioctl_ptr(fd, uapi::ENCODER_CMD, &raw mut raw)? };
    Ok(())
}

/// Sends a decoder command (`VIDIOC_DECODER_CMD`), e.g. [`DEC_CMD_STOP`].
pub fn decoder_cmd(fd: BorrowedFd<'_>, cmd: u32, flags: u32) -> io::Result<()> {
    let mut raw = uapi::DecoderCmd::zeroed();
    raw.cmd = cmd;
    raw.flags = flags;
    // SAFETY: `DECODER_CMD` takes a `v4l2_decoder_cmd`, which `raw` is.
    unsafe { ioctl_ptr(fd, uapi::DECODER_CMD, &raw mut raw)? };
    Ok(())
}

/// Subscribes to events of `event_type` (`VIDIOC_SUBSCRIBE_EVENT`), e.g.
/// [`EVENT_EOS`]. Control events take the control's id in `id` (0 for all of
/// them); other events take 0.
pub fn subscribe_event(fd: BorrowedFd<'_>, event_type: u32, id: u32) -> io::Result<()> {
    let mut raw = uapi::Subscription::zeroed();
    raw.event_type = event_type;
    raw.id = id;
    // SAFETY: `SUBSCRIBE_EVENT` takes a `v4l2_event_subscription`, which `raw` is.
    unsafe { ioctl_ptr(fd, uapi::SUBSCRIBE_EVENT, &raw mut raw)? };
    Ok(())
}

/// Takes the next pending event (`VIDIOC_DQEVENT`), or `None` when there is
/// none: the kernel answers ENOENT for an empty queue of a non-blocking
/// descriptor.
pub fn dqevent(fd: BorrowedFd<'_>) -> io::Result<Option<Event>> {
    let mut raw = uapi::Event::zeroed();
    // SAFETY: `DQEVENT` writes a `v4l2_event`, which `raw` is.
    match unsafe { ioctl_ptr(fd, uapi::DQEVENT, &raw mut raw) } {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound || e.kind() == io::ErrorKind::WouldBlock => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    }
    Ok(Some(Event {
        event_type: raw.event_type,
        id: raw.id,
        pending: raw.pending,
        sequence: raw.sequence,
        timestamp: duration_of(raw.timestamp[0], raw.timestamp[1], 1),
    }))
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests;
