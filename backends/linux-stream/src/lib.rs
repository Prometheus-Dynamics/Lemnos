//! What a frame stream needs from Linux, for a program using `lemnos-client`.
//!
//! `lemnos-client` has no `unsafe` and doesn't know about this crate. It asks for a
//! [`StreamPlatform`], and the program passes [`LinuxStreams`]. All the unsafe is in
//! `lemnos-linux-sys`.

#![cfg(target_os = "linux")]

use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use lemnos_client::StreamPlatform;
use lemnos_linux_sys::poll::{POLLIN, poll_one};
use lemnos_linux_sys::udmabuf::{SEAL_SHRINK, seals};
use lemnos_linux_sys::{dma_buf, eventfd, mmap::Mapping, scm};
use lemnos_wire::ring::{ByteMemory, WordMemory};

/// Plain Linux: `SCM_RIGHTS`, `mmap` and eventfd.
#[derive(Debug, Clone, Copy, Default)]
pub struct LinuxStreams;

struct SharedRing(Mapping);

impl WordMemory for SharedRing {
    fn words(&self) -> &[AtomicU64] {
        // Checked when this was made. An empty slice would just read as too small anyway.
        self.0.as_atomic_words().unwrap_or(&[])
    }
}

struct SharedBuffers(Mapping);

impl ByteMemory for SharedBuffers {
    fn bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// The descriptor must hold exactly `bytes`, and be sealed so the sender can't shrink it after
/// this check: a mapping past the end of a shrunk file faults when touched.
fn exactly(fd: BorrowedFd<'_>, bytes: usize) -> io::Result<()> {
    if dma_buf::size(fd)? == bytes as u64 && seals(fd)? & SEAL_SHRINK != 0 {
        Ok(())
    } else {
        Err(io::ErrorKind::InvalidData.into())
    }
}

/// The descriptor must hold at least `bytes` and never be able to hold less: it is a dma-buf,
/// whose size is fixed, or it is sealed against shrinking.
fn at_least(fd: BorrowedFd<'_>, bytes: usize) -> io::Result<()> {
    let long_enough = dma_buf::size(fd)? >= bytes as u64;
    let fixed = dma_buf::is_dma_buf(fd)? || seals(fd).is_ok_and(|seals| seals & SEAL_SHRINK != 0);
    if long_enough && fixed {
        Ok(())
    } else {
        Err(io::ErrorKind::InvalidData.into())
    }
}

impl StreamPlatform for LinuxStreams {
    fn recv_with_descriptors(
        &self,
        socket: BorrowedFd<'_>,
        buf: &mut [u8],
        fds: &mut Vec<OwnedFd>,
    ) -> io::Result<usize> {
        scm::recv_with_fds(socket, buf, fds)
    }

    fn map_ring(&self, fd: BorrowedFd<'_>, bytes: usize) -> io::Result<Arc<dyn WordMemory>> {
        exactly(fd, bytes)?;
        let mapping = Mapping::new(fd, bytes, 0, true)?;
        if mapping.as_atomic_words().is_none() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(Arc::new(SharedRing(mapping)))
    }

    fn map_buffers(&self, fd: BorrowedFd<'_>, bytes: usize) -> io::Result<Box<dyn ByteMemory>> {
        exactly(fd, bytes)?;
        Ok(Box::new(SharedBuffers(Mapping::new(fd, bytes, 0, false)?)))
    }

    fn map_buffer(&self, fd: BorrowedFd<'_>, bytes: usize) -> io::Result<Box<dyn ByteMemory>> {
        at_least(fd, bytes)?;
        Ok(Box::new(SharedBuffers(Mapping::new(fd, bytes, 0, false)?)))
    }

    fn wait_doorbell(&self, fd: BorrowedFd<'_>, timeout: Duration) -> io::Result<bool> {
        let woken = poll_one(fd, POLLIN, Some(timeout))? != 0;
        if woken {
            eventfd::drain(fd);
        }
        Ok(woken)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    use lemnos_linux_sys::dma_buf::sealed_memfd;

    #[test]
    fn a_descriptor_of_the_wrong_length_is_refused_before_mapping() {
        let streams = LinuxStreams;
        let fd = sealed_memfd(c"short", 4096).unwrap();
        assert!(streams.map_ring(fd.as_fd(), 8192).is_err());
        assert!(streams.map_buffers(fd.as_fd(), 8192).is_err());
        assert!(streams.map_buffers(fd.as_fd(), 2048).is_err());
    }

    #[test]
    fn a_descriptor_that_could_still_shrink_is_refused() {
        let streams = LinuxStreams;
        let fd = dma_buf::memfd(c"unsealed").unwrap();
        std::fs::File::from(fd.try_clone().unwrap())
            .set_len(4096)
            .unwrap();
        assert!(streams.map_ring(fd.as_fd(), 4096).is_err());
        assert!(streams.map_buffers(fd.as_fd(), 4096).is_err());
    }

    #[test]
    fn a_ring_maps_as_words_and_buffers_map_read_only() {
        let streams = LinuxStreams;
        let fd = sealed_memfd(c"ok", 4096).unwrap();
        let ring = streams.map_ring(fd.as_fd(), 4096).unwrap();
        assert_eq!(ring.words().len(), 512);
        let buffers = streams.map_buffers(fd.as_fd(), 4096).unwrap();
        assert_eq!(buffers.bytes().len(), 4096);
    }

    #[test]
    fn one_buffer_maps_when_it_is_long_enough_and_cant_shrink() {
        let streams = LinuxStreams;
        // A heap rounds up, so a buffer may be longer than asked.
        let fd = sealed_memfd(c"buffer", 8192).unwrap();
        assert_eq!(
            streams.map_buffer(fd.as_fd(), 5000).unwrap().bytes().len(),
            5000
        );
        assert_eq!(
            streams.map_buffer(fd.as_fd(), 8192).unwrap().bytes().len(),
            8192
        );
        assert!(streams.map_buffer(fd.as_fd(), 8193).is_err());
        let unsealed = dma_buf::memfd(c"unsealed").unwrap();
        std::fs::File::from(unsealed.try_clone().unwrap())
            .set_len(8192)
            .unwrap();
        assert!(streams.map_buffer(unsealed.as_fd(), 4096).is_err());
    }

    /// Only with `LEMNOS_TEST_DMA_HEAP` naming a heap this process may open.
    #[test]
    fn a_real_dma_buf_maps_as_one_buffer_when_a_heap_is_named() {
        let Some(heap) = std::env::var_os("LEMNOS_TEST_DMA_HEAP") else {
            return;
        };
        let heap = std::fs::File::open(heap).unwrap();
        let buffer = lemnos_linux_sys::dma_heap::allocate(heap.as_fd(), 5000).unwrap();
        let mapped = LinuxStreams.map_buffer(buffer.as_fd(), 5000).unwrap();
        assert_eq!(mapped.bytes().len(), 5000);
        assert!(LinuxStreams.map_buffer(buffer.as_fd(), 1 << 20).is_err());
    }

    #[test]
    fn the_doorbell_wakes_and_times_out() {
        let streams = LinuxStreams;
        let bell = eventfd::create().unwrap();
        assert!(
            !streams
                .wait_doorbell(bell.as_fd(), Duration::from_millis(5))
                .unwrap()
        );
        eventfd::signal(bell.as_fd());
        assert!(
            streams
                .wait_doorbell(bell.as_fd(), Duration::from_secs(5))
                .unwrap()
        );
        assert!(
            !streams
                .wait_doorbell(bell.as_fd(), Duration::from_millis(5))
                .unwrap()
        );
    }
}
