//! What the block drivers' tests share: a caller of `codec.block/1` on a real host's job
//! stream, and sealed memfds standing in for the stream's dma-bufs. Test support only: never
//! use it in a shipped binary.

#![allow(clippy::missing_panics_doc)]

use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;

use lemnos_contract_codec_block::work::Builder;
use lemnos_contract_codec_block::{
    BufferRef, COMPLETION_BYTES, Completion, DATA_OFFSET, JOB_BYTES, Job,
};
use lemnos_core::{CallerId, Decode, Encode, Reader, Status, Writer};
use lemnos_host::Host;
use lemnos_host::stream::{Backing, BufferMemory, HeapAllocator, Lease, StreamAllocator};
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_linux_sys::mmap::Mapping;
use lemnos_wire::Planes;
use lemnos_wire::ring::{Consumer, Layout};

/// The pool as the host addresses it, over one mapping per plane of each buffer.
struct Mapped {
    buffers: Vec<Mapping>,
    stride: usize,
    planes: Planes,
}

impl Mapped {
    /// The mapping and place that `len` bytes at pool offset `offset` are, when they lie in
    /// one plane.
    fn at(&self, offset: usize, len: usize) -> Option<(&Mapping, usize)> {
        let (plane, inner, room) = self.planes.locate(offset % self.stride)?;
        let index = offset / self.stride * self.planes.count() as usize + plane as usize;
        let map = self.buffers.get(index)?;
        (len <= room).then_some((map, inner))
    }
}

impl BufferMemory for Mapped {
    fn write(&self, offset: usize, data: &[u8]) -> bool {
        self.at(offset, data.len())
            .is_some_and(|(map, inner)| map.write_bytes(inner, data).is_some())
    }

    fn read(&self, offset: usize, out: &mut [u8]) -> bool {
        let Some((map, inner)) = self.at(offset, out.len()) else {
            return false;
        };
        match map.as_slice().get(inner..inner + out.len()) {
            Some(from) => {
                out.copy_from_slice(from);
                true
            }
            None => false,
        }
    }
}

/// A stream allocator that makes each buffer, or each plane of each buffer, its own sealed
/// memfd, mapped for the host and lent as its handle: what a stream of dma-bufs looks like to
/// a driver.
pub struct PerBuffer;

impl StreamAllocator for PerBuffer {
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Backing, Status> {
        self.allocate_planes(name, layout, "", &Planes::one(layout.buffer_bytes))
    }

    fn allocate_handles(
        &self,
        name: &str,
        layout: &Layout,
        _heap: &str,
    ) -> Result<Backing, Status> {
        self.allocate(name, layout)
    }

    fn allocate_planes(
        &self,
        name: &str,
        layout: &Layout,
        _heap: &str,
        planes: &Planes,
    ) -> Result<Backing, Status> {
        let plain = HeapAllocator.allocate(name, layout)?;
        let mut handles = Vec::new();
        let mut buffers = Vec::new();
        for _ in 0..layout.buffers {
            for length in planes.lengths() {
                // A buffer of one plane keeps the whole stride, as a heap rounds up.
                let made = if planes.count() == 1 {
                    layout.stride()
                } else {
                    u64::from(*length)
                };
                let fd: OwnedFd = sealed_memfd(c"codec-jobs-test", made).map_err(|_| Status::IO)?;
                buffers.push(
                    Mapping::new(fd.as_fd(), *length as usize, 0, true).map_err(|_| Status::IO)?,
                );
                handles.push(fd);
            }
        }
        Ok(Backing {
            buffers: Arc::new(Mapped {
                buffers,
                stride: layout.stride() as usize,
                planes: *planes,
            }),
            handles: Some(handles.into()),
            ..plain
        })
    }
}

/// A consumer of a device's job stream, as a client of the service is one.
pub struct Caller {
    lease: Lease,
    ring: Consumer,
}

impl Caller {
    /// Attaches to stream `name` of device `device` as caller `caller`.
    pub fn attach(host: &Host, device: u32, name: &str, caller: u64) -> Self {
        let lease = host.stream_attach(device, name, CallerId(caller)).unwrap();
        let ring = Consumer::attach(Arc::clone(&lease.ring), lease.layout).unwrap();
        Self { lease, ring }
    }

    fn at(&self, buffer: u32, offset: usize) -> usize {
        self.lease.layout.buffer_offset(buffer).unwrap() + offset
    }

    /// Writes `data` into `buffer` at `offset`.
    pub fn fill(&self, buffer: u32, offset: usize, data: &[u8]) {
        assert!(self.lease.buffers.write(self.at(buffer, offset), data));
    }

    /// Reads `len` bytes of `buffer` from `offset`.
    pub fn read(&self, buffer: u32, offset: usize, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        assert!(self.lease.buffers.read(self.at(buffer, offset), &mut out));
        out
    }

    /// The dma-buf stand-in behind `buffer`, as the stream lends it.
    pub fn handle(&self, buffer: u32) -> &OwnedFd {
        &self.lease.handles.as_ref().expect("a stream of handles")[buffer as usize]
    }

    /// Posts raw bytes on the job queue.
    pub fn post_bytes(&mut self, bytes: &[u8]) {
        assert!(self.ring.push_job(bytes).unwrap());
    }

    /// Writes work made of `refs` and `items` into buffer `work` and posts a job for it that
    /// completes on buffer `result`.
    pub fn post(
        &mut self,
        identity: u64,
        work: u32,
        result: u32,
        refs: &[BufferRef],
        items: &[(u32, &[u8])],
    ) {
        let mut bytes = vec![0u8; self.lease.layout.buffer_bytes as usize];
        let mut builder = Builder::new(&mut bytes).unwrap();
        for reference in refs {
            builder.reference(*reference).unwrap();
        }
        for (tag, data) in items {
            builder.item(*tag, data).unwrap();
        }
        let len = builder.finish();
        self.fill(work, 0, &bytes[..len]);
        let job = Job {
            identity,
            work,
            work_bytes: len as u32,
            result,
        };
        let mut encoded = [0u8; JOB_BYTES];
        let mut writer = Writer::new(&mut encoded);
        job.encode(&mut writer).unwrap();
        self.post_bytes(&encoded);
    }

    /// The next completion, if one has come: the job's identity, its record and its
    /// read-back. The result buffer goes back to the stream.
    pub fn completion(&mut self) -> Option<(u64, Completion, Vec<u8>)> {
        let frame = self.ring.pop_frame().unwrap()?;
        let record = self.read(frame.buffer, 0, COMPLETION_BYTES);
        let record = Completion::decode(&mut Reader::new(&record)).unwrap();
        assert_eq!(frame.bytes as usize, DATA_OFFSET + record.count as usize);
        let data = self.read(frame.buffer, DATA_OFFSET, record.count as usize);
        assert!(self.ring.release(frame.buffer, frame.fence).unwrap());
        Some((frame.identity, record, data))
    }

    /// The ring's state, as the consumer sees it.
    pub fn state(&self) -> u64 {
        self.ring.state()
    }
}

/// A buffer reference.
#[must_use]
pub fn reference(slot: u32, buffer: u32, offset: u32, length: u32) -> BufferRef {
    BufferRef {
        slot,
        buffer,
        offset,
        length,
    }
}
