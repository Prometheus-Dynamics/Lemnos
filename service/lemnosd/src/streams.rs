//! What a frame stream is backed by on Linux: sealed memory the client can map but can't resize,
//! and an eventfd. A stream the board file declares as dma-bufs gets one dma-buf per buffer from
//! a dma-heap in place of the one memfd for all of them, so a driver can hand a buffer to
//! hardware; the ring and the doorbell are the same.

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
use lemnos_wire::MAX_HANDLE_BUFFERS;
use lemnos_wire::ring::{Layout, WordMemory};

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

/// The buffers of a stream of dma-bufs, each mapped on its own. The host addresses them as one
/// pool, a buffer every `stride` bytes, which is turned back into a buffer and a place in it.
struct HandleBuffers {
    maps: Vec<Mapping>,
    handles: Arc<[OwnedFd]>,
    stride: usize,
    /// How much of each buffer is the stream's: the layout's `buffer_bytes`.
    bytes: usize,
    /// Whether the buffers are real dma-bufs, whose CPU access is bracketed for the kernel.
    /// Stand-ins in tests have no such call.
    sync: bool,
}

impl HandleBuffers {
    /// The buffer and the place in it that `len` bytes at `offset` of the pool are, when they
    /// lie within one buffer's own bytes.
    fn at(&self, offset: usize, len: usize) -> Option<(usize, usize)> {
        let inner = offset % self.stride;
        let fits = inner.checked_add(len).is_some_and(|end| end <= self.bytes);
        fits.then_some((offset / self.stride, inner))
    }

    /// Tells the kernel the CPU is starting or ending an access (`DMA_BUF_IOCTL_SYNC`), so a
    /// device whose memory isn't cache-coherent sees what was written and the CPU sees what the
    /// device wrote. A failure changes nothing about where the bytes go, so it isn't one here.
    fn bracket(&self, buffer: usize, flags: u64) {
        if self.sync
            && let Some(handle) = self.handles.get(buffer)
        {
            let _ = dma_buf::sync(handle.as_fd(), flags);
        }
    }
}

impl BufferMemory for HandleBuffers {
    fn write(&self, offset: usize, data: &[u8]) -> bool {
        let Some((buffer, inner)) = self.at(offset, data.len()) else {
            return false;
        };
        let Some(map) = self.maps.get(buffer) else {
            return false;
        };
        self.bracket(buffer, SYNC_START | SYNC_WRITE);
        let written = map.write_bytes(inner, data).is_some();
        self.bracket(buffer, SYNC_END | SYNC_WRITE);
        written
    }

    fn read(&self, offset: usize, out: &mut [u8]) -> bool {
        let Some((buffer, inner)) = self.at(offset, out.len()) else {
            return false;
        };
        let from = self.maps.get(buffer).and_then(|map| {
            let end = inner.checked_add(out.len())?;
            map.as_slice().get(inner..end)
        });
        let Some(from) = from else {
            return false;
        };
        self.bracket(buffer, SYNC_START | SYNC_READ);
        out.copy_from_slice(from);
        self.bracket(buffer, SYNC_END | SYNC_READ);
        true
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
        // More could not be handed to the consumer in the one message the answer is.
        if layout.buffers > MAX_HANDLE_BUFFERS {
            return Err(Status::INVALID_ARGUMENT);
        }
        let stride = layout.stride();
        let handles = self
            .source
            .allocate(heap, stride, layout.buffers)
            .map_err(|error| heap_status(&error))?;
        if handles.len() != layout.buffers as usize {
            return Err(Status::IO);
        }
        let mapped = usize::try_from(stride).map_err(|_| Status::IO)?;
        let mut maps = Vec::with_capacity(handles.len());
        let mut sync = true;
        for handle in &handles {
            // A source is taken at its word about nothing: the buffer must really be this long
            // before it is mapped, or touching the mapping would fault.
            let long_enough = dma_buf::size(handle.as_fd()).is_ok_and(|size| size >= stride);
            if !long_enough {
                return Err(Status::IO);
            }
            sync &= dma_buf::is_dma_buf(handle.as_fd()).unwrap_or(false);
            maps.push(Mapping::new(handle.as_fd(), mapped, 0, true).map_err(|_| Status::IO)?);
        }
        let handles: Arc<[OwnedFd]> = handles.into();
        let common = common(name, layout)?;
        Ok(Backing {
            ring: common.ring,
            buffers: Arc::new(HandleBuffers {
                maps,
                handles: Arc::clone(&handles),
                stride: mapped,
                bytes: layout.buffer_bytes as usize,
                sync,
            }),
            doorbell: common.doorbell,
            // The buffers follow these two to the consumer, as `handles`.
            descriptors: vec![common.ring_fd, common.bell_for_client],
            handles: Some(handles),
        })
    }
}
