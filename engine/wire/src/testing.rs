//! An in-process producer for tests: the host's end of a stream, made over the same shm objects a
//! host makes, so a test can play the driver against a real [`Consumer`] without a host.
//!
//! Planes are not modelled here: a harness stream has one plane per buffer. A stream of planes is
//! tested through the host.

use std::os::fd::OwnedFd;

use shm_ring::{FieldId, ReadMap, Ring, SampleTime, WriteMap, read_only, sealed_memfd};
use shm_ring_buffers::{BufferDesc, BufferIndex, BufferWriter, MAX_PER_FRAME, Pool};
use shm_ring_doorbell::{Bell, Doorbell};
use shm_ring_holds::{HoldTable, Limits, Marks};

use crate::ring::{Consumer, FrameInfo, JOB_ACK_BYTES, JobCollector, Layout, RingError, fds};

/// The host's end of one attachment, in this process.
pub struct Harness {
    layout: Layout,
    ring: Ring,
    publisher: BufferWriter<WriteMap>,
    jobs: JobCollector,
    holds: HoldTable<ReadMap>,
    bell: Bell,
}

impl core::fmt::Debug for Harness {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Harness")
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}

impl Harness {
    /// A stream of `layout`, one plane per buffer, shared memory when `shared` (one pool
    /// descriptor) and dma-buf-like one descriptor per buffer otherwise, with the consumer's end
    /// attached over it, writable when `writable`.
    ///
    /// # Errors
    ///
    /// [`RingError`] if the layout is out of range or the shared memory can't be made.
    pub fn new(
        layout: Layout,
        shared: bool,
        writable: bool,
    ) -> Result<(Self, Consumer), RingError> {
        let layout = layout.checked()?;
        let count = layout.buffers as usize;
        let (pool_fds, descs): (Vec<OwnedFd>, Vec<BufferDesc>) = if shared {
            let bytes = usize::try_from(layout.pool_bytes()).map_err(|_| RingError::BadLayout)?;
            (vec![sealed_memfd(bytes)?], layout.pool_descs(true, &[]))
        } else {
            let mut fds = Vec::with_capacity(count);
            for _ in 0..count {
                fds.push(sealed_memfd(layout.stride() as usize)?);
            }
            (fds, layout.pool_descs(false, &[]))
        };
        let copies = pool_fds
            .iter()
            .map(|fd| fd.try_clone().map_err(|e| RingError::Shm(e.to_string())))
            .collect::<Result<Vec<_>, _>>()?;
        let pool = Pool::mapped(1, copies, descs).map_err(RingError::from)?;

        let ring = Ring::create(&layout.frame_spec())?;
        let jobs = Ring::create(&layout.job_spec())?;
        let acks = sealed_memfd(JOB_ACK_BYTES)?;
        let publisher = BufferWriter::new(ring.writer()?, FieldId(0), pool);
        let holds_page = Marks::<WriteMap>::create(layout.buffers, 0, 0)?.writable_fd()?;
        let mut holds = HoldTable::new(layout.buffers);
        holds.add(
            0,
            read_only(std::os::fd::AsFd::as_fd(&holds_page))?,
            Limits::default(),
        )?;
        let doorbell = Doorbell::create()?;
        let (bell_eventfd, bell_page) = doorbell.subscriber_fds()?;
        let (host_eventfd, host_page) = doorbell.bell_fds()?;
        let bell = Bell::attach(host_eventfd, host_page)?;

        let mut consumer_fds: Vec<OwnedFd> = Vec::with_capacity(fds::BUFFERS + count);
        consumer_fds.push(ring.read_fd()?);
        consumer_fds.push(publisher.pool().page_fd()?);
        consumer_fds.push(jobs.write_fd()?);
        consumer_fds.push(read_only(std::os::fd::AsFd::as_fd(&acks))?);
        consumer_fds.push(bell_eventfd);
        consumer_fds.push(bell_page);
        consumer_fds.push(holds_page);
        for fd in publisher.pool().descriptors() {
            consumer_fds.push(fd.try_clone_to_owned()?);
        }
        let consumer = Consumer::attach(consumer_fds, layout, &[], shared, writable)?;
        let host = Self {
            layout,
            ring,
            publisher,
            jobs: JobCollector::new(jobs.read_fd()?, acks.try_clone()?, &layout)?,
            holds,
            bell,
        };
        Ok((host, consumer))
    }

    /// A free buffer to fill, or `None` while every buffer is published or held.
    pub fn take(&mut self) -> Option<u32> {
        self.publisher
            .take(&self.holds)
            .ok()
            .map(|BufferIndex(index)| index)
    }

    /// Copies `data` into buffer `buffer` at `offset`: a buffer the harness has taken.
    pub fn write(&mut self, buffer: u32, offset: usize, data: &[u8]) -> bool {
        self.publisher
            .buffer_mut(BufferIndex(buffer))
            .and_then(|bytes| bytes.get_mut(offset..offset.checked_add(data.len())?))
            .map(|dst| dst.copy_from_slice(data))
            .is_some()
    }

    /// Publishes buffer `buffer` as a frame of `bytes` bytes, with its sequence, identity and time.
    /// `false` if the buffer isn't free to publish, which the producer must not do.
    pub fn publish(
        &mut self,
        buffer: u32,
        bytes: u32,
        sequence: u64,
        identity: u64,
        timestamp_ns: u64,
    ) -> bool {
        let meta = FrameInfo {
            sequence,
            identity,
            bytes,
        }
        .encode();
        let names = [BufferIndex(buffer); MAX_PER_FRAME];
        self.publisher
            .publish(&names[..1], SampleTime::from_nanos(timestamp_ns), &meta)
            .is_ok()
    }

    /// Takes back every buffer no consumer holds any more. Returns how many came back.
    pub fn reclaim(&mut self) -> usize {
        let mut back = 0;
        for buffer in 0..self.layout.buffers {
            if self.publisher.reclaim(BufferIndex(buffer), &self.holds) {
                back += 1;
            }
        }
        back
    }

    /// The next job the consumer posted, into `out`, with its length.
    ///
    /// # Errors
    ///
    /// As [`JobCollector::next`].
    pub fn pop_job(&mut self, out: &mut [u8]) -> Result<Option<usize>, RingError> {
        self.jobs.next(out)
    }

    /// Ends the stream with `state`, as the host does when it ends a consumer.
    pub fn end(&mut self, state: u32) {
        self.ring.set_end(state);
        self.bell.ring();
    }

    /// Rings the consumer's doorbell, as after a publish.
    pub fn ring(&mut self) -> bool {
        self.bell.ring()
    }

    /// The layout the stream was made with.
    #[must_use]
    pub const fn layout(&self) -> &Layout {
        &self.layout
    }
}
