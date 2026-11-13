//! Streams whose buffers are dma-bufs: a producer borrows the handle behind a buffer it holds,
//! for hardware, a buffer of several planes lends one per plane, and a stream on standby has
//! memory to fill while nobody is attached. Unnamed files stand in for the dma-bufs, one per
//! buffer or plane.

#![cfg(unix)]

use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

use lemnos_core::{CallerId, Status};
use lemnos_fake_devices::FakeCounter;
use lemnos_hal::blocks::{BufferHandle, BufferKind, StreamFrame};
use lemnos_hal::{ErrorKind, StreamEndpoint};
use lemnos_host::board::parse;
use lemnos_host::stream::{
    Buffers, MemfdAllocator, ProducerEndpoint, StreamAllocator, StreamMemory,
};
use lemnos_host::{Host, Options, entry};
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_sim::SimBoard;
use lemnos_wire::Planes;
use lemnos_wire::ring::{Consumer, Layout};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const FIRST: CallerId = CallerId(7);
const SECOND: CallerId = CallerId(8);
const BUFFERS: u32 = 4;
const BYTES: usize = 64;
/// The board file's key for a stream of dma-bufs.
const DMA_BUF: &str = "memory = \"dma-buf\"\n";

/// A dma-buf stand-in of `len` bytes: a sealed memfd, which the host can map (a regular file
/// must be sealed against shrinking to be mapped at all).
fn unnamed(len: u64) -> OwnedFd {
    shm_ring::sealed_memfd(len as usize).unwrap()
}

/// Makes every stream's buffers files: one per plane of each buffer, as dma-bufs. `short` leaves
/// that many off the end, which the host must refuse.
struct FileStreams {
    short: usize,
}

impl StreamAllocator for FileStreams {
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Buffers, Status> {
        MemfdAllocator.allocate(name, layout)
    }

    fn allocate_planes(
        &self,
        _: &str,
        layout: &Layout,
        _: &str,
        planes: &Planes,
    ) -> Result<Buffers, Status> {
        let mut fds: Vec<OwnedFd> = (0..layout.buffers)
            .flat_map(|_| planes.lengths().iter().map(|len| unnamed(u64::from(*len))))
            .collect();
        fds.truncate(fds.len() - self.short);
        Ok(Buffers {
            fds,
            dma: true,
            sync: None,
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

/// What a consumer reads at the start of `buffer`.
fn seen(consumer: &Consumer, buffer: u32) -> [u8; 4] {
    let mut out = [0; 4];
    assert!(consumer.read(buffer, 0, &mut out));
    out
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

#[test]
fn a_driver_borrows_the_handles_of_buffers_it_holds_and_the_consumer_sees_what_hardware_wrote() {
    let host = with_handles();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let lease = host.stream_attach(0, "frames", FIRST).unwrap();
    let given = lease.handles.clone().unwrap();
    let mut consumer = lease.consumer_writable().unwrap();
    // What a driver holds.
    let stream: &mut dyn StreamEndpoint<Error = ErrorKind> = &mut producer;
    let input = stream.next_buffer().unwrap().unwrap();
    let output = stream.next_buffer().unwrap().unwrap();
    assert_ne!(input, output);
    // The consumer puts an image in a buffer the driver holds, as a job's input.
    assert!(consumer.write(input, 0, &[1, 2, 3, 4]));
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
    // The handle is the stream's buffer: at least the whole of it (a dma-buf stand-in is page
    // sized), and the one the consumer was given.
    let to = stream.buffer_handle(output).unwrap().unwrap();
    assert!(hardware(to).metadata().unwrap().len() >= BYTES as u64);
    assert_eq!(given.len(), BUFFERS as usize);
    File::from(given[output as usize].try_clone().unwrap())
        .read_exact_at(&mut out, 0)
        .unwrap();
    assert_eq!(out, [40, 30, 7, 7]);
    stream.publish(frame(output)).unwrap();
    let got = consumer.pop_frame().unwrap().unwrap();
    assert_eq!((got.buffer, got.identity), (output, 9));
    assert_eq!(seen(&consumer, output), [40, 30, 7, 7]);
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
    let mut consumer = host
        .stream_attach(0, "frames", FIRST)
        .unwrap()
        .consumer()
        .unwrap();
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
fn a_producer_can_tell_a_buffer_taken_under_a_consumer_that_has_gone() {
    // On plain memory too, where no handle can say so.
    let host = with_handles();
    let mut producer = host.stream_producer(0, "plain").unwrap();
    assert!(producer.buffer_current(0).is_err(), "not the producer's");
    let early = producer.next_buffer().unwrap().unwrap();
    assert_eq!(producer.buffer_current(early), Ok(false), "nobody");
    let _first = host.stream_attach(0, "plain", FIRST).unwrap();
    assert_eq!(
        producer.buffer_current(early),
        Ok(false),
        "taken before the consumer came"
    );
    let current = producer.next_buffer().unwrap().unwrap();
    assert_eq!(producer.buffer_current(current), Ok(true));
    // The consumer goes and another comes between two looks.
    host.stream_detach(0, "plain", FIRST).unwrap();
    assert_eq!(producer.buffer_current(current), Ok(false));
    let mut second = host
        .stream_attach(0, "plain", SECOND)
        .unwrap()
        .consumer()
        .unwrap();
    assert_eq!(producer.buffer_current(current), Ok(false));
    // Publishing it reaches no one and frees it, to be taken again under the one there now.
    producer.publish(frame(current)).unwrap();
    assert_eq!(second.pop_frame().unwrap(), None);
    producer.publish(frame(early)).unwrap();
    for _ in 0..2 {
        let buffer = producer.next_buffer().unwrap().unwrap();
        assert_eq!(producer.buffer_current(buffer), Ok(true));
    }
}

#[test]
fn a_consumer_that_goes_keeps_its_memory_to_itself_and_the_next_gets_its_own() {
    let host = with_handles();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let first = host
        .stream_attach(0, "frames", FIRST)
        .unwrap()
        .consumer()
        .unwrap();
    let kept = producer.next_buffer().unwrap().unwrap();
    // Hardware is given the buffer and keeps its hold past the borrow.
    let late = hardware(producer.buffer_handle(kept).unwrap().unwrap());
    host.stream_detach(0, "frames", FIRST).unwrap();
    assert!(
        producer.buffer_handle(kept).unwrap().is_none(),
        "the consumer went"
    );
    let mut second = host
        .stream_attach(0, "frames", SECOND)
        .unwrap()
        .consumer()
        .unwrap();
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
    assert_eq!(second.pop_frame().unwrap(), None);
    // The same buffer number, taken again, is the second consumer's memory.
    // Taking buffers until the producer offers `kept` again: a bounded number of takes, since the
    // producer reclaims unread frames and never runs out.
    let mut again = None;
    for _ in 0..2 * BUFFERS {
        let buffer = producer.next_buffer().unwrap().unwrap();
        if buffer == kept {
            again = Some(buffer);
            break;
        }
    }
    let again = again.expect("the buffer came back free");
    let fresh = hardware(producer.buffer_handle(again).unwrap().unwrap());
    fresh.write_all_at(&[0x22; 4], 0).unwrap();
    assert_eq!(seen(&second, again), [0x22; 4]);
    assert_eq!(seen(&first, again), [0xEE; 4], "the first sees none of it");
    producer.publish(frame(again)).unwrap();
    assert_eq!(second.pop_frame().unwrap().unwrap().buffer, again);
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
    /// Memory that is dma-bufs, for a stream the board file declares as plain.
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Buffers, Status> {
        Ok(Buffers {
            dma: true,
            ..MemfdAllocator.allocate(name, layout)?
        })
    }
}

#[test]
fn a_stream_declared_as_plain_memory_lends_no_handle_whatever_the_allocator_made() {
    // Now the host refuses the attachment, rather than quietly lending handles the board file
    // never asked for: what the driver was told at bind stays true.
    let host = start(Arc::new(Eager));
    let mut producer = host.stream_producer(0, "plain").unwrap();
    assert_eq!(producer.buffer_kind(), BufferKind::Plain);
    assert_eq!(
        host.stream_attach(0, "plain", FIRST).err(),
        Some(Status::IO)
    );
    assert!(!host.stream_stats(0, "plain").unwrap().attached);
    let buffer = producer.next_buffer().unwrap().unwrap();
    assert!(
        producer.buffer_handle(buffer).unwrap().is_none(),
        "nobody attached, so no handle is lent"
    );
}

#[test]
fn a_stream_of_plain_memory_has_no_handles_and_says_so() {
    let host = start(Arc::new(MemfdAllocator));
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
    let mut consumer = host
        .stream_attach(0, "frames", FIRST)
        .unwrap()
        .consumer()
        .unwrap();
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
        assert!(consumer.read(buffer, 0, &mut read));
        assert_eq!(read, pixels);
        assert!(consumer.release(got.buffer));
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
        self.asked
            .lock()
            .unwrap()
            .push((name.to_string(), heap.to_string()));
        if self.empty_handed {
            return MemfdAllocator.allocate(name, layout);
        }
        // Only whole buffers: a stream of planes is refused, as it always was.
        if planes.count() > 1 {
            return Err(Status::NOT_SUPPORTED);
        }
        FileStreams { short: 0 }.allocate_planes(name, layout, heap, planes)
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
    let host = start_declared(Arc::new(MemfdAllocator), "memory = \"dma-buf\"\n");
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

/// The board file's keys for a stream of dma-bufs whose buffers are two planes.
const PLANES: &str = "memory = \"dma-buf\"\nplanes = [48, 16]\n";

#[test]
fn a_buffer_of_planes_lends_each_plane_and_write_reaches_them_end_to_end() {
    let host = start_declared(Arc::new(FileStreams { short: 0 }), PLANES);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    assert_eq!(producer.planes(), 2);
    assert_eq!(producer.plane_offset(0), Some(0));
    assert_eq!(producer.plane_offset(1), Some(48));
    assert_eq!(producer.plane_offset(2), None);
    let lease = host.stream_attach(0, "frames", FIRST).unwrap();
    assert_eq!(lease.planes.lengths(), &[48, 16]);
    let given = lease.handles.clone().unwrap();
    assert_eq!(given.len(), 2 * BUFFERS as usize);
    let mut consumer = lease.consumer().unwrap();
    let buffer = producer.next_buffer().unwrap().unwrap();
    // Each plane is its own memory, as long as the board file gives it.
    let first = hardware(producer.plane_handle(buffer, 0).unwrap().unwrap());
    let second = hardware(producer.plane_handle(buffer, 1).unwrap().unwrap());
    assert!(first.metadata().unwrap().len() >= 48);
    assert!(second.metadata().unwrap().len() >= 16);
    let whole = hardware(producer.buffer_handle(buffer).unwrap().unwrap());
    assert!(whole.metadata().unwrap().len() >= 48, "plane 0");
    assert_eq!(
        producer.plane_handle(buffer, 2).map(|_| ()),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(
        producer.plane_handle(buffer + 1, 1).map(|_| ()),
        Err(ErrorKind::InvalidInput),
        "a buffer it doesn't hold"
    );
    // Hardware writes each plane; `read` finds the second after the first.
    first.write_all_at(&[1; 4], 0).unwrap();
    second.write_all_at(&[2; 4], 0).unwrap();
    let mut out = [0; 4];
    assert_eq!(producer.read(buffer, 0, &mut out), Ok(true));
    assert_eq!(out, [1; 4]);
    assert_eq!(producer.read(buffer, 48, &mut out), Ok(true));
    assert_eq!(out, [2; 4]);
    assert_eq!(producer.write(buffer, 60, &[3; 4]), Ok(true));
    second.read_exact_at(&mut out, 12).unwrap();
    assert_eq!(out, [3; 4]);
    // The consumer was given the same planes, buffer by buffer.
    File::from(given[buffer as usize * 2 + 1].try_clone().unwrap())
        .read_exact_at(&mut out, 0)
        .unwrap();
    assert_eq!(out, [2; 4]);
    producer.publish(frame(buffer)).unwrap();
    assert_eq!(consumer.pop_frame().unwrap().unwrap().buffer, buffer);
    // Lending a plane allocates nothing.
    let held = producer.next_buffer().unwrap().unwrap();
    let ((), count) = allocations(|| {
        for _ in 0..1000 {
            assert!(producer.plane_handle(held, 1).unwrap().is_some());
        }
    });
    assert_eq!(count, 0);
}

#[test]
fn an_allocator_that_makes_only_whole_buffers_refuses_a_stream_of_planes() {
    let host = start_declared(Arc::new(Declared::default()), PLANES);
    assert_eq!(
        host.stream_attach(0, "frames", FIRST).err(),
        Some(Status::NOT_SUPPORTED)
    );
    // A stream of one plane is still asked of it as it always was.
    let host = start_declared(Arc::new(Declared::default()), DMA_BUF);
    let lease = host.stream_attach(0, "frames", FIRST).unwrap();
    assert_eq!(lease.planes, Planes::one(BYTES as u32));
}

/// The board file's keys for a stream of dma-bufs kept on standby.
const STANDBY: &str = "memory = \"dma-buf\"\nstandby = true\n";

#[test]
fn on_standby_hardware_fills_buffers_with_nobody_attached_and_the_next_consumer_gets_the_newest() {
    let host = start_declared(Arc::new(FileStreams { short: 0 }), STANDBY);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    assert!(
        !host.stream_stats(0, "frames").unwrap().standby,
        "made when asked for"
    );
    let older = producer.next_buffer().unwrap().unwrap();
    assert!(host.stream_stats(0, "frames").unwrap().standby);
    assert_eq!(
        producer.buffer_current(older),
        Ok(true),
        "memory is in place"
    );
    let into = hardware(producer.buffer_handle(older).unwrap().unwrap());
    into.write_all_at(&[1; 4], 0).unwrap();
    producer.publish(frame(older)).unwrap();
    let newer = producer.next_buffer().unwrap().unwrap();
    let into = hardware(producer.buffer_handle(newer).unwrap().unwrap());
    into.write_all_at(&[2; 4], 0).unwrap();
    producer
        .publish(StreamFrame {
            sequence: 2,
            ..frame(newer)
        })
        .unwrap();
    // The newest is kept; the one before went back to be filled again.
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!((stats.kept, stats.dropped, stats.free), (1, 1, BUFFERS - 1));
    // A buffer still being filled when the consumer comes stays current on its memory.
    let filling = producer.next_buffer().unwrap().unwrap();
    let late = hardware(producer.buffer_handle(filling).unwrap().unwrap());
    let lease = host.stream_attach(0, "frames", FIRST).unwrap();
    let given = lease.handles.clone().unwrap();
    let mut consumer = lease.consumer().unwrap();
    let got = consumer
        .pop_frame()
        .unwrap()
        .expect("the newest frame, at once");
    assert_eq!((got.buffer, got.sequence), (newer, 2));
    assert_eq!(seen(&consumer, newer), [2; 4]);
    // The consumer's memory is what hardware wrote: nothing was copied.
    let mut out = [0; 4];
    File::from(given[newer as usize].try_clone().unwrap())
        .read_exact_at(&mut out, 0)
        .unwrap();
    assert_eq!(out, [2; 4]);
    assert_eq!(producer.buffer_current(filling), Ok(true));
    late.write_all_at(&[3; 4], 0).unwrap();
    producer
        .publish(StreamFrame {
            sequence: 3,
            ..frame(filling)
        })
        .unwrap();
    assert_eq!(consumer.pop_frame().unwrap().unwrap().buffer, filling);
    assert_eq!(seen(&consumer, filling), [3; 4]);
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!((stats.kept, stats.standby, stats.delivered), (0, false, 2));
}

#[test]
fn on_standby_the_memory_a_consumer_leaves_goes_with_it_and_fresh_memory_waits() {
    let host = start_declared(Arc::new(FileStreams { short: 0 }), STANDBY);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let first = host
        .stream_attach(0, "frames", FIRST)
        .unwrap()
        .consumer()
        .unwrap();
    let buffer = producer.next_buffer().unwrap().unwrap();
    let late = hardware(producer.buffer_handle(buffer).unwrap().unwrap());
    host.stream_detach(0, "frames", FIRST).unwrap();
    assert_eq!(
        producer.buffer_current(buffer),
        Ok(false),
        "its memory went"
    );
    assert!(producer.buffer_handle(buffer).unwrap().is_none());
    producer.publish(frame(buffer)).unwrap();
    assert_eq!(host.stream_stats(0, "frames").unwrap().kept, 0, "not kept");
    // Fresh memory, which the first consumer never had.
    let fresh = producer.next_buffer().unwrap().unwrap();
    assert!(host.stream_stats(0, "frames").unwrap().standby);
    let into = hardware(producer.buffer_handle(fresh).unwrap().unwrap());
    late.write_all_at(&[0xEE; 4], 0).unwrap();
    into.write_all_at(&[5; 4], 0).unwrap();
    producer.publish(frame(fresh)).unwrap();
    let mut second = host
        .stream_attach(0, "frames", SECOND)
        .unwrap()
        .consumer()
        .unwrap();
    let got = second.pop_frame().unwrap().unwrap();
    assert_eq!(seen(&second, got.buffer), [5; 4]);
    assert_eq!(
        seen(&first, buffer),
        [0xEE; 4],
        "the late write stayed the first's"
    );
    if got.buffer == buffer {
        unreachable!("the buffer given up went back free and was taken again");
    }
    // An empty frame is a buffer given up, and is not kept for anyone.
    host.stream_detach(0, "frames", SECOND).unwrap();
    let empty = producer.next_buffer().unwrap().unwrap();
    producer
        .publish(StreamFrame {
            bytes: 0,
            ..frame(empty)
        })
        .unwrap();
    assert_eq!(host.stream_stats(0, "frames").unwrap().kept, 0);
}

#[test]
fn on_standby_plain_memory_takes_writes_with_nobody_attached() {
    let host = start_declared(Arc::new(MemfdAllocator), "standby = true\n");
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let buffer = producer.next_buffer().unwrap().unwrap();
    assert_eq!(
        producer.write(buffer, 0, &[9; 4]),
        Ok(true),
        "somewhere to land"
    );
    producer.publish(frame(buffer)).unwrap();
    // The stream beside it is not on standby, and keeps nothing.
    let mut plain = host.stream_producer(0, "plain").unwrap();
    let other = plain.next_buffer().unwrap().unwrap();
    assert_eq!(plain.write(other, 0, &[1]), Ok(false));
    let mut consumer = host
        .stream_attach(0, "frames", FIRST)
        .unwrap()
        .consumer()
        .unwrap();
    let got = consumer.pop_frame().unwrap().unwrap();
    assert_eq!(got.buffer, buffer);
    assert_eq!(seen(&consumer, buffer), [9; 4]);
}

#[test]
fn on_standby_a_thousand_frames_with_nobody_attached_allocate_nothing() {
    let host = start_declared(Arc::new(FileStreams { short: 0 }), STANDBY);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut cycle = |sequence: u64| {
        let buffer = producer.next_buffer().unwrap().unwrap();
        assert!(producer.buffer_handle(buffer).unwrap().is_some());
        producer
            .publish(StreamFrame {
                sequence,
                ..frame(buffer)
            })
            .unwrap();
    };
    cycle(0);
    let ((), count) = allocations(|| {
        for sequence in 1..1001 {
            cycle(sequence);
        }
    });
    assert_eq!(count, 0, "allocations in 1000 frames");
    let mut consumer = host
        .stream_attach(0, "frames", FIRST)
        .unwrap()
        .consumer()
        .unwrap();
    assert_eq!(consumer.pop_frame().unwrap().unwrap().sequence, 1000);
}
