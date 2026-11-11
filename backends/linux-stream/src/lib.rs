//! What a frame stream needs from Linux, for a program using `lemnos-client`.
//!
//! `lemnos-client` has no `unsafe` and doesn't know about this crate. It asks for a
//! [`StreamPlatform`], and the program passes [`LinuxStreams`]. All the unsafe is in
//! `lemnos-linux-sys`.

#![cfg(target_os = "linux")]

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use lemnos_client::StreamPlatform;
use lemnos_linux_sys::dma_buf::{SYNC_END, SYNC_READ, SYNC_START, SYNC_WRITE};
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

/// Tells the kernel the processor starts or stops touching a buffer, with the flags of
/// `DMA_BUF_IOCTL_SYNC`.
type Sync = Box<dyn Fn(u64) + Send + std::marker::Sync>;

/// The one mapping of a stream of shared memory, or one buffer of a stream of dma-bufs.
struct SharedBuffers {
    map: Mapping,
    /// Whether the mapping can be written.
    writable: bool,
    /// Set for a dma-buf, which a device shares: it holds the buffer's descriptor. Other memory
    /// has no such call.
    synced: Option<Sync>,
}

impl SharedBuffers {
    fn sync(&self, flags: u64) {
        if let Some(sync) = &self.synced {
            sync(flags);
        }
    }
}

impl ByteMemory for SharedBuffers {
    fn bytes(&self) -> &[u8] {
        self.map.as_slice()
    }

    fn write(&self, offset: usize, data: &[u8]) -> bool {
        if !self.writable {
            return false;
        }
        self.sync(SYNC_START | SYNC_WRITE);
        let written = self.map.write_bytes(offset, data).is_some();
        self.sync(SYNC_END | SYNC_WRITE);
        written
    }

    fn begin_read(&self) {
        self.sync(SYNC_START | SYNC_READ);
    }

    fn end_read(&self) {
        self.sync(SYNC_END | SYNC_READ);
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

    fn map_buffers(
        &self,
        fd: BorrowedFd<'_>,
        bytes: usize,
        writable: bool,
    ) -> io::Result<Box<dyn ByteMemory>> {
        exactly(fd, bytes)?;
        Ok(Box::new(SharedBuffers {
            map: Mapping::new(fd, bytes, 0, writable)?,
            writable,
            synced: None,
        }))
    }

    fn map_buffer(
        &self,
        fd: BorrowedFd<'_>,
        bytes: usize,
        writable: bool,
    ) -> io::Result<Box<dyn ByteMemory>> {
        at_least(fd, bytes)?;
        // A stand-in for a dma-buf (a sealed memfd) takes no such call.
        let synced = if dma_buf::is_dma_buf(fd)? {
            let held = fd.try_clone_to_owned()?;
            // A failure changes nothing about where the bytes are, so it isn't one here.
            let sync: Sync = Box::new(move |flags| drop(dma_buf::sync(held.as_fd(), flags)));
            Some(sync)
        } else {
            None
        };
        Ok(Box::new(SharedBuffers {
            map: Mapping::new(fd, bytes, 0, writable)?,
            writable,
            synced,
        }))
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

    use lemnos_linux_sys::dma_buf::sealed_memfd;

    #[test]
    fn a_descriptor_of_the_wrong_length_is_refused_before_mapping() {
        let streams = LinuxStreams;
        let fd = sealed_memfd(c"short", 4096).unwrap();
        assert!(streams.map_ring(fd.as_fd(), 8192).is_err());
        assert!(streams.map_buffers(fd.as_fd(), 8192, false).is_err());
        assert!(streams.map_buffers(fd.as_fd(), 2048, false).is_err());
    }

    #[test]
    fn a_descriptor_that_could_still_shrink_is_refused() {
        let streams = LinuxStreams;
        let fd = dma_buf::memfd(c"unsealed").unwrap();
        std::fs::File::from(fd.try_clone().unwrap())
            .set_len(4096)
            .unwrap();
        assert!(streams.map_ring(fd.as_fd(), 4096).is_err());
        assert!(streams.map_buffers(fd.as_fd(), 4096, true).is_err());
    }

    #[test]
    fn a_ring_maps_as_words_and_buffers_map_read_only() {
        let streams = LinuxStreams;
        let fd = sealed_memfd(c"ok", 4096).unwrap();
        let ring = streams.map_ring(fd.as_fd(), 4096).unwrap();
        assert_eq!(ring.words().len(), 512);
        let buffers = streams.map_buffers(fd.as_fd(), 4096, false).unwrap();
        assert_eq!(buffers.bytes().len(), 4096);
        assert!(!buffers.write(0, &[1]), "mapped to be read");
        assert_eq!(buffers.bytes()[0], 0);
    }

    /// What the other side of `fd` sees at `offset`.
    fn seen(fd: &OwnedFd, offset: u64, len: usize) -> Vec<u8> {
        use std::os::unix::fs::FileExt;
        let mut out = vec![0; len];
        std::fs::File::from(fd.try_clone().unwrap())
            .read_exact_at(&mut out, offset)
            .unwrap();
        out
    }

    #[test]
    fn buffers_mapped_to_be_filled_take_writes_inside_them_and_the_other_side_sees_them() {
        let streams = LinuxStreams;
        let fd = sealed_memfd(c"filled", 8192).unwrap();
        let pool = streams.map_buffers(fd.as_fd(), 8192, true).unwrap();
        assert!(pool.write(4096, &[1, 2, 3]));
        assert_eq!(seen(&fd, 4096, 3), [1, 2, 3]);
        assert_eq!(&pool.bytes()[4096..4099], [1, 2, 3]);
        assert!(!pool.write(8190, &[0; 3]), "past the end");
        assert!(!pool.write(usize::MAX, &[0; 2]));
        assert_eq!(seen(&fd, 8190, 2), [0, 0], "nothing of it landed");

        let one = streams.map_buffer(fd.as_fd(), 5000, true).unwrap();
        assert!(one.write(4997, &[7, 8, 9]));
        assert_eq!(seen(&fd, 4997, 3), [7, 8, 9]);
        assert!(!one.write(4998, &[0; 3]), "past the buffer's own bytes");
        let read_only = streams.map_buffer(fd.as_fd(), 5000, false).unwrap();
        assert!(!read_only.write(0, &[1]));
    }

    #[test]
    fn a_dma_bufs_accesses_are_bracketed_for_the_kernel() {
        use std::sync::Mutex;
        let fd = sealed_memfd(c"synced", 4096).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&calls);
        let buffer = SharedBuffers {
            map: Mapping::new(fd.as_fd(), 4096, 0, true).unwrap(),
            writable: true,
            synced: Some(Box::new(move |flags| log.lock().unwrap().push(flags))),
        };
        let taken = || std::mem::take(&mut *calls.lock().unwrap());
        assert!(buffer.write(8, &[5; 4]));
        assert_eq!(taken(), [SYNC_START | SYNC_WRITE, SYNC_END | SYNC_WRITE]);
        // A refused write is still a matched pair.
        assert!(!buffer.write(4095, &[5; 4]));
        assert_eq!(taken(), [SYNC_START | SYNC_WRITE, SYNC_END | SYNC_WRITE]);
        buffer.begin_read();
        assert_eq!(buffer.bytes()[8], 5);
        buffer.end_read();
        assert_eq!(taken(), [SYNC_START | SYNC_READ, SYNC_END | SYNC_READ]);
        // Memory that is no dma-buf has no such call: a stand-in maps with none.
        let plain = LinuxStreams.map_buffer(fd.as_fd(), 4096, true).unwrap();
        assert!(plain.write(0, &[1]));
        plain.begin_read();
        plain.end_read();
        assert!(taken().is_empty());
    }

    #[test]
    fn one_buffer_maps_when_it_is_long_enough_and_cant_shrink() {
        let streams = LinuxStreams;
        // A heap rounds up, so a buffer may be longer than asked.
        let fd = sealed_memfd(c"buffer", 8192).unwrap();
        assert_eq!(
            streams
                .map_buffer(fd.as_fd(), 5000, false)
                .unwrap()
                .bytes()
                .len(),
            5000
        );
        assert_eq!(
            streams
                .map_buffer(fd.as_fd(), 8192, false)
                .unwrap()
                .bytes()
                .len(),
            8192
        );
        assert!(streams.map_buffer(fd.as_fd(), 8193, true).is_err());
        let unsealed = dma_buf::memfd(c"unsealed").unwrap();
        std::fs::File::from(unsealed.try_clone().unwrap())
            .set_len(8192)
            .unwrap();
        assert!(streams.map_buffer(unsealed.as_fd(), 4096, false).is_err());
    }

    /// Only with `LEMNOS_TEST_DMA_HEAP` naming a heap this process may open.
    #[test]
    fn a_real_dma_buf_maps_as_one_buffer_when_a_heap_is_named() {
        let Some(heap) = std::env::var_os("LEMNOS_TEST_DMA_HEAP") else {
            return;
        };
        let heap = std::fs::File::open(heap).unwrap();
        let buffer = lemnos_linux_sys::dma_heap::allocate(heap.as_fd(), 5000).unwrap();
        let mapped = LinuxStreams.map_buffer(buffer.as_fd(), 5000, true).unwrap();
        assert_eq!(mapped.bytes().len(), 5000);
        // The real ioctl, around a write and around a read in place.
        assert!(mapped.write(4990, &[9; 10]));
        mapped.begin_read();
        assert_eq!(&mapped.bytes()[4990..], [9; 10]);
        mapped.end_read();
        assert!(
            LinuxStreams
                .map_buffer(buffer.as_fd(), 1 << 20, false)
                .is_err()
        );
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
