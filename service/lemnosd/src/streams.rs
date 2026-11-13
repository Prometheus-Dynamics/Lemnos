//! What a frame stream is backed by on Linux: sealed memory the client can map but can't resize,
//! and an eventfd. A stream the board file declares as dma-bufs gets one dma-buf per buffer from
//! a dma-heap in place of the one memfd for all of them, so a driver can hand a buffer to
//! hardware, or one per plane of each buffer when the board file gives the buffers planes; the
//! ring and the doorbell are the same.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use lemnos_core::Status;
use lemnos_host::stream::{Backing, BufferMemory, Doorbell, StreamAllocator};
use lemnos_linux_sys::dma_buf::{self, SYNC_END, SYNC_READ, SYNC_START, SYNC_WRITE, sealed_memfd};
use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::{dma_heap, eventfd};
use lemnos_wire::ring::{Layout, WordMemory};
use lemnos_wire::{Planes, handle_descriptors};

/// Where the buffers of a stream of dma-bufs come from.
pub trait BufferSource: Send + Sync {
    /// `count` buffers of at least `bytes` bytes each from the heap called `heap`, each its own
    /// descriptor that hardware and a consumer can take and that can be mapped. An error fails
    /// the attachment that asked: nothing else is tried in its place.
    fn allocate(&self, heap: &str, bytes: u64, count: u32) -> io::Result<Vec<OwnedFd>>;
}

/// The kernel's dma-heaps: a heap is the character device of its name in one directory.
pub struct DmaHeaps {
    dir: PathBuf,
}

impl DmaHeaps {
    /// Heaps under `dir`.
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

impl Default for DmaHeaps {
    /// Where Linux puts them.
    fn default() -> Self {
        Self::at("/dev/dma_heap")
    }
}

impl BufferSource for DmaHeaps {
    fn allocate(&self, heap: &str, bytes: u64, count: u32) -> io::Result<Vec<OwnedFd>> {
        // The board file's check, again: the name is one file in `dir`.
        if heap.is_empty() || heap == "." || heap == ".." || heap.contains('/') {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let heap = File::open(self.dir.join(heap))?;
        (0..count)
            .map(|_| dma_heap::allocate(heap.as_fd(), bytes))
            .collect()
    }
}

/// Allocates each stream: memfds, or dma-bufs from `source` when the board file says so.
pub(crate) struct ServiceStreams {
    pub(crate) source: Arc<dyn BufferSource>,
}

struct SharedRing(Mapping);

impl WordMemory for SharedRing {
    fn words(&self) -> &[AtomicU64] {
        // Made from a writable mapping of whole words, which is what makes this `Some`.
        self.0.as_atomic_words().unwrap_or(&[])
    }
}

struct SharedBuffers(Mapping);

impl BufferMemory for SharedBuffers {
    fn write(&self, offset: usize, data: &[u8]) -> bool {
        self.0.write_bytes(offset, data).is_some()
    }

    fn read(&self, offset: usize, out: &mut [u8]) -> bool {
        let source = offset
            .checked_add(out.len())
            .and_then(|end| self.0.as_slice().get(offset..end));
        match source {
            Some(from) => {
                out.copy_from_slice(from);
                true
            }
            None => false,
        }
    }
}

struct EventDoorbell(OwnedFd);

impl Doorbell for EventDoorbell {
    fn ring(&self) {
        eventfd::signal(self.0.as_fd());
    }
}

fn memory(name: &str, suffix: &str, len: u64) -> Result<(OwnedFd, Mapping), Status> {
    let label = CString::new(format!("lemnos-{name}-{suffix}")).map_err(|_| Status::IO)?;
    let fd = sealed_memfd(&label, len).map_err(|_| Status::IO)?;
    let bytes = usize::try_from(len).map_err(|_| Status::IO)?;
    let mapping = Mapping::new(fd.as_fd(), bytes, 0, true).map_err(|_| Status::IO)?;
    Ok((fd, mapping))
}

/// The buffers of a stream of dma-bufs, each plane of each mapped on its own. The host
/// addresses them as one pool, a buffer every `stride` bytes and a buffer's planes end to end,
/// which is turned back into planes and places in them.
struct HandleBuffers {
    /// One per plane of each buffer, buffer 0's planes first, as `handles`.
    maps: Vec<Mapping>,
    handles: Arc<[OwnedFd]>,
    stride: usize,
    planes: Planes,
    /// Whether the buffers are real dma-bufs, whose CPU access is bracketed for the kernel.
    /// Stand-ins in tests have no such call.
    sync: bool,
}

impl HandleBuffers {
    /// Calls `each` with every plane `len` bytes at `offset` of the pool cover, the place in
    /// that plane and how many of the bytes it holds, in order. `false`, and `each` never
    /// called, unless all of them lie within one buffer's planes.
    fn span(&self, offset: usize, len: usize, mut each: impl FnMut(usize, usize, usize)) -> bool {
        let buffer = offset / self.stride;
        let inner = offset % self.stride;
        let inside = inner
            .checked_add(len)
            .is_some_and(|end| end <= self.planes.lengths().iter().map(|l| *l as usize).sum());
        if !inside || buffer.saturating_add(1) * self.planes.count() as usize > self.maps.len() {
            return false;
        }
        let (mut at, mut left, mut done) = (inner, len, 0);
        while left > 0 {
            let Some((plane, place, room)) = self.planes.locate(at) else {
                return done == len;
            };
            let take = left.min(room);
            each(
                buffer * self.planes.count() as usize + plane as usize,
                place,
                take,
            );
            at += take;
            left -= take;
            done += take;
        }
        true
    }

    /// Tells the kernel the CPU is starting or ending an access (`DMA_BUF_IOCTL_SYNC`), so a
    /// device whose memory isn't cache-coherent sees what was written and the CPU sees what the
    /// device wrote. A failure changes nothing about where the bytes go, so it isn't one here.
    fn bracket(&self, index: usize, flags: u64) {
        if self.sync
            && let Some(handle) = self.handles.get(index)
        {
            let _ = dma_buf::sync(handle.as_fd(), flags);
        }
    }
}

impl BufferMemory for HandleBuffers {
    fn write(&self, offset: usize, data: &[u8]) -> bool {
        let mut from = 0;
        let mut written = true;
        let fits = self.span(offset, data.len(), |index, place, take| {
            let part = data.get(from..from + take).unwrap_or(&[]);
            from += take;
            self.bracket(index, SYNC_START | SYNC_WRITE);
            written &= self
                .maps
                .get(index)
                .and_then(|map| map.write_bytes(place, part))
                .is_some();
            self.bracket(index, SYNC_END | SYNC_WRITE);
        });
        fits && written
    }

    fn read(&self, offset: usize, out: &mut [u8]) -> bool {
        let mut to = 0;
        let mut read = true;
        let fits = self.span(offset, out.len(), |index, place, take| {
            let source = self
                .maps
                .get(index)
                .and_then(|map| map.as_slice().get(place..place + take));
            match (source, out.get_mut(to..to + take)) {
                (Some(source), Some(dest)) => {
                    self.bracket(index, SYNC_START | SYNC_READ);
                    dest.copy_from_slice(source);
                    self.bracket(index, SYNC_END | SYNC_READ);
                }
                _ => read = false,
            }
            to += take;
        });
        fits && read
    }
}

/// The ring and the doorbell, which every stream has: what the host uses, and the two
/// descriptors the consumer gets.
struct Common {
    ring: Arc<dyn WordMemory>,
    doorbell: Arc<dyn Doorbell>,
    ring_fd: OwnedFd,
    bell_for_client: OwnedFd,
}

fn common(name: &str, layout: &Layout) -> Result<Common, Status> {
    let ring_bytes = u64::try_from(layout.ring_bytes()).map_err(|_| Status::IO)?;
    let (ring_fd, ring) = memory(name, "ring", ring_bytes)?;
    if ring.as_atomic_words().is_none() {
        return Err(Status::IO);
    }
    let bell = eventfd::create().map_err(|_| Status::IO)?;
    let bell_for_client = bell.try_clone().map_err(|_| Status::IO)?;
    Ok(Common {
        ring: Arc::new(SharedRing(ring)),
        doorbell: Arc::new(EventDoorbell(bell)),
        ring_fd,
        bell_for_client,
    })
}

/// Why a heap gave nothing, as the status the consumer is refused with.
fn heap_status(error: &io::Error) -> Status {
    match error.kind() {
        io::ErrorKind::NotFound => Status::NOT_FOUND,
        io::ErrorKind::PermissionDenied => Status::ACCESS,
        io::ErrorKind::OutOfMemory => Status::NO_MEMORY,
        io::ErrorKind::InvalidInput => Status::INVALID_ARGUMENT,
        _ => Status::IO,
    }
}

impl StreamAllocator for ServiceStreams {
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Backing, Status> {
        let common = common(name, layout)?;
        let (buffers_fd, buffers) = memory(name, "buffers", layout.pool_bytes())?;
        Ok(Backing {
            ring: common.ring,
            buffers: Arc::new(SharedBuffers(buffers)),
            doorbell: common.doorbell,
            descriptors: vec![common.ring_fd, common.bell_for_client, buffers_fd],
            // One memfd for the whole pool: nothing here is a dma-buf, so no buffer has a handle.
            handles: None,
        })
    }

    fn allocate_handles(&self, name: &str, layout: &Layout, heap: &str) -> Result<Backing, Status> {
        self.allocate_planes(name, layout, heap, &Planes::one(layout.buffer_bytes))
    }

    fn allocate_planes(
        &self,
        name: &str,
        layout: &Layout,
        heap: &str,
        planes: &Planes,
    ) -> Result<Backing, Status> {
        // More could not be handed to the consumer in the one message the answer is.
        if handle_descriptors(layout.buffers, planes.count()).is_none() {
            return Err(Status::INVALID_ARGUMENT);
        }
        let mut handles = Vec::with_capacity(layout.buffers as usize * planes.count() as usize);
        let mut maps = Vec::with_capacity(handles.capacity());
        let mut sync = true;
        // A buffer of one plane keeps the whole stride, as it always had.
        let lengths: Vec<u64> = if planes.count() == 1 {
            vec![layout.stride()]
        } else {
            planes.lengths().iter().map(|l| u64::from(*l)).collect()
        };
        for length in &lengths {
            let made = self
                .source
                .allocate(heap, *length, layout.buffers)
                .map_err(|error| heap_status(&error))?;
            if made.len() != layout.buffers as usize {
                return Err(Status::IO);
            }
            handles.push(made);
        }
        // Buffer-major: buffer 0's planes, then buffer 1's.
        let mut ordered = Vec::with_capacity(maps.capacity());
        let mut per_plane: Vec<std::vec::IntoIter<OwnedFd>> =
            handles.into_iter().map(Vec::into_iter).collect();
        for _ in 0..layout.buffers {
            for (plane, length) in per_plane.iter_mut().zip(&lengths) {
                let handle = plane.next().ok_or(Status::IO)?;
                // A source is taken at its word about nothing: the buffer must really be this
                // long before it is mapped, or touching the mapping would fault.
                let long_enough = dma_buf::size(handle.as_fd()).is_ok_and(|size| size >= *length);
                if !long_enough {
                    return Err(Status::IO);
                }
                sync &= dma_buf::is_dma_buf(handle.as_fd()).unwrap_or(false);
                let mapped = usize::try_from(*length).map_err(|_| Status::IO)?;
                maps.push(Mapping::new(handle.as_fd(), mapped, 0, true).map_err(|_| Status::IO)?);
                ordered.push(handle);
            }
        }
        let handles: Arc<[OwnedFd]> = ordered.into();
        let common = common(name, layout)?;
        Ok(Backing {
            ring: common.ring,
            buffers: Arc::new(HandleBuffers {
                maps,
                handles: Arc::clone(&handles),
                stride: usize::try_from(layout.stride()).map_err(|_| Status::IO)?,
                planes: *planes,
                sync,
            }),
            doorbell: common.doorbell,
            // The buffers follow these two to the consumer, as `handles`.
            descriptors: vec![common.ring_fd, common.bell_for_client],
            handles: Some(handles),
        })
    }
}
