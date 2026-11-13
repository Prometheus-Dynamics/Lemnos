//! What the block drivers' tests share: a caller of `codec.block/1` on a real host's job
//! stream, and sealed memfds standing in for the stream's dma-bufs. Test support only: never
//! use it in a shipped binary.

#![allow(clippy::missing_panics_doc)]

use std::os::fd::OwnedFd;
use std::sync::Arc;

use lemnos_contract_codec_block::work::Builder;
use lemnos_contract_codec_block::{
    BufferRef, COMPLETION_BYTES, Completion, DATA_OFFSET, JOB_BYTES, Job,
};
use lemnos_core::{CallerId, Decode, Encode, Reader, Status, Writer};
use lemnos_host::Host;
use lemnos_host::stream::{Buffers, Lease, MemfdAllocator, StreamAllocator};
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_wire::Planes;
use lemnos_wire::ring::{Consumer, Layout};

/// A stream allocator that makes each buffer, or each plane of each buffer, its own sealed
/// memfd, lent as its handle: what a stream of dma-bufs looks like to a driver. A shared stream
/// is one memfd, as [`MemfdAllocator`] makes it.
pub struct PerBuffer;

impl StreamAllocator for PerBuffer {
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Buffers, Status> {
        MemfdAllocator.allocate(name, layout)
    }

    fn allocate_planes(
        &self,
        name: &str,
        layout: &Layout,
        heap: &str,
        planes: &Planes,
    ) -> Result<Buffers, Status> {
        let _ = (name, heap);
        let mut fds: Vec<OwnedFd> = Vec::new();
        for _ in 0..layout.buffers {
            for length in planes.lengths() {
                // A buffer of one plane keeps the whole stride, as a heap rounds up.
                let made = if planes.count() == 1 {
                    layout.stride()
                } else {
                    u64::from(*length)
                };
                fds.push(sealed_memfd(c"codec-jobs-test", made).map_err(|_| Status::IO)?);
            }
        }
        Ok(Buffers {
            fds,
            dma: true,
            sync: None,
        })
    }
}

/// A consumer of a device's job stream, as a client of the service is one.
pub struct Caller {
    layout: Layout,
    handles: Option<Arc<[OwnedFd]>>,
    ring: Consumer,
}

impl Caller {
    /// Attaches to stream `name` of device `device` as caller `caller`. The buffers are mapped
    /// writable, so the caller can fill its work buffers.
    pub fn attach(host: &Host, device: u32, name: &str, caller: u64) -> Self {
        let lease: Lease = host.stream_attach(device, name, CallerId(caller)).unwrap();
        let layout = lease.layout;
        let handles = lease.handles.clone();
        let ring = lease.consumer_writable().unwrap();
        Self {
            layout,
            handles,
            ring,
        }
    }

    /// Writes `data` into `buffer` at `offset`.
    pub fn fill(&mut self, buffer: u32, offset: usize, data: &[u8]) {
        assert!(self.ring.write(buffer, offset, data));
    }

    /// Reads `len` bytes of `buffer` from `offset`.
    pub fn read(&self, buffer: u32, offset: usize, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        assert!(self.ring.read(buffer, offset, &mut out));
        out
    }

    /// The dma-buf stand-in behind `buffer`, as the stream lends it.
    pub fn handle(&self, buffer: u32) -> &OwnedFd {
        &self.handles.as_ref().expect("a stream of handles")[buffer as usize]
    }

    /// Posts raw bytes on the job queue.
    pub fn post_bytes(&mut self, bytes: &[u8]) {
        assert!(self.ring.jobs().post(bytes).unwrap());
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
        let mut bytes = vec![0u8; self.layout.buffer_bytes as usize];
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
        assert!(self.ring.release(frame.buffer));
        Some((frame.identity, record, data))
    }

    /// The ring's state, as the consumer sees it.
    pub fn state(&self) -> u32 {
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
