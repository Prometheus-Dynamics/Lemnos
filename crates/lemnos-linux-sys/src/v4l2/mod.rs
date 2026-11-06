//! V4L2 (`/dev/videoN`): capabilities, formats, buffer queues (MMAP and DMA-buf
//! memory), streaming, encoder and decoder commands, and events. Generic
//! device access only: the memory-to-memory session that uses these calls is
//! in `lemnos-linux`.
//!
//! Single-planar and multi-planar queues share one set of functions; the
//! [`BufType`] says which layout a call uses.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

pub mod device;
pub mod hevc;

use device::{
    VIDEO_MAX_PLANES, VIDIOC_DECODER_CMD, VIDIOC_DQBUF, VIDIOC_DQEVENT, VIDIOC_ENCODER_CMD,
    VIDIOC_ENUM_FMT, VIDIOC_G_CTRL, VIDIOC_G_FMT, VIDIOC_G_SELECTION, VIDIOC_QBUF, VIDIOC_QUERYBUF,
    VIDIOC_QUERYCAP, VIDIOC_REQBUFS, VIDIOC_S_CTRL, VIDIOC_S_FMT, VIDIOC_S_SELECTION,
    VIDIOC_STREAMOFF, VIDIOC_STREAMON, VIDIOC_SUBSCRIBE_EVENT, v4l2_buffer, v4l2_capability,
    v4l2_control, v4l2_decoder_cmd, v4l2_encoder_cmd, v4l2_event, v4l2_event_subscription,
    v4l2_exportbuffer, v4l2_fmtdesc, v4l2_format, v4l2_m, v4l2_pix_format, v4l2_pix_format_mplane,
    v4l2_plane, v4l2_requestbuffers, v4l2_selection, zeroed,
};

/// The most planes one buffer has (`VIDEO_MAX_PLANES`).
pub const MAX_PLANES: usize = VIDEO_MAX_PLANES;

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

/// `V4L2_BUF_FLAG_REQUEST_FD`: the buffer belongs to the request in `request_fd`.
pub const BUF_FLAG_REQUEST_FD: u32 = 0x0080_0000;

/// `V4L2_CTRL_WHICH_REQUEST_VAL`: controls of a request, not of the device.
pub const CTRL_WHICH_REQUEST_VAL: u32 = 0x0f01_0000;

/// `V4L2_EVENT_EOS`: the stream ended (after a drain).
pub const EVENT_EOS: u32 = 2;
/// `V4L2_EVENT_CTRL`.
pub const EVENT_CTRL: u32 = 3;
/// `V4L2_EVENT_SOURCE_CHANGE`: the stream's format changed.
pub const EVENT_SOURCE_CHANGE: u32 = 5;
/// `V4L2_EVENT_SRC_CH_RESOLUTION`: a source-change event's `changes` flag for a
/// new resolution (the decoder's coded size changed).
pub const SRC_CH_RESOLUTION: u32 = 1 << 0;
/// `V4L2_CID_HFLIP`: a horizontal flip (a boolean control).
pub const CID_HFLIP: u32 = 0x0098_0914;
/// `V4L2_CID_VFLIP`: a vertical flip (a boolean control).
pub const CID_VFLIP: u32 = 0x0098_0915;
/// `V4L2_CID_ROTATE`: a rotation in degrees (0, 90, 180, 270).
pub const CID_ROTATE: u32 = 0x0098_0922;
/// `V4L2_CID_BG_COLOR`: the background colour of a frame (an RGB value).
pub const CID_BG_COLOR: u32 = 0x0098_0923;

/// Sets control `id` to `value` (`VIDIOC_S_CTRL`) and returns the value the
/// driver kept (the kernel writes it back; a driver clamps or adjusts it).
pub fn s_ctrl(fd: BorrowedFd<'_>, id: u32, value: i32) -> io::Result<i32> {
    let mut raw = v4l2_control { id, value };
    VIDIOC_S_CTRL.call(fd, &mut raw)?;
    Ok(raw.value)
}

/// Reads control `id` (`VIDIOC_G_CTRL`).
pub fn g_ctrl(fd: BorrowedFd<'_>, id: u32) -> io::Result<i32> {
    let mut raw = v4l2_control { id, value: 0 };
    VIDIOC_G_CTRL.call(fd, &mut raw)?;
    Ok(raw.value)
}

/// `V4L2_SEL_TGT_COMPOSE`: the rectangle a frame occupies in the buffer (for a
/// decoder, the visible picture of the decoded frame).
pub const SEL_TGT_COMPOSE: u32 = 0x0100;

/// `V4L2_ENC_CMD_STOP`: finish encoding what is queued.
pub const ENC_CMD_STOP: u32 = 1;
/// `V4L2_DEC_CMD_STOP`: finish decoding what is queued.
pub const DEC_CMD_STOP: u32 = 1;

/// A four-character pixel format code (`V4L2_PIX_FMT_*`), e.g. `fourcc(b"NV12")`.
pub const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

pub use device::{BufType, Memory};

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

pub use device::Rect;

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
    /// The first word of the event's payload: for a source change, the
    /// `changes` flags (see [`SRC_CH_RESOLUTION`]); zero for other events.
    pub changes: u32,
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

// `libc` deprecates `time_t` and `suseconds_t` on musl (they will become 64-bit on 32-bit musl
// targets). The fields are those types, so the alias is named here either way.
#[allow(deprecated)]
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

/// Makes a descriptor non-blocking (`O_NONBLOCK`), so a dequeue returns at once
/// when nothing is ready. Already non-blocking descriptors are left alone.
pub fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    device::set_nonblocking(fd, true)
}

/// Queries the node's capabilities (`VIDIOC_QUERYCAP`).
pub fn querycap(fd: BorrowedFd<'_>) -> io::Result<Capability> {
    let mut raw: v4l2_capability = zeroed();
    VIDIOC_QUERYCAP.call(fd, &mut raw)?;
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
    let mut raw: v4l2_fmtdesc = zeroed();
    raw.index = index;
    raw.type_ = buf_type as u32;
    match VIDIOC_ENUM_FMT.call(fd, &mut raw) {
        Ok(_) => Ok(Some(FormatDesc {
            pixelformat: raw.pixelformat,
            flags: raw.flags,
            description: text(&raw.description),
        })),
        Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(None),
        Err(e) => Err(e),
    }
}

fn decode(buf_type: BufType, raw: &v4l2_format) -> Format {
    if buf_type.is_multiplanar() {
        let m = raw.pix_mp();
        let kernel_planes = m.plane_fmt;
        let mut planes = [PlaneFormat::default(); MAX_PLANES];
        for (dst, src) in planes.iter_mut().zip(kernel_planes.iter()) {
            *dst = PlaneFormat {
                size_image: src.sizeimage,
                bytes_per_line: src.bytesperline,
            };
        }
        Format::Multi(PixFormatMplane {
            width: m.width,
            height: m.height,
            pixelformat: m.pixelformat,
            field: m.field,
            colorspace: m.colorspace,
            planes,
            num_planes: u32::from(m.num_planes),
        })
    } else {
        let p = raw.pix();
        Format::Single(PixFormat {
            width: p.width,
            height: p.height,
            pixelformat: p.pixelformat,
            field: p.field,
            bytes_per_line: p.bytesperline,
            size_image: p.sizeimage,
            colorspace: p.colorspace,
        })
    }
}

fn encode(buf_type: BufType, format: &Format) -> io::Result<v4l2_format> {
    let mut raw: v4l2_format = zeroed();
    raw.type_ = buf_type as u32;
    match (format, buf_type.is_multiplanar()) {
        (Format::Single(p), false) => {
            raw.fmt.pix = v4l2_pix_format {
                width: p.width,
                height: p.height,
                pixelformat: p.pixelformat,
                field: p.field,
                bytesperline: p.bytes_per_line,
                sizeimage: p.size_image,
                colorspace: p.colorspace,
                ..Default::default()
            };
        }
        (Format::Multi(m), true) => {
            checked_planes(m.num_planes)?;
            let mut mp = v4l2_pix_format_mplane {
                width: m.width,
                height: m.height,
                pixelformat: m.pixelformat,
                field: m.field,
                colorspace: m.colorspace,
                num_planes: m.num_planes as u8,
                ..Default::default()
            };
            for (dst, src) in mp.plane_fmt.iter_mut().zip(m.planes.iter()) {
                dst.sizeimage = src.size_image;
                dst.bytesperline = src.bytes_per_line;
            }
            raw.fmt.pix_mp = mp;
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
    let mut raw: v4l2_format = zeroed();
    raw.type_ = buf_type as u32;
    VIDIOC_G_FMT.call(fd, &mut raw)?;
    Ok(decode(buf_type, &raw))
}

/// Sets a queue's format (`VIDIOC_S_FMT`) and returns the one the driver
/// took, which may differ (sizes and strides are rounded). `format` must match
/// `buf_type`'s layout.
pub fn s_fmt(fd: BorrowedFd<'_>, buf_type: BufType, format: &Format) -> io::Result<Format> {
    let mut raw = encode(buf_type, format)?;
    VIDIOC_S_FMT.call(fd, &mut raw)?;
    Ok(decode(buf_type, &raw))
}

/// Reads a queue's selection rectangle for `target` (`VIDIOC_G_SELECTION`),
/// e.g. [`SEL_TGT_CROP`].
pub fn g_selection(fd: BorrowedFd<'_>, buf_type: BufType, target: u32) -> io::Result<Rect> {
    let mut raw: v4l2_selection = zeroed();
    raw.type_ = buf_type as u32;
    raw.target = target;
    VIDIOC_G_SELECTION.call(fd, &mut raw)?;
    Ok(raw.r)
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
    let mut raw: v4l2_selection = zeroed();
    raw.type_ = buf_type as u32;
    raw.target = target;
    raw.r = rect;
    VIDIOC_S_SELECTION.call(fd, &mut raw)?;
    Ok(raw.r)
}

/// A control's value in an ext-controls call: a scalar, or a compound payload
/// (the bytes of the control's struct, copied by the kernel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtValue<'a> {
    /// A scalar (an integer or menu index).
    Int(i32),
    /// A compound control's payload, `size` bytes of its struct.
    Bytes(&'a [u8]),
}

/// One control to set: its id and value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtControl<'a> {
    /// The control's id (`V4L2_CID_*`).
    pub id: u32,
    /// Its value.
    pub value: ExtValue<'a>,
}

/// Sets `controls` on `fd` (`VIDIOC_S_EXT_CTRLS`). With `request`, the values
/// are the request's (`which` [`CTRL_WHICH_REQUEST_VAL`]) and apply when the
/// request runs; without it they are the device's current values. On failure
/// the error names the index of the control that was refused.
pub fn s_ext_ctrls(
    fd: BorrowedFd<'_>,
    request: Option<BorrowedFd<'_>>,
    controls: &[ExtControl<'_>],
) -> io::Result<()> {
    // The kernel only reads a set's payload, but the device layer takes its
    // payloads as exclusive slices, so each one is copied here.
    let mut payloads: Vec<Vec<u8>> = controls
        .iter()
        .map(|c| match c.value {
            ExtValue::Int(_) => Vec::new(),
            ExtValue::Bytes(bytes) => bytes.to_vec(),
        })
        .collect();
    let mut raw: Vec<device::ExtControl<'_>> = controls
        .iter()
        .zip(payloads.iter_mut())
        .map(|(c, payload)| match c.value {
            ExtValue::Int(v) => device::ExtControl::value(c.id, v),
            ExtValue::Bytes(_) => device::ExtControl::payload(c.id, payload.as_mut_slice()),
        })
        .collect();
    let which = if request.is_some() {
        CTRL_WHICH_REQUEST_VAL
    } else {
        0
    };
    device::VIDIOC_S_EXT_CTRLS
        .call(fd, which, request, &mut raw)
        .map_err(|e| {
            io::Error::new(
                e.source.kind(),
                format!("S_EXT_CTRLS refused control {}: {}", e.error_idx, e.source),
            )
        })
}

/// Asks for `count` buffers of `memory` on a queue (`VIDIOC_REQBUFS`). Asking
/// for 0 frees the queue's buffers.
pub fn reqbufs(
    fd: BorrowedFd<'_>,
    buf_type: BufType,
    memory: Memory,
    count: u32,
) -> io::Result<Requested> {
    let mut raw: v4l2_requestbuffers = zeroed();
    raw.count = count;
    raw.type_ = buf_type as u32;
    raw.memory = memory as u32;
    VIDIOC_REQBUFS.call(fd, &mut raw)?;
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
    let mut raw: v4l2_buffer = zeroed();
    raw.index = index;
    raw.type_ = buf_type as u32;
    raw.memory = Memory::Mmap as u32;
    let mut info = BufferInfo {
        planes: [PlaneInfo::default(); MAX_PLANES],
        num_planes: 1,
    };
    if buf_type.is_multiplanar() {
        let n = checked_planes(num_planes)?;
        let mut planes = [zeroed::<v4l2_plane>(); MAX_PLANES];
        VIDIOC_QUERYBUF.call(fd, &mut raw, &mut planes[..n])?;
        info.num_planes = n as u32;
        for (dst, src) in info.planes.iter_mut().zip(&planes).take(n) {
            *dst = PlaneInfo {
                length: src.length,
                offset: src.offset(),
            };
        }
    } else {
        VIDIOC_QUERYBUF.call(fd, &mut raw, &mut [])?;
        info.planes[0] = PlaneInfo {
            length: raw.length,
            offset: raw.offset(),
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
    let mut raw: v4l2_exportbuffer = zeroed();
    raw.type_ = buf_type as u32;
    raw.index = index;
    raw.plane = plane;
    raw.flags = (libc::O_CLOEXEC | libc::O_RDWR) as u32;
    device::export_buffer(fd, &mut raw)
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
    qbuf_with(fd, buf_type, memory, index, planes, timestamp, None)
}

/// Queues buffer `index` as part of a request: the device runs it when the
/// request is queued (`MEDIA_REQUEST_IOC_QUEUE`). Otherwise as [`qbuf`].
pub fn qbuf_in_request(
    fd: BorrowedFd<'_>,
    buf_type: BufType,
    memory: Memory,
    index: u32,
    planes: &[QueuePlane<'_>],
    timestamp: Duration,
    request: BorrowedFd<'_>,
) -> io::Result<()> {
    qbuf_with(
        fd,
        buf_type,
        memory,
        index,
        planes,
        timestamp,
        Some(request),
    )
}

/// The `m` member of a plane or buffer for one piece of memory.
fn memory_of(memory: PlaneMemory<'_>) -> v4l2_m {
    match memory {
        PlaneMemory::Mmap => v4l2_m { offset: 0 },
        PlaneMemory::Dmabuf(fd) => v4l2_m { fd: fd.as_raw_fd() },
    }
}

fn qbuf_with(
    fd: BorrowedFd<'_>,
    buf_type: BufType,
    memory: Memory,
    index: u32,
    planes: &[QueuePlane<'_>],
    timestamp: Duration,
    request: Option<BorrowedFd<'_>>,
) -> io::Result<()> {
    let mut raw: v4l2_buffer = zeroed();
    raw.index = index;
    raw.type_ = buf_type as u32;
    raw.memory = memory as u32;
    raw.timestamp = timeval(timestamp);
    if let Some(request) = request {
        raw.flags |= BUF_FLAG_REQUEST_FD;
        raw.request_fd = request.as_raw_fd();
    }
    if buf_type.is_multiplanar() {
        let n = checked_planes(planes.len() as u32)?;
        let mut kernel_planes = [zeroed::<v4l2_plane>(); MAX_PLANES];
        for (k, p) in kernel_planes.iter_mut().zip(planes) {
            k.length = p.length;
            k.bytesused = p.bytes_used;
            k.data_offset = p.data_offset;
            k.m = memory_of(p.memory);
        }
        VIDIOC_QBUF.call(fd, &mut raw, &mut kernel_planes[..n])?;
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
        raw.m = memory_of(p.memory);
        VIDIOC_QBUF.call(fd, &mut raw, &mut [])?;
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
    let mut raw: v4l2_buffer = zeroed();
    raw.type_ = buf_type as u32;
    raw.memory = memory as u32;
    let mut kernel_planes = [zeroed::<v4l2_plane>(); MAX_PLANES];
    let (n, result) = if buf_type.is_multiplanar() {
        let n = checked_planes(num_planes)?;
        (n, VIDIOC_DQBUF.call(fd, &mut raw, &mut kernel_planes[..n]))
    } else {
        (1, VIDIOC_DQBUF.call(fd, &mut raw, &mut []))
    };
    match result {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
        // EPIPE: the last buffer of a drained stream has been dequeued (its
        // LAST flag said so), and nothing more is coming. Not an error.
        Err(e) if e.raw_os_error() == Some(libc::EPIPE) => return Ok(None),
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
    let mut t = buf_type as libc::c_int;
    VIDIOC_STREAMON.call(fd, &mut t)?;
    Ok(())
}

/// Stops streaming a queue (`VIDIOC_STREAMOFF`); the device gives every queued
/// buffer back.
pub fn streamoff(fd: BorrowedFd<'_>, buf_type: BufType) -> io::Result<()> {
    let mut t = buf_type as libc::c_int;
    VIDIOC_STREAMOFF.call(fd, &mut t)?;
    Ok(())
}

/// Sends an encoder command (`VIDIOC_ENCODER_CMD`), e.g. [`ENC_CMD_STOP`].
pub fn encoder_cmd(fd: BorrowedFd<'_>, cmd: u32, flags: u32) -> io::Result<()> {
    let mut raw: v4l2_encoder_cmd = zeroed();
    raw.cmd = cmd;
    raw.flags = flags;
    VIDIOC_ENCODER_CMD.call(fd, &mut raw)?;
    Ok(())
}

/// Sends a decoder command (`VIDIOC_DECODER_CMD`), e.g. [`DEC_CMD_STOP`].
pub fn decoder_cmd(fd: BorrowedFd<'_>, cmd: u32, flags: u32) -> io::Result<()> {
    let mut raw: v4l2_decoder_cmd = zeroed();
    raw.cmd = cmd;
    raw.flags = flags;
    VIDIOC_DECODER_CMD.call(fd, &mut raw)?;
    Ok(())
}

/// Subscribes to events of `event_type` (`VIDIOC_SUBSCRIBE_EVENT`), e.g.
/// [`EVENT_EOS`]. Control events take the control's id in `id` (0 for all of
/// them); other events take 0.
pub fn subscribe_event(fd: BorrowedFd<'_>, event_type: u32, id: u32) -> io::Result<()> {
    let mut raw: v4l2_event_subscription = zeroed();
    raw.type_ = event_type;
    raw.id = id;
    VIDIOC_SUBSCRIBE_EVENT.call(fd, &mut raw)?;
    Ok(())
}

/// Takes the next pending event (`VIDIOC_DQEVENT`), or `None` when there is
/// none: the kernel answers ENOENT for an empty queue of a non-blocking
/// descriptor.
pub fn dqevent(fd: BorrowedFd<'_>) -> io::Result<Option<Event>> {
    let mut raw: v4l2_event = zeroed();
    match VIDIOC_DQEVENT.call(fd, &mut raw) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound || e.kind() == io::ErrorKind::WouldBlock => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    }
    let p = raw.u.0;
    Ok(Some(Event {
        event_type: raw.type_,
        id: raw.id,
        pending: raw.pending,
        sequence: raw.sequence,
        changes: u32::from_ne_bytes([p[0], p[1], p[2], p[3]]),
        timestamp: duration_of(raw.timestamp.tv_sec, raw.timestamp.tv_nsec, 1),
    }))
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests;
