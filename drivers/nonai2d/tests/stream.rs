//! `image.convert/1` jobs through the host's own frame stream, with dma-bufs behind its buffers:
//! a consumer posts jobs on the real ring, the driver borrows the handles of the buffers they
//! name from the endpoint a driver is given, and completions come back on the ring. Sealed
//! memfds stand in for the dma-bufs, and an engine that only counts for the device.
//!
//! With `LEMNOS_TEST_DMA_HEAP` set to a heap (`/dev/dma_heap/system`), one test runs the same
//! path on real dma-bufs from it. Without it that test does nothing, and no test opens a device.

#![cfg(target_os = "linux")]
#![expect(
    clippy::print_stderr,
    reason = "the heap test says whether it ran, for the record"
)]

use std::cell::Cell;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::rc::Rc;
use std::sync::Arc;

use lemnos_contract_image_convert::{ConvertJob, Counters, ImageRef, JOB_BYTES};
use lemnos_core::{CallerId, Encode, Status, Writer};
use lemnos_driver_nonai2d::convert::Failure;
use lemnos_driver_nonai2d::{Converter, Engine, Frame, Layout as FrameLayout, Nonai2d};
use lemnos_fake_devices::FakeCounter;
use lemnos_hal::{ErrorKind, StreamEndpoint};
use lemnos_host::board::parse;
use lemnos_host::stream::{Backing, BufferMemory, HeapAllocator, Lease, StreamAllocator};
use lemnos_host::{Host, Options, entry};
use lemnos_linux_sys::dma_buf::{self, SYNC_END, SYNC_READ, SYNC_START, SYNC_WRITE, sealed_memfd};
use lemnos_linux_sys::dma_heap;
use lemnos_linux_sys::mmap::Mapping;
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_sim::SimBoard;
use lemnos_wire::ring::{Consumer, FrameSlot, Layout, STATE_FAULT};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const BUFFERS: u32 = 4;
/// A buffer holds a 32 by 32 frame of four bytes a pixel: one page.
const BYTES: u32 = 4096;
/// A stream whose buffers aren't whole pages: the dma-buf behind each is longer, 8192 bytes.
const ODD_BYTES: u32 = 5000;
const ARGB: u32 = u32::from_le_bytes(*b"AR24");
const XRGB: u32 = u32::from_le_bytes(*b"XR24");

/// The pool as the host addresses it, over one mapping per buffer.
struct Mapped {
    buffers: Vec<Mapping>,
    /// The dma-bufs, when they are real: CPU access to one is bracketed for the kernel.
    synced: Vec<OwnedFd>,
    stride: usize,
    bytes: usize,
}

impl Mapped {
    /// The buffer and the offset in it that pool offset `offset` is, when `len` bytes fit.
    fn at(&self, offset: usize, len: usize) -> Option<(usize, usize)> {
        let inner = offset % self.stride;
        let fits = inner.checked_add(len).is_some_and(|end| end <= self.bytes);
        fits.then_some((offset / self.stride, inner))
    }

    fn sync(&self, buffer: usize, flags: u64) -> bool {
        self.synced
            .get(buffer)
            .is_none_or(|fd| dma_buf::sync(fd.as_fd(), flags).is_ok())
    }
}

impl BufferMemory for Mapped {
    fn write(&self, offset: usize, data: &[u8]) -> bool {
        let Some((buffer, inner)) = self.at(offset, data.len()) else {
            return false;
        };
        let Some(map) = self.buffers.get(buffer) else {
            return false;
        };
        self.sync(buffer, SYNC_START | SYNC_WRITE)
            && map.write_bytes(inner, data).is_some()
            && self.sync(buffer, SYNC_END | SYNC_WRITE)
    }

    fn read(&self, offset: usize, out: &mut [u8]) -> bool {
        let Some((buffer, inner)) = self.at(offset, out.len()) else {
            return false;
        };
        let from = self
            .buffers
            .get(buffer)
            .and_then(|map| map.as_slice().get(inner..inner + out.len()));
        let Some(from) = from else {
            return false;
        };
        if !self.sync(buffer, SYNC_START | SYNC_READ) {
            return false;
        }
        out.copy_from_slice(from);
        self.sync(buffer, SYNC_END | SYNC_READ)
    }
}

/// Makes each buffer of a stream its own descriptor, mapped for `write` and `read` and handed
/// out as the buffer's handle. From `heap` they are dma-bufs; without one, sealed memfds a page
/// multiple long, as a heap rounds.
struct PerBuffer {
    heap: Option<File>,
}

impl StreamAllocator for PerBuffer {
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Backing, Status> {
        let plain = HeapAllocator.allocate(name, layout)?;
        let mut handles = Vec::new();
        let mut buffers = Vec::new();
        let mut synced = Vec::new();
        for _ in 0..layout.buffers {
            let fd: OwnedFd = match &self.heap {
                Some(heap) => dma_heap::allocate(heap.as_fd(), u64::from(layout.buffer_bytes)),
                None => sealed_memfd(c"nonai2d-stream-test", layout.stride()),
            }
            .map_err(|_| Status::IO)?;
            let bytes = layout.buffer_bytes as usize;
            buffers.push(Mapping::new(fd.as_fd(), bytes, 0, true).map_err(|_| Status::IO)?);
            if self.heap.is_some() {
                synced.push(fd.try_clone().map_err(|_| Status::IO)?);
            }
            handles.push(fd);
        }
        Ok(Backing {
            buffers: Arc::new(Mapped {
                buffers,
                synced,
                stride: layout.stride() as usize,
                bytes: layout.buffer_bytes as usize,
            }),
            handles: Some(handles.into()),
            ..plain
        })
    }
}

fn start_on(heap: Option<File>) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n\
         [devices.streams.jobs]\nbuffers = {BUFFERS}\nbuffer_bytes = {BYTES}\n\
         job_slots = 4\njob_bytes = {JOB_BYTES}\n\
         [devices.streams.odd]\nbuffers = 2\nbuffer_bytes = {ODD_BYTES}\n\
         job_slots = 4\njob_bytes = {JOB_BYTES}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start_with_streams(
        &parse(&text).unwrap(),
        &[entry::<FakeCounter>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
        Arc::new(PerBuffer { heap }),
    )
    .unwrap()
}

fn start() -> Host {
    start_on(None)
}

/// An engine that counts the conversions it was given.
struct Counted {
    runs: Rc<Cell<u32>>,
    layouts: (FrameLayout, FrameLayout),
}

impl Engine for Counted {
    fn layouts(&self) -> (FrameLayout, FrameLayout) {
        self.layouts
    }

    fn convert(&mut self, _src: &Frame<'_>, _dst: &Frame<'_>) -> io::Result<()> {
        self.runs.set(self.runs.get() + 1);
        Ok(())
    }
}

fn converter(
    runs: &Rc<Cell<u32>>,
) -> Converter<Counted, impl FnMut(FrameLayout, FrameLayout) -> io::Result<Counted>> {
    let runs = Rc::clone(runs);
    Converter::new(Nonai2d::new(move |src, dst| {
        Ok(Counted {
            runs: Rc::clone(&runs),
            layouts: (src, dst),
        })
    }))
}

fn job(identity: u64, input: u32, output: u32, width: u32, height: u32) -> ConvertJob {
    let image = |buffer, fourcc| ImageRef {
        buffer,
        width,
        height,
        fourcc,
        stride: width * 4,
    };
    ConvertJob {
        identity,
        input: image(input, ARGB),
        output: image(output, XRGB),
    }
}

fn post(consumer: &mut Consumer, job: &ConvertJob) {
    let mut bytes = [0u8; JOB_BYTES];
    let mut writer = Writer::new(&mut bytes);
    job.encode(&mut writer).unwrap();
    let used = writer.len();
    assert!(consumer.push_job(&bytes[..used]).unwrap());
}

fn consumer(lease: &Lease) -> Consumer {
    Consumer::attach(Arc::clone(&lease.ring), lease.layout).unwrap()
}

fn completion(consumer: &mut Consumer) -> FrameSlot {
    consumer.pop_frame().unwrap().expect("a completion")
}

#[test]
fn jobs_on_the_ring_run_on_the_streams_dma_bufs_and_complete_on_the_ring() {
    let host = start();
    let mut producer = host.stream_producer(0, "jobs").unwrap();
    let lease = host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer = consumer(&lease);
    // What a driver is given.
    let stream: &mut dyn StreamEndpoint<Error = ErrorKind> = &mut producer;
    let runs = Rc::new(Cell::new(0));
    let mut converter = converter(&runs);
    assert_eq!(converter.serve(stream, 0), Ok(0), "holds the free buffers");
    // The consumer fills buffer 0 and asks for it converted into buffer 1.
    let offset = lease.layout.buffer_offset(0).unwrap();
    assert!(lease.buffers.write(offset, &[0x11; 64]));
    post(&mut consumer, &job(0xF00D, 0, 1, 32, 32));
    assert_eq!(converter.serve(stream, 9), Ok(1));
    assert_eq!(runs.get(), 1);
    let done = completion(&mut consumer);
    assert_eq!(
        (done.buffer, done.identity, done.bytes, done.timestamp_ns),
        (1, 0xF00D, BYTES, 9)
    );
    assert_eq!(converter.counters(), Counters { jobs: 1, errors: 0 });
    // A frame one row taller than the dma-buf behind the buffer never reaches the engine.
    post(&mut consumer, &job(2, 0, 2, 32, 33));
    assert_eq!(converter.serve(stream, 10), Ok(1));
    assert_eq!(runs.get(), 1);
    assert_eq!(converter.last_failure(), Some(Failure::Frame));
    let failed = completion(&mut consumer);
    assert_eq!((failed.buffer, failed.identity, failed.bytes), (2, 2, 0));
    // Nor does a job on a buffer the consumer has: buffer 1 isn't back yet.
    post(&mut consumer, &job(3, 1, 3, 32, 32));
    assert_eq!(converter.serve(stream, 11), Ok(1));
    assert_eq!(runs.get(), 1);
    assert_eq!(converter.last_failure(), Some(Failure::Buffers));
    assert_eq!(completion(&mut consumer).bytes, 0);
    // Returned, it can be an input.
    assert!(consumer.release(done.buffer, done.fence).unwrap());
    assert!(consumer.release(failed.buffer, failed.fence).unwrap());
    post(&mut consumer, &job(4, 1, 2, 32, 32));
    assert_eq!(converter.serve(stream, 12), Ok(1));
    assert_eq!(runs.get(), 2);
    assert_eq!(completion(&mut consumer).identity, 4);
    let stats = host.stream_stats(0, "jobs").unwrap();
    assert_eq!((stats.faults, stats.dropped), (0, 0));
}

#[test]
fn bytes_that_are_no_job_end_the_consumers_stream_as_faulted_and_the_next_consumer_is_served() {
    let host = start();
    let mut producer = host.stream_producer(0, "jobs").unwrap();
    let lease = host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer = consumer(&lease);
    let runs = Rc::new(Cell::new(0));
    let mut converter = converter(&runs);
    assert_eq!(converter.serve(&mut producer, 0), Ok(0));
    assert!(consumer.push_job(&[1, 2, 3]).unwrap());
    post(&mut consumer, &job(1, 0, 1, 32, 32));
    assert_eq!(converter.serve(&mut producer, 1), Ok(1));
    // The consumer sees the fault, and the job it posted behind the bad bytes ran nowhere.
    assert_eq!(consumer.state(), STATE_FAULT);
    assert_eq!(runs.get(), 0);
    assert_eq!(converter.last_failure(), Some(Failure::Undecodable));
    let stats = host.stream_stats(0, "jobs").unwrap();
    assert_eq!((stats.faults, stats.attached), (1, false));
    assert_eq!((stats.producer_held, stats.consumer_held), (0, 0));
    // The driver carries on: the next consumer's job runs.
    let next = host.stream_attach(0, "jobs", CallerId(2)).unwrap();
    let mut fresh = self::consumer(&next);
    post(&mut fresh, &job(2, 0, 1, 32, 32));
    assert_eq!(converter.serve(&mut producer, 2), Ok(1));
    assert_eq!(runs.get(), 1);
    assert_eq!(completion(&mut fresh).identity, 2);
    assert_eq!(host.stream_stats(0, "jobs").unwrap().faults, 1);
}

#[test]
fn a_new_consumers_first_job_runs_on_its_own_memory() {
    let host = start();
    let mut producer = host.stream_producer(0, "jobs").unwrap();
    let runs = Rc::new(Cell::new(0));
    let mut converter = converter(&runs);
    let first = host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    assert_eq!(converter.serve(&mut producer, 0), Ok(0));
    assert!((0..BUFFERS).all(|buffer| converter.holds(buffer)));
    host.stream_detach(0, "jobs", CallerId(1)).unwrap();
    // Everything the driver holds is from the first consumer's time.
    let second = host.stream_attach(0, "jobs", CallerId(2)).unwrap();
    let mut consumer = consumer(&second);
    post(&mut consumer, &job(7, 2, 3, 32, 32));
    assert_eq!(converter.serve(&mut producer, 1), Ok(1));
    assert_eq!(runs.get(), 1);
    assert_eq!(converter.counters(), Counters { jobs: 1, errors: 0 });
    let done = completion(&mut consumer);
    assert_eq!((done.buffer, done.identity, done.bytes), (3, 7, BYTES));
    assert_eq!(consumer.pop_frame().unwrap(), None);
    // The two buffers given up reached no one, and the handles lent are the second's.
    assert_eq!(host.stream_stats(0, "jobs").unwrap().dropped, 2);
    let lent = producer.buffer_handle(2).unwrap().unwrap();
    let given = &second.handles.as_ref().unwrap()[2];
    let same = |fd| lemnos_linux_sys::dma_buf::identity(fd).unwrap();
    assert_eq!(same(lent.fd()), same(given.as_fd()));
    assert_ne!(
        same(lent.fd()),
        same(first.handles.as_ref().unwrap()[2].as_fd())
    );
}

#[test]
fn a_thousand_jobs_through_the_stream_allocate_nothing() {
    let host = start();
    let mut producer = host.stream_producer(0, "jobs").unwrap();
    let lease = host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let mut consumer = consumer(&lease);
    let runs = Rc::new(Cell::new(0));
    let mut converter = converter(&runs);
    let mut cycle = |n: u64| {
        let output = 1 + (n % 3) as u32;
        post(&mut consumer, &job(n, 0, output, 32, 32));
        assert_eq!(converter.serve(&mut producer, n), Ok(1));
        let done = completion(&mut consumer);
        assert_eq!((done.buffer, done.identity, done.bytes), (output, n, BYTES));
        assert!(consumer.release(done.buffer, done.fence).unwrap());
    };
    for n in 0..10 {
        cycle(n);
    }
    let ((), count) = allocations(|| {
        for n in 10..1010 {
            cycle(n);
        }
    });
    assert_eq!(count, 0, "allocations in 1000 jobs");
    assert_eq!(runs.get(), 1010);
}

/// A frame that fits the dma-buf but not the stream's buffer: the engine may write it, since
/// the memory is there, and the stream refuses a completion that long.
fn a_frame_past_the_streams_buffer_but_inside_its_dma_buf(host: &Host) {
    let mut producer = host.stream_producer(0, "odd").unwrap();
    let lease = host.stream_attach(0, "odd", CallerId(1)).unwrap();
    let mut consumer = consumer(&lease);
    let runs = Rc::new(Cell::new(0));
    let mut converter = converter(&runs);
    assert_eq!(converter.serve(&mut producer, 0), Ok(0));
    let lent = producer.buffer_handle(1).unwrap().unwrap();
    assert_eq!(
        dma_buf::size(lent.fd()).unwrap(),
        8192,
        "rounded up to pages"
    );
    // 32 by 39 is 4992 bytes: inside the stream's buffer.
    post(&mut consumer, &job(1, 0, 1, 32, 39));
    assert_eq!(converter.serve(&mut producer, 0), Ok(1));
    let done = completion(&mut consumer);
    assert_eq!((done.identity, done.bytes), (1, 4992));
    assert!(consumer.release(done.buffer, done.fence).unwrap());
    // 32 by 40 is 5120: past the stream's buffer, inside the dma-buf.
    post(&mut consumer, &job(2, 0, 1, 32, 40));
    assert_eq!(converter.serve(&mut producer, 0), Ok(1));
    assert_eq!(runs.get(), 2);
    assert_eq!(converter.last_failure(), Some(Failure::Completion));
    let done = completion(&mut consumer);
    assert_eq!((done.identity, done.bytes), (2, 0));
    assert!(consumer.release(done.buffer, done.fence).unwrap());
    // 32 by 65 is 8320: past the dma-buf, and the engine never sees it.
    post(&mut consumer, &job(3, 0, 1, 32, 65));
    assert_eq!(converter.serve(&mut producer, 0), Ok(1));
    assert_eq!(runs.get(), 2);
    assert_eq!(converter.last_failure(), Some(Failure::Frame));
    assert_eq!(completion(&mut consumer).bytes, 0);
    assert_eq!(converter.counters(), Counters { jobs: 1, errors: 2 });
}

#[test]
fn a_dma_buf_longer_than_the_streams_buffer_is_checked_by_its_real_size() {
    a_frame_past_the_streams_buffer_but_inside_its_dma_buf(&start());
}

#[test]
fn the_same_on_dma_bufs_from_a_real_heap_when_one_is_named() {
    let Some(path) = std::env::var_os("LEMNOS_TEST_DMA_HEAP") else {
        eprintln!("skipped: LEMNOS_TEST_DMA_HEAP names no heap");
        return;
    };
    let heap = File::open(&path).expect("the heap LEMNOS_TEST_DMA_HEAP names");
    let host = start_on(Some(heap));
    a_frame_past_the_streams_buffer_but_inside_its_dma_buf(&host);
    // What the producer writes and what is behind the handle are one memory.
    let mut producer = host.stream_producer(0, "jobs").unwrap();
    let lease = host.stream_attach(0, "jobs", CallerId(1)).unwrap();
    let buffer = producer.next_buffer().unwrap().unwrap();
    assert_eq!(producer.write(buffer, 8, &[1, 2, 3, 4]), Ok(true));
    let lent = producer.buffer_handle(buffer).unwrap().unwrap();
    assert_eq!(dma_buf::size(lent.fd()).unwrap(), u64::from(BYTES));
    let map = Mapping::new(lent.fd(), BYTES as usize, 0, false).unwrap();
    dma_buf::sync(lent.fd(), SYNC_START | SYNC_READ).unwrap();
    assert_eq!(map.as_slice()[8..12], [1, 2, 3, 4]);
    dma_buf::sync(lent.fd(), SYNC_END | SYNC_READ).unwrap();
    // And it is the dma-buf the consumer was given.
    let given = &lease.handles.as_ref().unwrap()[buffer as usize];
    assert_eq!(
        dma_buf::identity(lent.fd()).unwrap(),
        dma_buf::identity(given.as_fd()).unwrap()
    );
    let mut out = [0; 4];
    assert_eq!(producer.read(buffer, 8, &mut out), Ok(true));
    assert_eq!(out, [1, 2, 3, 4]);
    eprintln!("ran on dma-bufs from {}", path.to_string_lossy());
}
