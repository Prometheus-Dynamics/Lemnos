//! What a frame stream's buffers are on Linux: one sealed memfd for a stream of shared memory, or
//! dma-bufs from a dma-heap for a stream the board file declares as dma-bufs: one per buffer, or one
//! per plane of each buffer when the board file gives the buffers planes. The rings, the holds and
//! the doorbell are the shm family's and made by the host (`engine/host`), so the same for both.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;

use lemnos_core::Status;
use lemnos_host::stream::{Access, Buffers, DmaSync, StreamAllocator};
use lemnos_linux_sys::dma_buf::{self, SYNC_END, SYNC_READ, SYNC_START, SYNC_WRITE, sealed_memfd};
use lemnos_linux_sys::dma_heap;
use lemnos_wire::Planes;
use lemnos_wire::handle_descriptors;
use lemnos_wire::ring::Layout;

/// Where the buffers of a stream of dma-bufs come from.
pub trait BufferSource: Send + Sync {
    /// `count` buffers of at least `bytes` bytes each from the heap called `heap`, each its own
    /// descriptor that hardware and a consumer can take and that can be mapped. An error fails
    /// the attachment that asked: nothing else is tried in its place.
    fn allocate(&self, heap: &str, bytes: u64, count: u32) -> io::Result<Vec<OwnedFd>>;

    /// As [`allocate`](Self::allocate), but with `read_only` each descriptor is a read-only file:
    /// the buffers are for a stream that has reader slots. A source that can't make read-only
    /// buffers keeps this default, which gives writable ones; a ticket on such a stream is then
    /// refused (the host checks each descriptor's access mode).
    fn allocate_for(
        &self,
        heap: &str,
        bytes: u64,
        count: u32,
        read_only: bool,
    ) -> io::Result<Vec<OwnedFd>> {
        let _ = read_only;
        self.allocate(heap, bytes, count)
    }
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
        self.allocate_for(heap, bytes, count, false)
    }

    fn allocate_for(
        &self,
        heap: &str,
        bytes: u64,
        count: u32,
        read_only: bool,
    ) -> io::Result<Vec<OwnedFd>> {
        // The board file's check, again: the name is one file in `dir`.
        if heap.is_empty() || heap == "." || heap == ".." || heap.contains('/') {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let heap = File::open(self.dir.join(heap))?;
        (0..count)
            .map(|_| dma_heap::allocate_with(heap.as_fd(), bytes, read_only))
            .collect()
    }
}

/// Allocates each stream's buffers: a memfd, or dma-bufs from `source` when the board file says so.
pub(crate) struct ServiceStreams {
    pub(crate) source: Arc<dyn BufferSource>,
}

/// Brackets the host's CPU accesses to a stream's dma-bufs (`DMA_BUF_IOCTL_SYNC`), so a device
/// whose memory isn't cache-coherent sees what was written and the CPU sees what the device wrote.
/// Off for stand-ins that are not dma-bufs, which have no such call.
struct HandleSync {
    handles: Arc<[OwnedFd]>,
    enabled: bool,
}

impl HandleSync {
    fn bracket(&self, index: usize, flags: u64) {
        if self.enabled
            && let Some(handle) = self.handles.get(index)
        {
            let _ = dma_buf::sync(handle.as_fd(), flags);
        }
    }
}

impl DmaSync for HandleSync {
    fn begin(&self, index: usize, access: Access) {
        self.bracket(index, SYNC_START | flag(access));
    }

    fn end(&self, index: usize, access: Access) {
        self.bracket(index, SYNC_END | flag(access));
    }
}

const fn flag(access: Access) -> u64 {
    match access {
        Access::Read => SYNC_READ,
        Access::Write => SYNC_WRITE,
    }
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
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Buffers, Status> {
        let label = CString::new(format!("lemnos-{name}-buffers")).map_err(|_| Status::IO)?;
        let len = layout.pool_bytes();
        let fd = sealed_memfd(&label, len).map_err(|_| Status::IO)?;
        Ok(Buffers {
            fds: vec![fd],
            dma: false,
            sync: None,
        })
    }

    fn allocate_planes(
        &self,
        name: &str,
        layout: &Layout,
        heap: &str,
        planes: &Planes,
    ) -> Result<Buffers, Status> {
        self.allocate_planes_for(name, layout, heap, planes, false, 0)
    }

    fn allocate_planes_for(
        &self,
        name: &str,
        layout: &Layout,
        heap: &str,
        planes: &Planes,
        read_only: bool,
        tail: u32,
    ) -> Result<Buffers, Status> {
        // More could not be handed to the consumer in the one message the answer is.
        if handle_descriptors(layout.buffers, planes.count()).is_none() {
            return Err(Status::INVALID_ARGUMENT);
        }
        // A buffer of one plane keeps the whole stride, as it always had.
        let lengths: Vec<u64> = if planes.count() == 1 {
            vec![layout.stride()]
        } else {
            planes.lengths().iter().map(|l| u64::from(*l)).collect()
        };
        let mut per_plane: Vec<std::vec::IntoIter<OwnedFd>> = Vec::with_capacity(lengths.len());
        let mut sync = true;
        for length in &lengths {
            // The tail is slack past the plane, for an importer that reads a little beyond it.
            let made = self
                .source
                .allocate_for(heap, *length + u64::from(tail), layout.buffers, read_only)
                .map_err(|error| heap_status(&error))?;
            if made.len() != layout.buffers as usize {
                return Err(Status::IO);
            }
            for handle in &made {
                // A source is taken at its word about nothing: the buffer must really be this
                // long before anything maps or touches it.
                let long_enough = dma_buf::size(handle.as_fd()).is_ok_and(|size| size >= *length);
                if !long_enough {
                    return Err(Status::IO);
                }
                sync &= dma_buf::is_dma_buf(handle.as_fd()).unwrap_or(false);
            }
            per_plane.push(made.into_iter());
        }
        // Buffer-major, as the host indexes the pool: buffer 0's planes, then buffer 1's.
        let mut ordered = Vec::with_capacity(layout.buffers as usize * lengths.len());
        for _ in 0..layout.buffers {
            for plane in &mut per_plane {
                ordered.push(plane.next().ok_or(Status::IO)?);
            }
        }
        let copies = ordered
            .iter()
            .map(|fd| fd.try_clone().map_err(|_| Status::IO))
            .collect::<Result<Vec<_>, _>>()?;
        let _ = name;
        Ok(Buffers {
            fds: ordered,
            dma: true,
            sync: Some(Arc::new(HandleSync {
                handles: Arc::from(copies),
                enabled: sync,
            })),
        })
    }
}
