//! Buffers and streaming: REQBUFS (MMAP, DMABUF), QUERYBUF, mmap, EXPBUF, QBUF/DQBUF,
//! STREAMON/OFF.

use std::os::fd::{AsFd, BorrowedFd};
use std::time::Duration;

use lemnos_linux_sys::mmap::{Mapping, page_size};
use lemnos_linux_sys::v4l2 as sys;
use lemnos_linux_sys::v4l2::device::{
    VIDEO_MAX_PLANES, VIDIOC_CREATE_BUFS, VIDIOC_G_FMT, VIDIOC_REQBUFS, VIDIOC_STREAMOFF,
    VIDIOC_STREAMON, v4l2_create_buffers, v4l2_requestbuffers, zeroed,
};

use super::{BufType, DmaBufAccess, VideoDevice};
use crate::flags::flags;
use crate::node;
use crate::{Error, Result};
use std::os::fd::OwnedFd;

/// The most planes a buffer has (`VIDEO_MAX_PLANES`).
pub const MAX_PLANES: usize = VIDEO_MAX_PLANES;

/// A buffer's planes, held inline: no allocation per queue or dequeue. Derefs to the planes
/// as a slice. A list longer than [`MAX_PLANES`] keeps the first ones and sets
/// [`Planes::is_over`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Planes<T> {
    len: usize,
    over: bool,
    items: [T; MAX_PLANES],
}

impl<T: Copy + Default> Planes<T> {
    /// No planes.
    pub fn new() -> Self {
        Self {
            len: 0,
            over: false,
            items: [T::default(); MAX_PLANES],
        }
    }

    /// One plane.
    pub fn one(p: T) -> Self {
        let mut out = Self::new();
        out.items[0] = p;
        out.len = 1;
        out
    }

    /// Whether more planes were given than fit (the buffer is then not queued).
    pub fn is_over(&self) -> bool {
        self.over
    }
}

impl<T: Copy + Default> Default for Planes<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + Default> FromIterator<T> for Planes<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut out = Self::new();
        for p in iter {
            if out.len == MAX_PLANES {
                out.over = true;
                break;
            }
            out.items[out.len] = p;
            out.len += 1;
        }
        out
    }
}

impl<T> std::ops::Deref for Planes<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.items[..self.len]
    }
}

impl<T> std::ops::DerefMut for Planes<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.items[..self.len]
    }
}

impl<'a, T> IntoIterator for &'a Planes<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub use lemnos_linux_sys::v4l2::device::Memory;

flags! {
    /// What a queue supports (`V4L2_BUF_CAP_*`), reported by `VIDIOC_REQBUFS`.
    pub struct BufferCapabilities: u32 {
        const SUPPORTS_MMAP = 1 << 0;
        const SUPPORTS_USERPTR = 1 << 1;
        const SUPPORTS_DMABUF = 1 << 2;
        const SUPPORTS_REQUESTS = 1 << 3;
        const SUPPORTS_ORPHANED_BUFS = 1 << 4;
        const SUPPORTS_M2M_HOLD_CAPTURE_BUF = 1 << 5;
        const SUPPORTS_MMAP_CACHE_HINTS = 1 << 6;
        const SUPPORTS_MAX_NUM_BUFFERS = 1 << 7;
        const SUPPORTS_REMOVE_BUFS = 1 << 8;
    }
}

flags! {
    /// Buffer state and metadata flags (`V4L2_BUF_FLAG_*`).
    pub struct BufferFlags: u32 {
        const MAPPED = 0x0000_0001;
        const QUEUED = 0x0000_0002;
        const DONE = 0x0000_0004;
        const KEYFRAME = 0x0000_0008;
        const PFRAME = 0x0000_0010;
        const BFRAME = 0x0000_0020;
        const ERROR = 0x0000_0040;
        const IN_REQUEST = 0x0000_0080;
        const TIMECODE = 0x0000_0100;
        const M2M_HOLD_CAPTURE_BUF = 0x0000_0200;
        const PREPARED = 0x0000_0400;
        const NO_CACHE_INVALIDATE = 0x0000_0800;
        const NO_CACHE_CLEAN = 0x0000_1000;
        const TIMESTAMP_MONOTONIC = 0x0000_2000;
        const TIMESTAMP_COPY = 0x0000_4000;
        const TSTAMP_SRC_SOE = 0x0001_0000;
        const LAST = 0x0010_0000;
        const REQUEST_FD = 0x0080_0000;
    }
}

/// The result of `VIDIOC_REQBUFS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestedBuffers {
    /// Number of buffers allocated (may differ from the number asked for).
    pub count: u32,
    /// What the queue supports.
    pub capabilities: BufferCapabilities,
}

/// One plane of a buffer, as reported by `VIDIOC_QUERYBUF`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaneInfo {
    /// Plane size in bytes.
    pub length: u32,
    /// The `mmap` offset (MMAP memory).
    pub mem_offset: u32,
    /// Bytes of payload.
    pub bytes_used: u32,
    /// Offset of the data from the start of the plane.
    pub data_offset: u32,
}

/// A buffer's description (`VIDIOC_QUERYBUF`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferInfo {
    /// Buffer index.
    pub index: u32,
    /// Queue type.
    pub buf_type: BufType,
    /// Memory type.
    pub memory: Option<Memory>,
    /// State flags.
    pub flags: BufferFlags,
    /// The planes (one for single-planar queues).
    pub planes: Vec<PlaneInfo>,
}

/// A plane to queue. For MMAP buffers only `bytes_used` matters (and only for output queues);
/// for DMABUF buffers `dmabuf` is the dma-buf to import.
#[derive(Clone, Copy, Debug, Default)]
pub struct QueuePlane<'fd> {
    /// The dma-buf to import (DMABUF memory).
    pub dmabuf: Option<BorrowedFd<'fd>>,
    /// Size of the dma-buf plane in bytes (DMABUF; 0 lets the driver use the dma-buf size).
    pub length: u32,
    /// Payload size (output queues).
    pub bytes_used: u32,
    /// Offset of the data in the plane (output queues, multi-planar).
    pub data_offset: u32,
}

/// A buffer to queue with [`VideoDevice::queue`].
#[derive(Clone, Debug)]
pub struct QueueBuffer<'fd> {
    /// Queue type.
    pub buf_type: BufType,
    /// Memory type the queue was set up with.
    pub memory: Memory,
    /// Buffer index.
    pub index: u32,
    /// Planes (one for single-planar queues; may be empty for capture MMAP buffers).
    pub planes: Planes<QueuePlane<'fd>>,
    /// Queue the buffer into this media request instead of directly.
    pub request: Option<BorrowedFd<'fd>>,
    /// Field order (output queues; 0 lets the driver choose).
    pub field: u32,
    /// Timestamp to attach (output queues; copied to the capture side by m2m devices).
    pub timestamp: Duration,
}

impl<'fd> QueueBuffer<'fd> {
    /// An MMAP buffer with no payload information (capture).
    pub fn mmap(buf_type: BufType, index: u32) -> Self {
        Self {
            buf_type,
            memory: Memory::Mmap,
            index,
            planes: Planes::new(),
            request: None,
            field: 0,
            timestamp: Duration::ZERO,
        }
    }

    /// A DMABUF buffer importing one dma-buf per plane.
    pub fn dmabuf(buf_type: BufType, index: u32, planes: &[BorrowedFd<'fd>]) -> Self {
        Self {
            planes: planes
                .iter()
                .map(|&fd| QueuePlane {
                    dmabuf: Some(fd),
                    ..Default::default()
                })
                .collect(),
            memory: Memory::DmaBuf,
            ..Self::mmap(buf_type, index)
        }
    }
}

/// A buffer returned by [`VideoDevice::dequeue`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DequeuedBuffer {
    /// Buffer index.
    pub index: u32,
    /// Frame sequence number from the driver.
    pub sequence: u32,
    /// Flags (`ERROR` marks a corrupted frame; `LAST` the end of a stream).
    pub flags: BufferFlags,
    /// Field order.
    pub field: u32,
    /// Capture timestamp; `CLOCK_MONOTONIC` when `flags` has `TIMESTAMP_MONOTONIC`.
    pub timestamp: Duration,
    /// Per-plane payload: `(bytes_used, data_offset)`.
    pub planes: Planes<(u32, u32)>,
}

impl DequeuedBuffer {
    /// Total payload bytes over all planes.
    pub fn bytes_used(&self) -> usize {
        self.planes.iter().fold(0usize, |total, &(used, _)| {
            total.saturating_add(used as usize)
        })
    }
}

/// The largest plane a buffer maps. A kernel reporting more is not believed; no plane of any
/// video or metadata format comes close.
const MAX_PLANE_BYTES: u32 = 1 << 30;

/// A `QUERYBUF` answer as this crate's [`BufferInfo`], refused if the kernel answered about
/// another buffer than the one asked for.
fn buffer_info_of(index: u32, buf_type: BufType, info: &sys::BufferInfo) -> Result<BufferInfo> {
    if info.index != index {
        return Err(Error::Invalid(format!(
            "VIDIOC_QUERYBUF answered for buffer {} when asked for {index}",
            info.index
        )));
    }
    let count = (info.num_planes as usize).min(MAX_PLANES);
    Ok(BufferInfo {
        index,
        buf_type,
        memory: Memory::from_raw(info.memory),
        flags: BufferFlags(info.flags),
        planes: info.planes[..count]
            .iter()
            .map(|p| PlaneInfo {
                length: p.length,
                mem_offset: p.offset,
                bytes_used: p.bytes_used,
                data_offset: p.data_offset,
            })
            .collect(),
    })
}

/// The length and offset for `mmap` of a plane the kernel described, checked: a positive
/// length under [`MAX_PLANE_BYTES`] and a page-aligned offset. `mmap` itself refuses a range
/// the buffer does not have.
fn mmap_bounds(plane: &PlaneInfo) -> Result<(usize, u64)> {
    let bad = |what: &str| {
        Error::Invalid(format!(
            "VIDIOC_QUERYBUF reported {what}: length {}, offset {:#x}",
            plane.length, plane.mem_offset
        ))
    };
    if plane.length == 0 || plane.length > MAX_PLANE_BYTES {
        return Err(bad("an implausible plane size"));
    }
    let length = usize::try_from(plane.length).map_err(|_| bad("a plane too large for memory"))?;
    let page = u64::try_from(page_size()).map_err(|_| bad("an unreadable page size"))?;
    if page == 0 || u64::from(plane.mem_offset) % page != 0 {
        return Err(bad("an unaligned mmap offset"));
    }
    Ok((length, u64::from(plane.mem_offset)))
}

/// Maps the planes a validated `QUERYBUF` described. An MMAP buffer with no plane to map is an
/// error, as is a plane described for any other kind of memory.
fn map_planes(fd: BorrowedFd<'_>, info: &BufferInfo, writable: bool) -> Result<Vec<Mapping>> {
    if info.memory != Some(Memory::Mmap) {
        return Err(Error::Invalid(format!(
            "buffer {} is not MMAP memory",
            info.index
        )));
    }
    if info.planes.is_empty() {
        return Err(Error::Invalid(format!(
            "VIDIOC_QUERYBUF reported no planes for buffer {}",
            info.index
        )));
    }
    info.planes
        .iter()
        .map(|p| {
            let (length, offset) = mmap_bounds(p)?;
            Mapping::new(fd, length, offset, writable)
                .map_err(|source| Error::sys_io("mmap", source))
        })
        .collect()
}

/// This crate's [`DequeuedBuffer`] from the kernel's answer. Copies at most what it holds.
fn dequeued_from(d: &sys::Dequeued) -> DequeuedBuffer {
    let planes = (d.num_planes as usize).min(MAX_PLANES);
    DequeuedBuffer {
        index: d.index,
        sequence: d.sequence,
        flags: BufferFlags(d.flags),
        field: d.field,
        timestamp: d.timestamp,
        planes: d.bytes_used[..planes]
            .iter()
            .zip(&d.data_offset[..planes])
            .map(|(&used, &offset)| (used, offset))
            .collect(),
    }
}

impl VideoDevice {
    /// Allocates (or, with `count == 0`, frees) the buffers of a queue (`VIDIOC_REQBUFS`).
    pub fn request_buffers(
        &self,
        buf_type: BufType,
        memory: Memory,
        count: u32,
    ) -> Result<RequestedBuffers> {
        let mut raw: v4l2_requestbuffers = zeroed();
        raw.count = count;
        raw.type_ = buf_type.to_raw();
        raw.memory = memory as u32;
        node::ioctl(self.as_fd(), VIDIOC_REQBUFS, &mut raw)?;
        Ok(RequestedBuffers {
            count: raw.count,
            capabilities: BufferCapabilities(raw.capabilities),
        })
    }

    /// Adds up to `count` buffers after the existing ones, sized for the queue's current
    /// format (`VIDIOC_CREATE_BUFS`); works while streaming where the driver allows it. Returns
    /// the index of the first new buffer and how many were made (possibly fewer than asked, or
    /// none). They are freed only with the whole queue, unless the queue reports
    /// [`BufferCapabilities::SUPPORTS_REMOVE_BUFS`].
    pub fn create_buffers(
        &self,
        buf_type: BufType,
        memory: Memory,
        count: u32,
    ) -> Result<(u32, RequestedBuffers)> {
        let mut raw: v4l2_create_buffers = zeroed();
        raw.format.type_ = buf_type.to_raw();
        node::ioctl(self.as_fd(), VIDIOC_G_FMT, &mut raw.format)?;
        raw.count = count;
        raw.memory = memory as u32;
        node::ioctl(self.as_fd(), VIDIOC_CREATE_BUFS, &mut raw)?;
        Ok((
            raw.index,
            RequestedBuffers {
                count: raw.count,
                capabilities: BufferCapabilities(raw.capabilities),
            },
        ))
    }

    /// Frees all buffers of a queue (`VIDIOC_REQBUFS` with count 0). Mappings and exported
    /// dma-bufs keep the memory alive until they are dropped.
    pub fn free_buffers(&self, buf_type: BufType, memory: Memory) -> Result<()> {
        self.request_buffers(buf_type, memory, 0).map(|_| ())
    }

    /// Describes a buffer (`VIDIOC_QUERYBUF`). The numbers are the kernel's: see
    /// [`Self::map_buffer`] for mapping them safely.
    pub fn query_buffer(&self, buf_type: BufType, index: u32) -> Result<BufferInfo> {
        let info = sys::querybuf(self.as_fd(), buf_type, index, MAX_PLANES as u32)
            .map_err(|source| Error::ioctl_io("VIDIOC_QUERYBUF", source))?;
        buffer_info_of(index, buf_type, &info)
    }

    /// Maps every plane of an MMAP buffer into memory (`VIDIOC_QUERYBUF` + `mmap`), the lengths
    /// and offsets checked first. Each [`Mapping`] unmaps itself on drop, so a slice of it cannot
    /// outlive the mapping.
    pub fn map_buffer(&self, buf_type: BufType, index: u32) -> Result<Vec<Mapping>> {
        self.map_buffer_as(buf_type, index, buf_type.is_output())
    }

    /// [`Self::map_buffer`], writable whatever the direction.
    pub fn map_buffer_writable(&self, buf_type: BufType, index: u32) -> Result<Vec<Mapping>> {
        self.map_buffer_as(buf_type, index, true)
    }

    fn map_buffer_as(&self, buf_type: BufType, index: u32, writable: bool) -> Result<Vec<Mapping>> {
        let info = self.query_buffer(buf_type, index)?;
        map_planes(self.as_fd(), &info, writable)
    }

    /// Exports one plane of an MMAP buffer as a read-write dma-buf (`VIDIOC_EXPBUF`),
    /// close-on-exec. [`VideoDevice::export_buffer_with`] chooses the access.
    pub fn export_buffer(&self, buf_type: BufType, index: u32, plane: u32) -> Result<OwnedFd> {
        self.export_buffer_with(buf_type, index, plane, DmaBufAccess::ReadWrite)
    }

    /// Queues a buffer (`VIDIOC_QBUF`). User-pointer memory is refused: the kernel would keep
    /// the address past the call, which a borrowed buffer cannot promise.
    pub fn queue(&self, req: &QueueBuffer<'_>) -> Result<()> {
        self.queue_or_prepare(req, false)
    }

    /// Maps a buffer's dma-bufs into the queue without queueing it (`VIDIOC_PREPARE_BUF`).
    /// The first queueing of a dma-buf otherwise attaches and maps it on the queueing thread;
    /// a buffer prepared with the same dma-buf is queued later without that work.
    pub fn prepare(&self, req: &QueueBuffer<'_>) -> Result<()> {
        self.queue_or_prepare(req, true)
    }

    fn queue_or_prepare(&self, req: &QueueBuffer<'_>, prepare: bool) -> Result<()> {
        let name = if prepare {
            "VIDIOC_PREPARE_BUF"
        } else {
            "VIDIOC_QBUF"
        };
        if req.planes.is_over() {
            return Err(Error::Invalid(format!("more than {MAX_PLANES} planes")));
        }
        if req.memory == Memory::DmaBuf && req.planes.iter().any(|p| p.dmabuf.is_none()) {
            return Err(Error::Invalid(
                "DMABUF queue needs a dma-buf for every plane".into(),
            ));
        }
        let mut planes = [sys::QueuePlane {
            memory: sys::PlaneMemory::Mmap,
            length: 0,
            bytes_used: 0,
            data_offset: 0,
        }; MAX_PLANES];
        for (dst, src) in planes.iter_mut().zip(req.planes.iter()) {
            dst.memory = match src.dmabuf {
                Some(fd) => sys::PlaneMemory::Dmabuf(fd),
                None => sys::PlaneMemory::Mmap,
            };
            dst.length = src.length;
            dst.bytes_used = src.bytes_used;
            dst.data_offset = src.data_offset;
        }
        // A single-planar MMAP buffer always passes one (empty) plane.
        let count = if !req.buf_type.is_multiplanar() && req.planes.is_empty() {
            1
        } else {
            req.planes.len()
        };
        let queue_request = sys::QueueRequest {
            buf_type: req.buf_type,
            memory: req.memory,
            index: req.index,
            planes: &planes[..count],
            timestamp: req.timestamp,
            field: req.field,
            request: req.request,
        };
        let result = if prepare {
            sys::prepare_buf(self.as_fd(), &queue_request)
        } else {
            sys::qbuf_buffer(self.as_fd(), &queue_request)
        };
        result.map_err(|source| Error::ioctl_io(name, source))
    }

    /// Dequeues a finished buffer (`VIDIOC_DQBUF`). Returns `Ok(None)` when none is ready, or a
    /// drained queue has nothing more (the device is non-blocking by default). The buffer's
    /// numbers are the kernel's: see [`MmapQueue::payload`](super::MmapQueue::payload).
    pub fn dequeue(&self, buf_type: BufType, memory: Memory) -> Result<Option<DequeuedBuffer>> {
        Ok(
            sys::dqbuf(self.as_fd(), buf_type, memory, MAX_PLANES as u32)
                .map_err(|source| Error::ioctl_io("VIDIOC_DQBUF", source))?
                .map(|d| dequeued_from(&d)),
        )
    }

    /// Starts streaming on a queue (`VIDIOC_STREAMON`).
    pub fn stream_on(&self, buf_type: BufType) -> Result<()> {
        let mut ty = buf_type.to_raw() as libc::c_int;
        node::ioctl(self.as_fd(), VIDIOC_STREAMON, &mut ty)
    }

    /// Stops streaming on a queue (`VIDIOC_STREAMOFF`); all buffers return to userspace.
    pub fn stream_off(&self, buf_type: BufType) -> Result<()> {
        let mut ty = buf_type.to_raw() as libc::c_int;
        node::ioctl(self.as_fd(), VIDIOC_STREAMOFF, &mut ty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v4l2::test_device;
    use lemnos_measure::test_alloc::allocations;
    use std::io::{Seek, SeekFrom, Write};

    #[test]
    fn buffer_flags_debug() {
        let f = BufferFlags::DONE | BufferFlags::TIMESTAMP_MONOTONIC;
        assert_eq!(format!("{f:?}"), "DONE | TIMESTAMP_MONOTONIC");
    }

    #[test]
    fn planes_keep_the_first_eight_and_flag_the_rest() {
        let planes: Planes<u32> = (0..10).collect();
        assert_eq!(planes.len(), MAX_PLANES);
        assert!(planes.is_over());
        assert_eq!(planes[7], 7);
        let one = Planes::one(5u32);
        assert_eq!(&*one, &[5]);
        assert!(!one.is_over());
    }

    fn plane(length: u32, mem_offset: u32) -> PlaneInfo {
        PlaneInfo {
            length,
            mem_offset,
            bytes_used: 0,
            data_offset: 0,
        }
    }

    #[test]
    fn crafted_mmap_numbers_are_refused() {
        let page = page_size() as u32;
        assert_eq!(mmap_bounds(&plane(4096, 0)).unwrap(), (4096, 0));
        assert_eq!(
            mmap_bounds(&plane(4096, page * 2)).unwrap(),
            (4096, u64::from(page * 2))
        );
        for (length, offset) in [
            (0, 0),                   // no plane at all
            (MAX_PLANE_BYTES + 1, 0), // a crafted huge length
            (u32::MAX, 0),            // the largest length the kernel could report
            (4096, 1),                // an unaligned offset
            (4096, page + 1),
        ] {
            assert!(
                mmap_bounds(&plane(length, offset)).is_err(),
                "{length} {offset:#x}"
            );
        }
    }

    fn info_of(planes: Vec<PlaneInfo>) -> BufferInfo {
        BufferInfo {
            index: 0,
            buf_type: BufType::VideoCaptureMplane,
            memory: Some(Memory::Mmap),
            flags: BufferFlags::empty(),
            planes,
        }
    }

    #[test]
    fn map_buffer_refuses_a_query_with_no_planes_or_the_wrong_memory() {
        let (_dev, file) = test_device();
        let fd = file.as_fd();
        let e = map_planes(fd, &info_of(Vec::new()), false).unwrap_err();
        assert!(e.to_string().contains("no planes"), "{e}");
        let mut info = info_of(vec![plane(4096, 0)]);
        info.memory = Some(Memory::DmaBuf);
        let e = map_planes(fd, &info, false).unwrap_err();
        assert!(e.to_string().contains("not MMAP"), "{e}");
        info.memory = None;
        assert!(map_planes(fd, &info, false).is_err());
    }

    #[test]
    fn a_mapping_is_exactly_the_length_and_offset_the_query_said() {
        let (_dev, mut file) = test_device();
        let page = page_size();
        file.set_len(2 * page as u64).unwrap();
        file.seek(SeekFrom::Start(page as u64)).unwrap();
        file.write_all(&[9, 8, 7, 6]).unwrap();
        let info = info_of(vec![plane(page as u32, page as u32)]);
        let maps = map_planes(file.as_fd(), &info, false).unwrap();
        assert_eq!(maps.len(), 1);
        assert_eq!(maps[0].len(), page);
        assert_eq!(&maps[0].as_slice()[..4], &[9, 8, 7, 6]);
    }

    #[test]
    fn a_map_with_a_bad_plane_maps_nothing() {
        let (_dev, file) = test_device();
        file.set_len(page_size() as u64).unwrap();
        let info = info_of(vec![plane(page_size() as u32, 0), plane(0, 0)]);
        let e = map_planes(file.as_fd(), &info, false).unwrap_err();
        assert!(e.to_string().contains("implausible"), "{e}");
    }

    #[test]
    fn query_answers_about_another_buffer_are_refused() {
        let mut raw = sys::BufferInfo {
            index: 7,
            memory: 1,
            flags: 0,
            planes: [sys::PlaneInfo::default(); MAX_PLANES],
            num_planes: 2,
        };
        raw.planes[0].length = 4096;
        raw.planes[1].length = 2048;
        raw.planes[1].offset = 0x1000;
        let good = buffer_info_of(7, BufType::VideoCaptureMplane, &raw).unwrap();
        assert_eq!(good.planes.len(), 2);
        assert_eq!(good.planes[1], plane(2048, 0x1000));
        assert_eq!(good.memory, Some(Memory::Mmap));
        assert!(buffer_info_of(6, BufType::VideoCaptureMplane, &raw).is_err());
        raw.num_planes = u32::MAX; // sys bounds this; here it cannot make a longer slice either
        let clamped = buffer_info_of(7, BufType::VideoCaptureMplane, &raw).unwrap();
        assert_eq!(clamped.planes.len(), MAX_PLANES);
    }

    #[test]
    fn dequeued_buffers_copy_only_the_planes_they_hold() {
        let mut d = sys::Dequeued {
            index: 2,
            flags: BufferFlags::ERROR.0,
            field: 1,
            timestamp: Duration::from_millis(5),
            sequence: 9,
            bytes_used: [0; MAX_PLANES],
            data_offset: [0; MAX_PLANES],
            num_planes: 2,
        };
        d.bytes_used[0] = 100;
        d.bytes_used[1] = 50;
        d.data_offset[1] = 4;
        let out = dequeued_from(&d);
        assert_eq!(out.index, 2);
        assert!(out.flags.contains(BufferFlags::ERROR));
        assert_eq!(&*out.planes, &[(100, 0), (50, 4)]);
        assert_eq!(out.bytes_used(), 150);
        d.num_planes = u32::MAX;
        assert_eq!(dequeued_from(&d).planes.len(), MAX_PLANES);
    }

    #[test]
    fn a_dmabuf_queue_takes_one_plane_per_descriptor() {
        let (_dev, file) = test_device();
        let fd = file.as_fd();
        let q = QueueBuffer::dmabuf(BufType::VideoCaptureMplane, 2, &[fd, fd]);
        assert_eq!(q.planes.len(), 2);
        assert_eq!(q.memory, Memory::DmaBuf);
        assert!(q.planes.iter().all(|p| p.dmabuf.is_some()));
    }

    #[test]
    fn a_dmabuf_queue_needs_a_descriptor_on_every_plane() {
        let (dev, file) = test_device();
        let mut q = QueueBuffer::dmabuf(BufType::VideoCaptureMplane, 0, &[file.as_fd()]);
        q.planes[0].dmabuf = None;
        assert!(matches!(dev.queue(&q).unwrap_err(), Error::Invalid(_)));
    }

    #[test]
    fn queueing_user_pointer_memory_is_refused_before_the_kernel() {
        let (dev, _file) = test_device();
        let mut q = QueueBuffer::mmap(BufType::VideoOutput, 0);
        q.memory = Memory::UserPtr;
        let err = dev.queue(&q).unwrap_err();
        assert!(err.to_string().contains("user-pointer"), "{err}");
        assert_eq!(err.errno(), None, "refused here, not by the kernel");
        let err = dev
            .dequeue(BufType::VideoOutput, Memory::UserPtr)
            .unwrap_err();
        assert_eq!(
            err.errno(),
            Some(libc::EINVAL),
            "ENOTTY would be the kernel's"
        );
    }

    #[test]
    fn a_well_formed_queue_reaches_the_kernel() {
        let (dev, file) = test_device();
        let fd = file.as_fd();
        let q = QueueBuffer::dmabuf(BufType::VideoOutputMplane, 1, &[fd, fd]);
        assert_eq!(dev.queue(&q).unwrap_err().errno(), Some(libc::ENOTTY));
        assert_eq!(dev.prepare(&q).unwrap_err().errno(), Some(libc::ENOTTY));
        let single = QueueBuffer::mmap(BufType::VideoCapture, 0);
        assert_eq!(dev.queue(&single).unwrap_err().errno(), Some(libc::ENOTTY));
    }

    #[test]
    fn the_steady_state_calls_allocate_nothing() {
        let (dev, file) = test_device();
        let fd = file.as_fd();
        let q = QueueBuffer::dmabuf(BufType::VideoOutputMplane, 1, &[fd, fd]);
        let d = sys::Dequeued {
            index: 1,
            flags: 0,
            field: 0,
            timestamp: Duration::ZERO,
            sequence: 0,
            bytes_used: [1; MAX_PLANES],
            data_offset: [0; MAX_PLANES],
            num_planes: MAX_PLANES as u32,
        };
        let (results, n) = allocations(|| {
            (
                dev.queue(&q),
                dev.dequeue(BufType::VideoCaptureMplane, Memory::Mmap),
                dequeued_from(&d),
            )
        });
        assert_eq!(n, 0);
        assert_eq!(results.0.unwrap_err().errno(), Some(libc::ENOTTY));
        assert_eq!(results.1.unwrap_err().errno(), Some(libc::ENOTTY));
        assert_eq!(results.2.planes.len(), MAX_PLANES);
    }
}
