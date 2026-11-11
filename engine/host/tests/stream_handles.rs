//! Streams whose buffers are dma-bufs: a producer borrows the handle behind a buffer it holds,
//! for hardware. Unnamed files stand in for the dma-bufs, one per buffer.

#![cfg(unix)]

use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lemnos_core::{CallerId, Status};
use lemnos_fake_devices::FakeCounter;
use lemnos_hal::blocks::{BufferHandle, BufferKind, StreamFrame};
use lemnos_hal::{ErrorKind, StreamEndpoint};
use lemnos_host::board::parse;
use lemnos_host::stream::{
    Backing, BufferMemory, HeapAllocator, Lease, ProducerEndpoint, StreamAllocator, StreamMemory,
};
use lemnos_host::{Host, Options, entry};
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_sim::SimBoard;
use lemnos_wire::ring::{Consumer, Layout};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const FIRST: CallerId = CallerId(7);
const SECOND: CallerId = CallerId(8);
const BUFFERS: u32 = 4;
const BYTES: usize = 64;
/// The board file's key for a stream of dma-bufs.
const DMA_BUF: &str = "memory = \"dma-buf\"\n";

/// A file of `len` bytes with no name left.
fn unnamed(len: u64) -> File {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let name = format!(
        "lemnos-host-handles-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let path = std::env::temp_dir().join(name);
    let file = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    file.set_len(len).unwrap();
    file
}

/// The pool as the host addresses it, over one file per buffer.
struct PerBuffer {
    files: Vec<File>,
    stride: usize,
    bytes: usize,
}

impl PerBuffer {
    /// The file and the offset in it that pool offset `offset` is, when `len` bytes fit.
    fn at(&self, offset: usize, len: usize) -> Option<(&File, u64)> {
        let inner = offset % self.stride;
        let fits = inner.checked_add(len).is_some_and(|end| end <= self.bytes);
        let file = self.files.get(offset / self.stride)?;
        fits.then_some((file, inner as u64))
    }
}

impl BufferMemory for PerBuffer {
    fn write(&self, offset: usize, data: &[u8]) -> bool {
        self.at(offset, data.len())
            .is_some_and(|(file, at)| file.write_all_at(data, at).is_ok())
    }

    fn read(&self, offset: usize, out: &mut [u8]) -> bool {
        self.at(offset, out.len())
            .is_some_and(|(file, at)| file.read_exact_at(out, at).is_ok())
    }
}

/// Makes every stream's buffers files with handles, `short` fewer handles than buffers.
struct FileStreams {
    short: usize,
}

impl StreamAllocator for FileStreams {
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Backing, Status> {
        HeapAllocator.allocate(name, layout)
    }

    fn allocate_handles(&self, name: &str, layout: &Layout, _: &str) -> Result<Backing, Status> {
        let heap = HeapAllocator.allocate(name, layout)?;
        let files: Vec<File> = (0..layout.buffers)
            .map(|_| unnamed(u64::from(layout.buffer_bytes)))
            .collect();
        let handles: Vec<OwnedFd> = files
            .iter()
            .skip(self.short)
            .map(|file| file.try_clone().map(OwnedFd::from))
            .collect::<Result<_, _>>()
            .map_err(|_| Status::IO)?;
        Ok(Backing {
            buffers: Arc::new(PerBuffer {
                files,
                stride: layout.stride() as usize,
                bytes: layout.buffer_bytes as usize,
            }),
            handles: Some(handles.into()),
            ..heap
        })
    }
}

fn start(allocator: Arc<dyn StreamAllocator>) -> Host {
    start_declared(allocator, "")
}

/// A host whose `frames` stream has `keys` added to it in the board file, and a `plain` stream
/// beside it.
fn start_declared(allocator: Arc<dyn StreamAllocator>, keys: &str) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = {BUFFERS}\nbuffer_bytes = {BYTES}\n{keys}\
         [devices.streams.plain]\nbuffers = 2\nbuffer_bytes = {BYTES}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start_with_streams(
        &parse(&text).unwrap(),
        &[entry::<FakeCounter>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
        allocator,
    )
    .unwrap()
}

fn with_handles() -> Host {
    start_declared(Arc::new(FileStreams { short: 0 }), DMA_BUF)
}

fn consumer(lease: &Lease) -> Consumer {
    Consumer::attach(Arc::clone(&lease.ring), lease.layout).unwrap()
}

/// The memory behind a handle, as hardware given the descriptor reaches it: through a hold of
/// its own, which outlives the borrow.
fn hardware(handle: BufferHandle<'_>) -> File {
    handle.fd().try_clone_to_owned().unwrap().into()
}

fn frame(buffer: u32) -> StreamFrame {
    StreamFrame {
        buffer,
        bytes: BYTES as u32,
        sequence: 1,
        identity: 9,
        timestamp_ns: 0,
    }
}

/// What a consumer reads in `buffer`.
fn seen(lease: &Lease, buffer: u32) -> [u8; 4] {
    let mut out = [0; 4];
    let offset = lease.layout.buffer_offset(buffer).unwrap();
    assert!(lease.buffers.read(offset, &mut out));
    out
}

#[test]
fn a_driver_borrows_the_handles_of_buffers_it_holds_and_the_consumer_sees_what_hardware_wrote() {
    let host = with_handles();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let lease = host.stream_attach(0, "frames", FIRST).unwrap();
    let mut consumer = consumer(&lease);
    // What a driver holds.
    let stream: &mut dyn StreamEndpoint<Error = ErrorKind> = &mut producer;
    let input = stream.next_buffer().unwrap().unwrap();
    let output = stream.next_buffer().unwrap().unwrap();
    assert_ne!(input, output);
    // The consumer puts an image in a buffer the driver holds, as a job's input.
    let offset = lease.layout.buffer_offset(input).unwrap();
    assert!(lease.buffers.write(offset, &[1, 2, 3, 4]));
    // Both handles at once: hardware reads one buffer and writes the other.
    let from = stream.buffer_handle(input).unwrap().unwrap();
    let to = stream.buffer_handle(output).unwrap().unwrap();
    let mut pixels = [0; 4];
    hardware(from).read_exact_at(&mut pixels, 0).unwrap();
    assert_eq!(pixels, [1, 2, 3, 4]);
    hardware(to).write_all_at(&[40, 30, 20, 10], 0).unwrap();
    // `read` and `write` reach the same memory as the handle.
    let mut out = [0; 4];
    assert_eq!(stream.read(output, 0, &mut out), Ok(true));
    assert_eq!(out, [40, 30, 20, 10]);
    assert_eq!(stream.write(output, 2, &[7, 7]), Ok(true));
    let to = stream.buffer_handle(output).unwrap().unwrap();
    hardware(to).read_exact_at(&mut out, 0).unwrap();
    assert_eq!(out, [40, 30, 7, 7]);
    // The handle is the stream's buffer: the whole of it, and the one the consumer was given.
    let to = stream.buffer_handle(output).unwrap().unwrap();
    assert_eq!(hardware(to).metadata().unwrap().len(), BYTES as u64);
    let given = lease.handles.as_ref().unwrap();
    assert_eq!(given.len(), BUFFERS as usize);
    File::from(given[output as usize].try_clone().unwrap())
        .read_exact_at(&mut out, 0)
        .unwrap();
    assert_eq!(out, [40, 30, 7, 7]);
    stream.publish(frame(output)).unwrap();
    let got = consumer.pop_frame().unwrap().unwrap();
    assert_eq!((got.buffer, got.identity), (output, 9));
    assert_eq!(seen(&lease, output), [40, 30, 7, 7]);
}

#[test]
fn only_a_buffer_the_producer_holds_has_a_handle() {
    let host = with_handles();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let _lease = host.stream_attach(0, "frames", FIRST).unwrap();
    let refused = |producer: &ProducerEndpoint, buffer| {
        assert_eq!(
            producer.buffer_handle(buffer).map(|_| ()),
            Err(ErrorKind::InvalidInput),
            "buffer {buffer}"
        );
    };
    refused(&producer, 0);
    refused(&producer, BUFFERS);
    refused(&producer, u32::MAX);
    let buffer = producer.next_buffer().unwrap().unwrap();
    assert!(producer.buffer_handle(buffer).unwrap().is_some());
    refused(&producer, buffer + 1);
    producer.publish(frame(buffer)).unwrap();
    refused(&producer, buffer);
}

#[test]
fn nothing_is_behind_a_buffer_with_no_consumer_or_from_before_this_one() {
    let host = with_handles();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let early = producer.next_buffer().unwrap().unwrap();
    assert!(producer.buffer_handle(early).unwrap().is_none(), "nobody");
    let lease = host.stream_attach(0, "frames", FIRST).unwrap();
    let mut consumer = consumer(&lease);
    assert!(
        producer.buffer_handle(early).unwrap().is_none(),
        "taken before the consumer came"
    );
    // Publishing it reaches no one and frees it.
    producer.publish(frame(early)).unwrap();
    assert_eq!(consumer.pop_frame().unwrap(), None);
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!((stats.dropped, stats.free), (1, BUFFERS));
    // Taken now, each one has a handle.
    for _ in 0..BUFFERS {
        let buffer = producer.next_buffer().unwrap().unwrap();
        assert!(producer.buffer_handle(buffer).unwrap().is_some());
    }
}

#[test]
fn a_consumer_that_goes_keeps_its_memory_to_itself_and_the_next_gets_its_own() {
    let host = with_handles();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let first = host.stream_attach(0, "frames", FIRST).unwrap();
    let kept = producer.next_buffer().unwrap().unwrap();
    // Hardware is given the buffer and keeps its hold past the borrow.
    let late = hardware(producer.buffer_handle(kept).unwrap().unwrap());
    host.stream_detach(0, "frames", FIRST).unwrap();
    assert!(
        producer.buffer_handle(kept).unwrap().is_none(),
        "the consumer went"
    );
    let second = host.stream_attach(0, "frames", SECOND).unwrap();
    let mut consumer = consumer(&second);
    assert!(
        producer.buffer_handle(kept).unwrap().is_none(),
        "still the first consumer's buffer"
    );
    // Hardware still writing lands in the first consumer's memory, which is still there, and
    // nowhere the second can see.
    late.write_all_at(&[0xEE; 4], 0).unwrap();
    assert_eq!(seen(&first, kept), [0xEE; 4]);
    assert_eq!(seen(&second, kept), [0; 4]);
    // Its frame reaches no one.
    producer.publish(frame(kept)).unwrap();
    assert_eq!(consumer.pop_frame().unwrap(), None);
    // The same buffer number, taken again, is the second consumer's memory.
    let mut again = None;
    while let Some(buffer) = producer.next_buffer().unwrap() {
        if buffer == kept {
            again = Some(buffer);
        }
    }
    let again = again.expect("the buffer came back free");
    let fresh = hardware(producer.buffer_handle(again).unwrap().unwrap());
    fresh.write_all_at(&[0x22; 4], 0).unwrap();
    assert_eq!(seen(&second, again), [0x22; 4]);
    assert_eq!(seen(&first, again), [0xEE; 4], "the first sees none of it");
    producer.publish(frame(again)).unwrap();
    assert_eq!(consumer.pop_frame().unwrap().unwrap().buffer, again);
}

#[test]
fn a_producer_knows_what_its_buffers_are_before_any_consumer_attaches() {
    let host = with_handles();
    // What a driver is given when it binds.
    let frames: Box<dyn StreamEndpoint<Error = ErrorKind>> =
        Box::new(host.stream_producer(0, "frames").unwrap());
    let plain: Box<dyn StreamEndpoint<Error = ErrorKind>> =
        Box::new(host.stream_producer(0, "plain").unwrap());
    assert_eq!(frames.buffer_kind(), BufferKind::Handles);
    assert_eq!(plain.buffer_kind(), BufferKind::Plain);
    for name in ["frames", "plain"] {
        assert_eq!(host.stream_stats(0, name).unwrap().attachments, 0, "{name}");
    }
    // It stays so with a consumer, and after it.
    host.stream_attach(0, "frames", FIRST).unwrap();
    host.stream_attach(0, "plain", FIRST).unwrap();
    assert_eq!(frames.buffer_kind(), BufferKind::Handles);
    assert_eq!(plain.buffer_kind(), BufferKind::Plain);
    host.caller_gone(FIRST);
    assert_eq!(frames.buffer_kind(), BufferKind::Handles);
    assert_eq!(plain.buffer_kind(), BufferKind::Plain);
}

/// Hands out handles for every stream, declared as dma-bufs or not.
struct Eager;

impl StreamAllocator for Eager {
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Backing, Status> {
        FileStreams { short: 0 }.allocate_handles(name, layout, "")
    }
}

#[test]
fn a_stream_declared_as_plain_memory_lends_no_handle_whatever_the_allocator_made() {
    let host = start(Arc::new(Eager));
    let mut producer = host.stream_producer(0, "plain").unwrap();
    assert_eq!(producer.buffer_kind(), BufferKind::Plain);
    let lease = host.stream_attach(0, "plain", FIRST).unwrap();
    assert!(lease.handles.is_none());
    let buffer = producer.next_buffer().unwrap().unwrap();
    assert_eq!(
        producer.buffer_handle(buffer).map(|_| ()),
        Err(ErrorKind::Unsupported),
        "what the driver was told at bind stays true"
    );
}

#[test]
fn a_stream_of_plain_memory_has_no_handles_and_says_so() {
    let host = start(Arc::new(HeapAllocator));
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let buffer = producer.next_buffer().unwrap().unwrap();
    assert!(producer.buffer_handle(buffer).unwrap().is_none(), "nobody");
    let lease = host.stream_attach(0, "frames", FIRST).unwrap();
    assert!(lease.handles.is_none());
    assert_eq!(
        producer.buffer_handle(buffer).map(|_| ()),
        Err(ErrorKind::Unsupported)
    );
    let buffer = producer.next_buffer().unwrap().unwrap();
    assert_eq!(
        producer.buffer_handle(buffer).map(|_| ()),
        Err(ErrorKind::Unsupported)
    );
    assert_eq!(
        producer.write(buffer, 0, &[1]),
        Ok(true),
        "write still works"
    );
}

#[test]
fn an_attachment_with_the_wrong_number_of_handles_is_refused() {
    let host = start_declared(Arc::new(FileStreams { short: 1 }), DMA_BUF);
    assert_eq!(
        host.stream_attach(0, "frames", FIRST).err(),
        Some(Status::IO)
    );
    let stats = host.stream_stats(0, "frames").unwrap();
    assert!(!stats.attached);
    assert_eq!(stats.attachments, 0);
}

#[test]
fn a_thousand_frames_filled_through_their_handles_allocate_nothing() {
    let host = with_handles();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let lease = host.stream_attach(0, "frames", FIRST).unwrap();
    let mut consumer = consumer(&lease);
    // The hardware's own holds, taken once, as a kernel driver keeps what it imported.
    let mut imported: Vec<Option<File>> = (0..BUFFERS).map(|_| None).collect();
    let mut cycle = |sequence: u64| {
        let buffer = producer.next_buffer().unwrap().unwrap();
        let handle = producer.buffer_handle(buffer).unwrap().unwrap();
        let slot = &mut imported[buffer as usize];
        if slot.is_none() {
            *slot = Some(hardware(handle));
        }
        let pixels = sequence.to_le_bytes();
        slot.as_ref().unwrap().write_all_at(&pixels, 0).unwrap();
        producer
            .publish(StreamFrame {
                sequence,
                ..frame(buffer)
            })
            .unwrap();
        let got = consumer.pop_frame().unwrap().unwrap();
        assert_eq!((got.buffer, got.sequence), (buffer, sequence));
        let mut read = [0; 8];
        let offset = lease.layout.buffer_offset(buffer).unwrap();
        assert!(lease.buffers.read(offset, &mut read));
        assert_eq!(read, pixels);
        assert!(consumer.release(got.buffer, got.fence).unwrap());
    };
    for sequence in 0..10 {
        cycle(sequence);
    }
    let ((), count) = allocations(|| {
        for sequence in 10..1010 {
            cycle(sequence);
        }
    });
    assert_eq!(count, 0, "allocations in 1000 frames");
}

/// Gives dma-bufs (files, here) only when asked for them, and notes the heap it was asked for.
/// `empty_handed` answers such a request with plain memory, which no allocator should.
#[derive(Default)]
struct Declared {
    asked: std::sync::Mutex<Vec<(String, String)>>,
    empty_handed: bool,
}

impl StreamAllocator for Declared {
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Backing, Status> {
        HeapAllocator.allocate(name, layout)
    }

    fn allocate_handles(&self, name: &str, layout: &Layout, heap: &str) -> Result<Backing, Status> {
        self.asked
            .lock()
            .unwrap()
            .push((name.to_string(), heap.to_string()));
        if self.empty_handed {
            return HeapAllocator.allocate(name, layout);
        }
        FileStreams { short: 0 }.allocate_handles(name, layout, heap)
    }
}

#[test]
fn a_stream_the_board_declares_as_dma_bufs_is_asked_of_the_allocator_with_its_heap() {
    for (keys, heap) in [
        ("memory = \"dma-buf\"\n", "system"),
        ("memory = \"dma-buf\"\nheap = \"linux,cma\"\n", "linux,cma"),
    ] {
        let allocator = Arc::new(Declared::default());
        let host = start_declared(Arc::clone(&allocator) as Arc<dyn StreamAllocator>, keys);
        assert_eq!(host.stream_memory(0, "frames"), Some(StreamMemory::DmaBuf));
        assert_eq!(host.stream_memory(0, "plain"), Some(StreamMemory::Shared));
        assert_eq!(host.stream_memory(0, "none"), None);
        assert_eq!(host.stream_memory(9, "frames"), None);

        let mut producer = host.stream_producer(0, "frames").unwrap();
        let lease = host.stream_attach(0, "frames", FIRST).unwrap();
        assert_eq!(lease.handles.as_ref().unwrap().len(), BUFFERS as usize);
        let buffer = producer.next_buffer().unwrap().unwrap();
        assert!(producer.buffer_handle(buffer).unwrap().is_some());
        // The stream beside it is shared memory, and is asked for as it always was.
        let plain = host.stream_attach(0, "plain", FIRST).unwrap();
        assert!(plain.handles.is_none());
        assert_eq!(
            *allocator.asked.lock().unwrap(),
            [("frames".to_string(), heap.to_string())]
        );
    }
}

#[test]
fn an_allocator_with_no_dma_bufs_refuses_a_stream_declared_as_them_and_never_makes_it_plain() {
    // The host's own allocator, for a consumer in the same process, has none.
    let host = start_declared(Arc::new(HeapAllocator), "memory = \"dma-buf\"\n");
    assert_eq!(
        host.stream_attach(0, "frames", FIRST).err(),
        Some(Status::NOT_SUPPORTED)
    );
    let stats = host.stream_stats(0, "frames").unwrap();
    assert!(!stats.attached);
    assert_eq!(stats.attachments, 0);
    // The stream beside it is untouched by that.
    assert!(host.stream_attach(0, "plain", FIRST).is_ok());
}

#[test]
fn plain_memory_handed_over_for_a_stream_declared_as_dma_bufs_is_refused() {
    let allocator = Arc::new(Declared {
        empty_handed: true,
        ..Declared::default()
    });
    let host = start_declared(allocator, "memory = \"dma-buf\"\n");
    assert_eq!(
        host.stream_attach(0, "frames", FIRST).err(),
        Some(Status::IO)
    );
    assert!(!host.stream_stats(0, "frames").unwrap().attached);
}
