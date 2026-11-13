//! An MMAP buffer queue on one video node, every buffer mapped ([`MmapQueue`]), or a queue of
//! DMABUF slots importing the caller's dma-bufs ([`MmapQueue::importing`]).

use std::ops::Range;
use std::os::fd::BorrowedFd;
use std::time::{Duration, Instant};

use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::v4l2::payload_range;

use super::{BufType, DequeuedBuffer, Memory, Planes, QueueBuffer, QueuePlane, VideoDevice};
use crate::{Error, Result};

/// The most buffers a queue keeps: a kernel that grants more than this is not believed,
/// so the count it returned never sizes an allocation.
const MAX_QUEUE_BUFFERS: u32 = 1024;

/// Records how long an operation on a queue took: `(queue name, operation, time)`, with the
/// operations `qbuf`, `dqbuf` and `poll`.
pub type QueueTimer = fn(&'static str, &'static str, Duration);

/// A video node with MMAP buffers, all mapped: an image block's parameter, statistics and image
/// queues.
///
/// The mappings stay inside the queue and every slice borrows it, so no slice outlives its
/// mapping. Indices and sizes the kernel reports are checked against the buffers actually held
/// before they are used.
pub struct MmapQueue {
    /// The node.
    pub dev: VideoDevice,
    /// Its queue type.
    pub buf_type: BufType,
    /// Mappings per buffer, per plane (no planes for a queue importing dma-bufs).
    maps: Vec<Vec<Mapping>>,
    /// `Mmap`, or `DmaBuf` for a queue importing dma-bufs.
    pub memory: Memory,
    /// The queue's name, for errors and the timer.
    pub name: &'static str,
    writable: bool,
    timer: Option<QueueTimer>,
}

impl MmapQueue {
    /// Requests `count` MMAP buffers on `dev` and maps them; an error if the driver gives none,
    /// or an implausible number.
    pub fn new(
        dev: VideoDevice,
        buf_type: BufType,
        count: u32,
        name: &'static str,
    ) -> Result<Self> {
        let got = dev.request_buffers(buf_type, Memory::Mmap, count)?;
        let granted = granted_count(got.count, name)?;
        let maps = (0..granted)
            .map(|i| dev.map_buffer(buf_type, i))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            dev,
            buf_type,
            maps,
            memory: Memory::Mmap,
            name,
            writable: buf_type.is_output(),
            timer: None,
        })
    }

    /// Requests `count` DMABUF slots on `dev`: each buffer queued imports the caller's
    /// dma-bufs ([`QueueBuffer::dmabuf`] through [`VideoDevice::queue`]); nothing is mapped and
    /// [`Self::queue`] does not apply. An error if the driver gives none.
    pub fn importing(
        dev: VideoDevice,
        buf_type: BufType,
        count: u32,
        name: &'static str,
    ) -> Result<Self> {
        let got = dev.request_buffers(buf_type, Memory::DmaBuf, count)?;
        let granted = granted_count(got.count, name)?;
        Ok(Self {
            dev,
            buf_type,
            maps: (0..granted).map(|_| Vec::new()).collect(),
            memory: Memory::DmaBuf,
            name,
            writable: false,
            timer: None,
        })
    }

    /// Times `qbuf`, `dqbuf` and `poll` with `timer`.
    pub fn with_timer(mut self, timer: QueueTimer) -> Self {
        self.timer = Some(timer);
        self
    }

    fn timed<T>(&self, op: &'static str, f: impl FnOnce() -> T) -> T {
        match self.timer {
            Some(timer) => {
                let t = Instant::now();
                let r = f();
                timer(self.name, op, t.elapsed());
                r
            }
            None => f(),
        }
    }

    /// Number of buffers.
    pub fn len(&self) -> usize {
        self.maps.len()
    }

    /// Whether there are no buffers.
    pub fn is_empty(&self) -> bool {
        self.maps.is_empty()
    }

    /// The mapping of plane `plane` of buffer `buffer`, if the queue has one.
    pub fn mapping(&self, buffer: usize, plane: usize) -> Option<&Mapping> {
        self.maps.get(buffer)?.get(plane)
    }

    /// The mapped bytes of plane `plane` of buffer `buffer`.
    pub fn plane(&self, buffer: usize, plane: usize) -> Option<&[u8]> {
        self.mapping(buffer, plane).map(Mapping::as_slice)
    }

    /// The mapped bytes of a plane, for writing. `None` when the queue was mapped read-only
    /// (a capture queue) or has no such plane.
    pub fn plane_mut(&mut self, buffer: usize, plane: usize) -> Option<&mut [u8]> {
        if !self.writable {
            return None;
        }
        self.maps
            .get_mut(buffer)?
            .get_mut(plane)
            .map(Mapping::as_mut_slice)
    }

    /// The bytes of `plane` the kernel reported as data in a dequeued buffer. An error when the
    /// buffer or plane is not this queue's, or the kernel's numbers run past the mapping.
    pub fn payload(&self, done: &DequeuedBuffer, plane: usize) -> Result<&[u8]> {
        let mapping = self
            .maps
            .get(done.index as usize)
            .and_then(|m| m.get(plane))
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "{}: no mapped plane {plane} of buffer {}",
                    self.name, done.index
                ))
            })?;
        let (used, offset) = done.planes.get(plane).copied().ok_or_else(|| {
            Error::Invalid(format!(
                "{}: buffer {} has no plane {plane}",
                self.name, done.index
            ))
        })?;
        let range: Range<usize> = payload_range(used, offset, mapping.len())
            .map_err(|e| Error::Invalid(format!("{}: {e}", self.name)))?;
        mapping
            .as_slice()
            .get(range)
            .ok_or_else(|| Error::Invalid(format!("{}: empty mapping", self.name)))
    }

    /// Queues buffer `index`; for output queues `bytes_used` gives each plane's payload. An
    /// error when the buffer is not this queue's, or a payload exceeds its mapping.
    pub fn queue(&self, index: u32, bytes_used: &[u32]) -> Result<()> {
        let mappings = self
            .maps
            .get(index as usize)
            .ok_or_else(|| Error::Invalid(format!("{}: no buffer {index}", self.name)))?;
        let mut q = QueueBuffer::mmap(self.buf_type, index);
        if self.buf_type.is_multiplanar() || !bytes_used.is_empty() {
            q.planes = mappings
                .iter()
                .enumerate()
                .map(|(p, mapping)| QueuePlane {
                    bytes_used: bytes_used
                        .get(p)
                        .copied()
                        .filter(|&used| used as usize <= mapping.len())
                        .unwrap_or_default(),
                    ..Default::default()
                })
                .collect::<Planes<_>>();
            let oversized = bytes_used
                .iter()
                .zip(mappings)
                .any(|(&used, mapping)| used as usize > mapping.len());
            if oversized {
                return Err(Error::Invalid(format!(
                    "{}: a payload does not fit its plane",
                    self.name
                )));
            }
        }
        self.timed("qbuf", || self.dev.queue(&q))
    }

    /// Queues slot `index` of a queue importing dma-bufs ([`Self::importing`]) on `planes`, one
    /// dma-buf per plane. The device takes its own hold on each, so the borrow ends with the
    /// call. An error when the queue maps its own buffers, or has no such slot.
    pub fn queue_dmabuf(&self, index: u32, planes: &[BorrowedFd<'_>]) -> Result<()> {
        if self.memory != Memory::DmaBuf {
            return Err(Error::Invalid(format!(
                "{}: the queue does not import dma-bufs",
                self.name
            )));
        }
        if index as usize >= self.maps.len() {
            return Err(Error::Invalid(format!("{}: no slot {index}", self.name)));
        }
        let q = QueueBuffer::dmabuf(self.buf_type, index, planes);
        self.timed("qbuf", || self.dev.queue(&q))
    }

    /// Dequeues a buffer if one is ready. An error, rather than an index past the queue, when
    /// the kernel names a buffer this queue does not hold.
    pub fn try_dequeue(&self) -> Result<Option<DequeuedBuffer>> {
        let done = self.timed("dqbuf", || self.dev.dequeue(self.buf_type, self.memory))?;
        match done {
            Some(buffer) if buffer.index as usize >= self.maps.len() => {
                Err(Error::Invalid(format!(
                    "{}: the kernel returned buffer {} of {}",
                    self.name,
                    buffer.index,
                    self.maps.len()
                )))
            }
            other => Ok(other),
        }
    }

    /// Dequeues a buffer, waiting up to `timeout`; `None` if none came.
    pub fn dequeue_within(&self, timeout: Duration) -> Result<Option<DequeuedBuffer>> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            if let Some(b) = self.try_dequeue()? {
                return Ok(Some(b));
            }
            let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            if left.is_some_and(|l| l.is_zero()) {
                return Ok(None);
            }
            self.timed("poll", || self.dev.wait(left))?;
        }
    }

    /// Adds up to `count` MMAP buffers after the existing ones (`VIDIOC_CREATE_BUFS`), maps
    /// them, and returns their indices; the caller queues them. Works while streaming where the
    /// driver allows it. Not for a queue importing dma-bufs.
    pub fn add_buffers(&mut self, count: u32) -> Result<std::ops::Range<u32>> {
        if self.memory != Memory::Mmap {
            return Err(Error::Invalid(format!("{}: not an MMAP queue", self.name)));
        }
        let (first, got) = self
            .dev
            .create_buffers(self.buf_type, Memory::Mmap, count)?;
        let range = added_range(first, got.count, self.maps.len(), self.name)?;
        for i in range.clone() {
            self.maps.push(self.dev.map_buffer(self.buf_type, i)?);
        }
        Ok(range)
    }

    /// Starts streaming.
    pub fn stream_on(&self) -> Result<()> {
        self.dev.stream_on(self.buf_type)
    }

    /// Stops streaming and frees the buffers (mappings are dropped first).
    pub fn close(mut self) -> Result<()> {
        let r = self.dev.stream_off(self.buf_type);
        self.maps.clear();
        let f = self.dev.free_buffers(self.buf_type, self.memory);
        r?;
        f
    }
}

/// A `REQBUFS` count usable as a loop bound: at least one, and below the bound.
fn granted_count(count: u32, name: &'static str) -> Result<u32> {
    if count == 0 || count > MAX_QUEUE_BUFFERS {
        return Err(Error::Invalid(format!(
            "{name}: driver granted {count} buffers (1 to {MAX_QUEUE_BUFFERS} allowed)"
        )));
    }
    Ok(count)
}

/// The indices a `CREATE_BUFS` answer adds: they must follow the `held` buffers and stay under
/// the bound.
fn added_range(first: u32, added: u32, held: usize, name: &'static str) -> Result<Range<u32>> {
    let end = first.checked_add(added);
    match end {
        Some(end) if first as usize == held && end <= MAX_QUEUE_BUFFERS => Ok(first..end),
        _ => Err(Error::Invalid(format!(
            "{name}: new buffers {first}+{added} do not follow {held}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v4l2::{BufferFlags, MAX_PLANES, test_device};
    use lemnos_measure::test_alloc::allocations;
    use std::os::fd::AsFd;

    /// A queue of `buffers` single-plane buffers of `len` bytes, mapped from an ordinary file.
    fn queue_of(buffers: usize, len: usize, writable: bool) -> (MmapQueue, std::fs::File) {
        let (dev, file) = test_device();
        file.set_len(len as u64).unwrap();
        let maps = (0..buffers)
            .map(|_| vec![Mapping::new(file.as_fd(), len, 0, writable).unwrap()])
            .collect();
        let queue = MmapQueue {
            dev,
            buf_type: BufType::VideoOutput,
            maps,
            memory: Memory::Mmap,
            name: "test",
            writable,
            timer: None,
        };
        (queue, file)
    }

    #[test]
    fn only_a_queue_that_imports_takes_a_dma_buf_and_only_in_a_slot_it_has() {
        let (mapped, file) = queue_of(2, 64, false);
        let error = mapped.queue_dmabuf(0, &[file.as_fd()]).unwrap_err();
        assert!(matches!(error, Error::Invalid(_)), "{error:?}");
        let importing = MmapQueue {
            memory: Memory::DmaBuf,
            maps: vec![Vec::new(), Vec::new()],
            ..mapped
        };
        let error = importing.queue_dmabuf(2, &[file.as_fd()]).unwrap_err();
        assert!(matches!(error, Error::Invalid(_)), "{error:?}");
        // A slot it has is handed to the device, which here is a plain file and says no.
        let error = importing.queue_dmabuf(1, &[file.as_fd()]).unwrap_err();
        assert!(!matches!(error, Error::Invalid(_)), "{error:?}");
    }

    fn done(index: u32, used: u32, offset: u32) -> DequeuedBuffer {
        DequeuedBuffer {
            index,
            sequence: 0,
            flags: BufferFlags::empty(),
            field: 0,
            timestamp: Duration::ZERO,
            planes: Planes::one((used, offset)),
        }
    }

    #[test]
    fn a_granted_count_must_be_positive_and_under_the_bound() {
        assert_eq!(granted_count(4, "q").unwrap(), 4);
        assert!(granted_count(0, "q").is_err());
        assert!(granted_count(MAX_QUEUE_BUFFERS + 1, "q").is_err());
        assert!(granted_count(u32::MAX, "q").is_err());
    }

    #[test]
    fn create_bufs_answers_must_follow_the_buffers_held() {
        assert_eq!(added_range(4, 2, 4, "q").unwrap(), 4..6);
        assert!(added_range(3, 2, 4, "q").is_err(), "does not follow");
        assert!(added_range(u32::MAX, 1, 4, "q").is_err(), "overflow");
        assert!(
            added_range(4, MAX_QUEUE_BUFFERS, 4, "q").is_err(),
            "too many"
        );
    }

    #[test]
    fn payload_is_inside_the_mapping_for_crafted_numbers() {
        let (queue, _file) = queue_of(2, 4096, false);
        let bytes = queue.payload(&done(1, 16, 4), 0).unwrap();
        assert_eq!(bytes.len(), 12);
        assert_eq!(bytes.as_ptr(), queue.plane(1, 0).unwrap()[4..].as_ptr());
        for (index, used, offset, plane) in [
            (0, 4097, 0, 0), // bytes_used past the mapping
            (0, u32::MAX, 0, 0),
            (0, 100, 101, 0), // data_offset past bytes_used
            (2, 4, 0, 0),     // a buffer the queue does not hold
            (u32::MAX, 4, 0, 0),
            (0, 4, 0, 1), // a plane the queue does not hold
        ] {
            let err = queue
                .payload(&done(index, used, offset), plane)
                .unwrap_err();
            assert!(matches!(err, Error::Invalid(_)), "{index} {used} {offset}");
        }
    }

    #[test]
    fn queueing_is_bounded_by_the_buffers_held() {
        let (queue, _file) = queue_of(2, 4096, true);
        assert!(matches!(
            queue.queue(2, &[]).unwrap_err(),
            Error::Invalid(_)
        ));
        assert!(matches!(
            queue.queue(u32::MAX, &[]).unwrap_err(),
            Error::Invalid(_)
        ));
        assert!(matches!(
            queue.queue(0, &[4097]).unwrap_err(),
            Error::Invalid(_)
        ));
        // In bounds it reaches the kernel: a plain file answers ENOTTY.
        assert_eq!(
            queue.queue(1, &[4096]).unwrap_err().errno(),
            Some(libc::ENOTTY)
        );
    }

    #[test]
    fn a_capture_queue_gives_no_writable_slice() {
        let (mut queue, _file) = queue_of(1, 4096, false);
        assert!(queue.plane_mut(0, 0).is_none());
        let (mut writable, _file) = queue_of(1, 4096, true);
        writable.plane_mut(0, 0).unwrap()[7] = 42;
        assert_eq!(writable.plane(0, 0).unwrap()[7], 42);
        assert!(writable.plane_mut(1, 0).is_none());
    }

    #[test]
    fn queueing_allocates_nothing() {
        let (queue, _file) = queue_of(2, 4096, true);
        let (result, n) = allocations(|| queue.queue(1, &[16]));
        assert_eq!(result.unwrap_err().errno(), Some(libc::ENOTTY));
        assert_eq!(n, 0);
    }

    #[test]
    fn dequeuing_allocates_nothing() {
        let (queue, _file) = queue_of(2, 4096, true);
        let (result, n) = allocations(|| queue.try_dequeue());
        assert_eq!(result.unwrap_err().errno(), Some(libc::ENOTTY));
        assert_eq!(n, 0);
    }

    /// A queue of `slots` multi-planar slots importing dma-bufs, on a test device.
    fn importing_of(slots: usize) -> (MmapQueue, std::fs::File) {
        let (dev, file) = test_device();
        let queue = MmapQueue {
            dev,
            buf_type: BufType::VideoCaptureMplane,
            maps: (0..slots).map(|_| Vec::new()).collect(),
            memory: Memory::DmaBuf,
            name: "test",
            writable: false,
            timer: None,
        };
        (queue, file)
    }

    #[test]
    fn an_importing_queue_takes_a_dma_buf_per_plane_up_to_the_most_a_buffer_has() {
        let (queue, file) = importing_of(2);
        let fd = file.as_fd();
        // Two planes reach the kernel (a test device answers `ENOTTY`).
        let error = queue.queue_dmabuf(1, &[fd, fd]).unwrap_err();
        assert_eq!(error.errno(), Some(libc::ENOTTY), "{error:?}");
        // As many as a buffer can have still do.
        let error = queue.queue_dmabuf(0, &[fd; MAX_PLANES]).unwrap_err();
        assert_eq!(error.errno(), Some(libc::ENOTTY), "{error:?}");
        // One more is refused here, before the kernel.
        let error = queue.queue_dmabuf(0, &[fd; MAX_PLANES + 1]).unwrap_err();
        assert!(matches!(error, Error::Invalid(_)), "{error:?}");
        assert_eq!(error.errno(), None);
    }

    #[test]
    fn queueing_a_dma_buf_per_plane_allocates_nothing() {
        let (queue, file) = importing_of(2);
        let fd = file.as_fd();
        let (result, n) = allocations(|| queue.queue_dmabuf(1, &[fd, fd]));
        assert_eq!(result.unwrap_err().errno(), Some(libc::ENOTTY));
        assert_eq!(n, 0);
    }

    #[test]
    fn a_huge_timeout_does_not_overflow_the_deadline() {
        let (queue, _file) = queue_of(1, 4096, false);
        let err = queue.dequeue_within(Duration::MAX).unwrap_err();
        assert_eq!(err.errno(), Some(libc::ENOTTY), "reached the kernel");
    }
}
