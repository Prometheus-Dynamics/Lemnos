//! What a frame stream is backed by on Linux: sealed memory the client can map but can't resize,
//! and an eventfd.

use std::ffi::CString;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use lemnos_core::Status;
use lemnos_host::stream::{Backing, BufferMemory, Doorbell, StreamAllocator};
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_linux_sys::eventfd;
use lemnos_linux_sys::mmap::Mapping;
use lemnos_wire::ring::{Layout, WordMemory};

/// Allocates each stream from memfds.
pub(crate) struct MemfdStreams;

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

impl StreamAllocator for MemfdStreams {
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Backing, Status> {
        let ring_bytes = u64::try_from(layout.ring_bytes()).map_err(|_| Status::IO)?;
        let (ring_fd, ring) = memory(name, "ring", ring_bytes)?;
        if ring.as_atomic_words().is_none() {
            return Err(Status::IO);
        }
        let (buffers_fd, buffers) = memory(name, "buffers", layout.pool_bytes())?;
        let bell = eventfd::create().map_err(|_| Status::IO)?;
        let bell_for_client = bell.try_clone().map_err(|_| Status::IO)?;
        Ok(Backing {
            ring: Arc::new(SharedRing(ring)),
            buffers: Arc::new(SharedBuffers(buffers)),
            doorbell: Arc::new(EventDoorbell(bell)),
            descriptors: vec![ring_fd, bell_for_client, buffers_fd],
            // One memfd for the whole pool: nothing here is a dma-buf, so no buffer has a handle.
            handles: None,
        })
    }
}
