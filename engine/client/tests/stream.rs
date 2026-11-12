//! Taking a stream against a service that may not follow the rules.

mod common;

use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use common::{FakeHost, StreamScript};
use lemnos_client::{Error, Session, StreamPlatform};
use lemnos_core::Status;
use lemnos_linux_stream::LinuxStreams;
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_linux_sys::eventfd;
use lemnos_linux_sys::mmap::Mapping;
use lemnos_wire::ring::{
    ByteMemory, FrameSlot, Layout, Producer, RingError, STATE_CLOSED, WordMemory, offsets,
};
use lemnos_wire::{MAX_HANDLE_BUFFERS, feature};

struct Ring(Mapping);

impl WordMemory for Ring {
    fn words(&self) -> &[AtomicU64] {
        self.0.as_atomic_words().unwrap()
    }
}

fn platform() -> Arc<dyn StreamPlatform> {
    Arc::new(LinuxStreams)
}

fn layout() -> Layout {
    Layout::new(2, 128, 1, 8).unwrap()
}

/// A good stream's three descriptors, and the host's view of its ring.
fn good() -> (Arc<dyn WordMemory>, Producer, Vec<OwnedFd>, Mapping) {
    let layout = layout();
    let ring_fd = sealed_memfd(c"ring", layout.ring_bytes() as u64).unwrap();
    let pool_fd = sealed_memfd(c"pool", layout.pool_bytes()).unwrap();
    let ring = Mapping::new(ring_fd.as_fd(), layout.ring_bytes(), 0, true).unwrap();
    let pool = Mapping::new(pool_fd.as_fd(), layout.pool_bytes() as usize, 0, true).unwrap();
    let memory: Arc<dyn WordMemory> = Arc::new(Ring(ring));
    let producer = Producer::new(Arc::clone(&memory), layout).unwrap();
    let bell = eventfd::create().unwrap();
    (memory, producer, vec![ring_fd, bell, pool_fd], pool)
}

fn host_with(layout: Layout, claimed: u8, fds: Vec<OwnedFd>) -> (FakeHost, Session) {
    let host = FakeHost::start();
    *host.control.stream.lock().unwrap() = Some(StreamScript {
        layout,
        claimed,
        fds,
    });
    let session = Session::connect(&host.path, "t").unwrap();
    (host, session)
}

#[test]
fn a_good_stream_delivers_frames_and_reads_them_from_shared_memory() {
    let (memory, mut producer, fds, pool) = good();
    let (_host, mut session) = host_with(layout(), 3, fds);
    let mut stream = session.open_stream(7, "s", platform()).unwrap();
    assert_eq!(stream.layout(), &layout());
    pool.write_bytes(4096, &[5, 6, 7]).unwrap();
    assert!(
        producer
            .push_frame(FrameSlot {
                buffer: 1,
                bytes: 3,
                sequence: 11,
                identity: 12,
                timestamp_ns: 13,
                fence: 14,
            })
            .unwrap()
    );
    let frame = stream.try_next().unwrap().unwrap();
    assert_eq!((frame.buffer(), frame.len()), (1, 3));
    assert_eq!(
        (frame.sequence(), frame.identity(), frame.timestamp_ns()),
        (11, 12, 13)
    );
    assert_eq!(stream.bytes(&frame), [5, 6, 7]);
    stream.release(frame).unwrap();
    assert_eq!(producer.pop_return().unwrap(), Some((1, 14)));
    assert_eq!(memory.words()[offsets::STATE].load(Ordering::SeqCst), 0);
}

#[test]
fn a_refusal_comes_back_as_one() {
    let host = FakeHost::start();
    let mut session = Session::connect(&host.path, "t").unwrap();
    let error = session.open_stream(1, "s", platform()).unwrap_err();
    assert!(
        matches!(&error, Error::Refused { status, .. } if *status == Status::NOT_FOUND),
        "{error:?}"
    );
}

#[test]
fn fewer_descriptors_than_claimed_is_refused_and_ends_the_connection() {
    let (_, _producer, mut fds, _pool) = good();
    fds.pop();
    let (_host, mut session) = host_with(layout(), 3, fds);
    let error = session.open_stream(1, "s", platform()).unwrap_err();
    assert_eq!(error, Error::Wire(lemnos_wire::WireError::Malformed));
    assert!(!session.is_connected());
}

#[test]
fn the_wrong_number_of_descriptors_is_refused() {
    let (_, _producer, mut fds, _pool) = good();
    fds.pop();
    let (_host, mut session) = host_with(layout(), 2, fds);
    assert_eq!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::BadHeader)
    );
}

#[test]
fn a_layout_out_of_range_is_refused() {
    let (_, _producer, fds, _pool) = good();
    let bad = Layout {
        frame_slots: 3,
        ..layout()
    };
    let (_host, mut session) = host_with(bad, 3, fds);
    assert_eq!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::BadLayout)
    );
}

#[test]
fn a_ring_descriptor_shorter_than_the_layout_is_refused_without_mapping_it() {
    let (_, _producer, mut fds, _pool) = good();
    fds[0] = sealed_memfd(c"short", 4096).unwrap();
    let (_host, mut session) = host_with(layout(), 3, fds);
    assert_eq!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::TooSmall)
    );
}

#[test]
fn a_pool_descriptor_of_the_wrong_length_is_refused() {
    let (_, _producer, mut fds, _pool) = good();
    fds[2] = sealed_memfd(c"short", 16).unwrap();
    let (_host, mut session) = host_with(layout(), 3, fds);
    assert_eq!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::TooSmall)
    );
}

#[test]
fn a_header_that_doesnt_match_is_refused() {
    let (memory, _producer, fds, _pool) = good();
    memory.words()[offsets::MAGIC].store(1, Ordering::SeqCst);
    let (_host, mut session) = host_with(layout(), 3, fds);
    assert_eq!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::BadHeader)
    );
}

#[test]
fn a_producer_gone_bad_is_an_error_on_the_stream_and_the_session_is_fine() {
    let (memory, _producer, fds, _pool) = good();
    let (_host, mut session) = host_with(layout(), 3, fds);
    let mut stream = session.open_stream(1, "s", platform()).unwrap();
    memory.words()[offsets::FRAMES_HEAD].store(u64::MAX, Ordering::SeqCst);
    assert_eq!(
        stream.try_next().unwrap_err(),
        Error::Stream(RingError::BadIndex)
    );
    assert_eq!(
        stream.try_next().unwrap_err(),
        Error::Stream(RingError::Broken)
    );
    assert!(session.list().is_ok());
}

#[test]
fn a_frame_naming_a_buffer_outside_the_pool_is_an_error_and_never_read() {
    let (memory, _producer, fds, _pool) = good();
    let (_host, mut session) = host_with(layout(), 3, fds);
    let mut stream = session.open_stream(1, "s", platform()).unwrap();
    memory.words()[offsets::FRAMES].store(1 << 20, Ordering::SeqCst);
    memory.words()[offsets::FRAMES_HEAD].store(1, Ordering::SeqCst);
    assert_eq!(
        stream.try_next().unwrap_err(),
        Error::Stream(RingError::BadBuffer)
    );
}

#[test]
fn waiting_ends_when_the_stream_ends_or_time_passes() {
    let (_, producer, fds, _pool) = good();
    let (_host, mut session) = host_with(layout(), 3, fds);
    let mut stream = session.open_stream(1, "s", platform()).unwrap();
    assert_eq!(
        stream.next_frame(Duration::from_millis(5)).unwrap(),
        None,
        "nothing published"
    );
    producer.close(STATE_CLOSED);
    assert!(!stream.is_open());
    assert_eq!(
        stream.next_frame(Duration::from_secs(30)).unwrap(),
        None,
        "doesn't wait out the timeout"
    );
}

#[test]
fn the_stream_stays_usable_after_the_session_is_dropped() {
    let (_, mut producer, fds, _pool) = good();
    let (_host, mut session) = host_with(layout(), 3, fds);
    let mut stream = session.open_stream(1, "s", platform()).unwrap();
    drop(session);
    assert!(
        producer
            .push_frame(FrameSlot {
                buffer: 0,
                bytes: 0,
                sequence: 1,
                identity: 2,
                timestamp_ns: 3,
                fence: 4,
            })
            .unwrap()
    );
    let frame = stream.next_frame(Duration::from_secs(5)).unwrap().unwrap();
    assert_eq!(frame.identity(), 2);
    assert!(frame.is_empty());
    stream.release(frame).unwrap();
}

// Streams whose buffers are a descriptor each. Sealed memfds stand in for the dma-bufs.

const KIND_STREAM_OPEN: u16 = 17;
const KIND_STREAM_OPEN_HANDLES: u16 = 22;

/// A good stream of `buffers` buffers of 128 bytes, each its own descriptor a page long as a
/// heap would round it: the host's ring, the descriptors in the order they travel, and the
/// host's mapping of each buffer.
fn good_handles(buffers: u32) -> (Layout, Producer, Vec<OwnedFd>, Vec<Mapping>) {
    let layout = Layout::new(buffers, 128, 1, 8).unwrap();
    let ring_fd = sealed_memfd(c"ring", layout.ring_bytes() as u64).unwrap();
    let ring = Mapping::new(ring_fd.as_fd(), layout.ring_bytes(), 0, true).unwrap();
    let producer = Producer::new(Arc::new(Ring(ring)), layout).unwrap();
    let mut fds = vec![ring_fd, eventfd::create().unwrap()];
    let mut maps = Vec::new();
    for _ in 0..buffers {
        let fd = sealed_memfd(c"buffer", 4096).unwrap();
        maps.push(Mapping::new(fd.as_fd(), 4096, 0, true).unwrap());
        fds.push(fd);
    }
    (layout, producer, fds, maps)
}

/// A service that says it has streams of handles and answers the next stream with one.
fn handles_host(layout: Layout, claimed: u8, fds: Vec<OwnedFd>) -> (FakeHost, Session) {
    let host = FakeHost::start();
    host.control
        .features
        .store(feature::STREAM_HANDLES, Ordering::SeqCst);
    host.control.answer_handles.store(true, Ordering::SeqCst);
    *host.control.stream.lock().unwrap() = Some(StreamScript {
        layout,
        claimed,
        fds,
    });
    let session = Session::connect(&host.path, "t").unwrap();
    (host, session)
}

fn opens(host: &FakeHost) -> Vec<u16> {
    host.control.opens.lock().unwrap().clone()
}

fn closes(host: &FakeHost) -> u32 {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let closes = host.control.stream_closes.load(Ordering::SeqCst);
        if closes > 0 || std::time::Instant::now() >= deadline {
            return closes;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn slot(buffer: u32, bytes: u32) -> FrameSlot {
    FrameSlot {
        buffer,
        bytes,
        sequence: u64::from(buffer),
        identity: 0,
        timestamp_ns: 0,
        fence: 1,
    }
}

#[test]
fn a_service_from_before_handles_is_asked_the_old_way_and_its_streams_still_open() {
    let (_, _producer, fds, _pool) = good();
    // `features` is 0, as every service before this addition sent it.
    let (host, mut session) = host_with(layout(), 3, fds);
    let stream = session.open_stream(7, "s", platform()).unwrap();
    assert!(!stream.has_buffer_handles());
    // To such a service the new kind is an unknown one, which would end the connection.
    assert_eq!(opens(&host), [KIND_STREAM_OPEN]);
}

#[test]
fn a_service_with_handles_is_asked_the_new_way_and_still_answers_shared_memory_as_before() {
    let (_, mut producer, fds, pool) = good();
    let (host, mut session) = handles_host(layout(), 3, fds);
    host.control.answer_handles.store(false, Ordering::SeqCst);
    let mut stream = session.open_stream(7, "s", platform()).unwrap();
    assert_eq!(opens(&host), [KIND_STREAM_OPEN_HANDLES]);
    assert!(!stream.has_buffer_handles());
    pool.write_bytes(4096, &[5, 6, 7]).unwrap();
    assert!(producer.push_frame(slot(1, 3)).unwrap());
    let frame = stream.try_next().unwrap().unwrap();
    assert_eq!(stream.bytes(&frame), [5, 6, 7]);
}

#[test]
fn a_stream_of_handles_reads_each_frame_from_its_own_buffers_memory() {
    let (layout, mut producer, fds, maps) = good_handles(3);
    let (host, mut session) = handles_host(layout, 5, fds);
    let mut stream = session.open_stream(7, "s", platform()).unwrap();
    assert_eq!(opens(&host), [KIND_STREAM_OPEN_HANDLES]);
    assert!(stream.has_buffer_handles());
    assert_eq!(stream.layout(), &layout);
    for (buffer, map) in maps.iter().enumerate() {
        map.write_bytes(0, &[buffer as u8 + 10; 128]).unwrap();
    }
    // Out of order, so a frame is read from the buffer it names and not the next one along.
    for buffer in [2, 0, 1] {
        assert!(producer.push_frame(slot(buffer, 128)).unwrap());
        let frame = stream.try_next().unwrap().unwrap();
        assert_eq!(frame.buffer(), buffer);
        assert_eq!(stream.bytes(&frame), [buffer as u8 + 10; 128]);
        stream.release(frame).unwrap();
        assert_eq!(producer.pop_return().unwrap(), Some((buffer, 1)));
    }
    // A frame is never read past its buffer's own bytes, whatever the heap rounded up to.
    assert!(producer.push_frame(slot(0, 64)).unwrap());
    let frame = stream.try_next().unwrap().unwrap();
    assert_eq!(stream.bytes(&frame).len(), 64);
    assert_eq!(host.control.stream_closes.load(Ordering::SeqCst), 0);
}

#[test]
fn a_stream_at_the_limit_of_buffers_arrives_whole_in_one_message() {
    let (layout, mut producer, fds, maps) = good_handles(MAX_HANDLE_BUFFERS);
    assert_eq!(fds.len(), 130);
    let (_host, mut session) = handles_host(layout, 130, fds);
    let mut stream = session.open_stream(7, "s", platform()).unwrap();
    let last = MAX_HANDLE_BUFFERS - 1;
    maps[last as usize].write_bytes(0, &[9, 9]).unwrap();
    assert!(producer.push_frame(slot(last, 2)).unwrap());
    let frame = stream.try_next().unwrap().unwrap();
    assert_eq!(stream.bytes(&frame), [9, 9]);
}

/// What opening a stream of handles comes to when the answer's numbers are `claimed`
/// descriptors for `layout`, with `fds` really sent.
fn refused(layout: Layout, claimed: u8, fds: Vec<OwnedFd>) -> (Error, bool) {
    let (_host, mut session) = handles_host(layout, claimed, fds);
    let error = session.open_stream(1, "s", platform()).unwrap_err();
    (error, session.is_connected())
}

const MALFORMED: Error = Error::Wire(lemnos_wire::WireError::Malformed);

#[test]
fn a_count_of_descriptors_that_isnt_two_more_than_the_buffers_is_refused() {
    // One too many claimed and sent: the count agrees with what came, and not with the buffers.
    let (layout, _producer, mut fds, _maps) = good_handles(3);
    fds.push(sealed_memfd(c"extra", 4096).unwrap());
    assert_eq!(refused(layout, 6, fds), (MALFORMED, false));
    // One too few.
    let (layout, _producer, mut fds, _maps) = good_handles(3);
    fds.pop();
    assert_eq!(refused(layout, 4, fds), (MALFORMED, false));
}

#[test]
fn fewer_or_more_descriptors_than_the_answer_claims_is_refused() {
    let (layout, _producer, mut fds, _maps) = good_handles(3);
    fds.pop();
    assert_eq!(refused(layout, 5, fds), (MALFORMED, false));
    let (layout, _producer, mut fds, _maps) = good_handles(3);
    fds.push(sealed_memfd(c"extra", 4096).unwrap());
    assert_eq!(refused(layout, 5, fds), (MALFORMED, false));
}

#[test]
fn more_buffers_than_one_message_carries_is_refused_whatever_else_agrees() {
    // 129 buffers, 131 descriptors claimed and 131 sent: consistent, and over the limit.
    let (layout, _producer, fds, _maps) = good_handles(MAX_HANDLE_BUFFERS + 1);
    assert_eq!(fds.len(), 131);
    assert_eq!(refused(layout, 131, fds), (MALFORMED, false));
}

#[test]
fn no_buffers_is_refused() {
    let (layout, _producer, mut fds, _maps) = good_handles(1);
    fds.pop();
    let none = Layout {
        buffers: 0,
        ..layout
    };
    assert_eq!(refused(none, 2, fds), (MALFORMED, false));
}

#[test]
fn handles_nobody_asked_for_are_refused() {
    // The service never said it has streams of handles, so it was asked the old way, and
    // answers with handles all the same.
    let (layout, _producer, fds, _maps) = good_handles(3);
    let host = FakeHost::start();
    host.control.answer_handles.store(true, Ordering::SeqCst);
    *host.control.stream.lock().unwrap() = Some(StreamScript {
        layout,
        claimed: 5,
        fds,
    });
    let mut session = Session::connect(&host.path, "t").unwrap();
    assert_eq!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        MALFORMED
    );
    assert_eq!(opens(&host), [KIND_STREAM_OPEN]);
    assert!(!session.is_connected());
}

#[test]
fn a_buffer_shorter_than_the_layout_says_is_refused_unmapped_and_the_stream_given_back() {
    let (_, _producer, mut fds, _maps) = good_handles(3);
    fds[3] = sealed_memfd(c"short", 4096).unwrap();
    let long = Layout::new(3, 8192, 1, 8).unwrap();
    let ring_fd = sealed_memfd(c"ring", long.ring_bytes() as u64).unwrap();
    fds[0] = ring_fd;
    for fd in [2, 4] {
        fds[fd] = sealed_memfd(c"buffer", 8192).unwrap();
    }
    let (host, mut session) = handles_host(long, 5, fds);
    assert_eq!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::TooSmall)
    );
    assert_eq!(closes(&host), 1, "the service was told the stream is free");
    assert!(session.list().is_ok(), "and the connection carries on");
}

#[test]
fn a_buffer_that_could_still_shrink_is_refused() {
    let (layout, _producer, mut fds, _maps) = good_handles(2);
    let unsealed = lemnos_linux_sys::dma_buf::memfd(c"unsealed").unwrap();
    std::fs::File::from(unsealed.try_clone().unwrap())
        .set_len(4096)
        .unwrap();
    fds[2] = unsealed;
    let (_host, mut session) = handles_host(layout, 4, fds);
    assert_eq!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::TooSmall)
    );
}

/// A platform from before streams of handles: everything but `map_buffer`, which it leaves
/// as the trait has it.
#[derive(Debug)]
struct Older;

impl StreamPlatform for Older {
    fn recv_with_descriptors(
        &self,
        socket: std::os::fd::BorrowedFd<'_>,
        buf: &mut [u8],
        fds: &mut Vec<OwnedFd>,
    ) -> std::io::Result<usize> {
        LinuxStreams.recv_with_descriptors(socket, buf, fds)
    }

    fn map_ring(
        &self,
        fd: std::os::fd::BorrowedFd<'_>,
        bytes: usize,
    ) -> std::io::Result<Arc<dyn WordMemory>> {
        StreamPlatform::map_ring(&LinuxStreams, fd, bytes)
    }

    fn map_buffers(
        &self,
        fd: std::os::fd::BorrowedFd<'_>,
        bytes: usize,
        writable: bool,
    ) -> std::io::Result<Box<dyn ByteMemory>> {
        StreamPlatform::map_buffers(&LinuxStreams, fd, bytes, writable)
    }

    fn wait_doorbell(
        &self,
        fd: std::os::fd::BorrowedFd<'_>,
        timeout: Duration,
    ) -> std::io::Result<bool> {
        LinuxStreams.wait_doorbell(fd, timeout)
    }
}

#[test]
fn a_platform_that_cant_map_one_buffer_fails_cleanly_and_gives_the_stream_back() {
    let (layout, _producer, fds, _maps) = good_handles(3);
    let (host, mut session) = handles_host(layout, 5, fds);
    assert_eq!(
        session.open_stream(1, "s", Arc::new(Older)).unwrap_err(),
        Error::StreamUnsupported
    );
    assert_eq!(closes(&host), 1);
    // The answer to giving it back is nobody's, and is not handed to whoever asks next.
    assert_eq!(session.list().unwrap().len(), 2);
    assert!(session.is_connected());
    // Shared memory still works on such a platform.
    let (_, _producer, fds, _pool) = good();
    host.control.answer_handles.store(false, Ordering::SeqCst);
    *host.control.stream.lock().unwrap() = Some(StreamScript {
        layout: self::layout(),
        claimed: 3,
        fds,
    });
    assert!(session.open_stream(1, "s", Arc::new(Older)).is_ok());
}

// Streams the consumer fills.

#[test]
fn a_stream_taken_to_be_filled_takes_writes_and_the_producer_sees_them() {
    let (_memory, _producer, fds, pool) = good();
    let (_host, mut session) = host_with(layout(), 3, fds);
    let mut stream = session.open_stream_writable(7, "s", platform()).unwrap();
    assert!(stream.is_writable());
    stream.write(1, 5, &[1, 2, 3]).unwrap();
    assert_eq!(&pool.as_slice()[4096 + 5..4096 + 8], [1, 2, 3]);
    assert_eq!(&pool.as_slice()[5..8], [0, 0, 0], "buffer 0 is untouched");
    // And reads back what the producer left in a buffer that is no frame.
    pool.write_bytes(100, &[9; 28]).unwrap();
    let mut out = [0u8; 28];
    stream.read(0, 100, &mut out).unwrap();
    assert_eq!(out, [9; 28]);
}

#[test]
fn a_write_outside_a_buffer_is_refused_and_lands_nowhere() {
    let (_memory, _producer, fds, pool) = good();
    let (_host, mut session) = host_with(layout(), 3, fds);
    let mut stream = session.open_stream_writable(7, "s", platform()).unwrap();
    let before = pool.as_slice().to_vec();
    // The layout has two buffers of 128 bytes, a page apart.
    for (buffer, offset, len) in [
        (2, 0, 1),
        (u32::MAX, 0, 1),
        (0, 128, 1),
        (0, 127, 2),
        (1, 0, 129),
        (0, usize::MAX, 2),
    ] {
        let data = vec![0xEE; len];
        assert!(
            matches!(stream.write(buffer, offset, &data), Err(Error::Stream(_))),
            "{buffer} {offset} {len}"
        );
        let mut out = vec![0; len];
        assert!(
            matches!(stream.read(buffer, offset, &mut out), Err(Error::Stream(_))),
            "{buffer} {offset} {len}"
        );
    }
    assert!(pool.as_slice() == before.as_slice(), "nothing was written");
    // The last byte of a buffer is inside it.
    stream.write(1, 127, &[4]).unwrap();
    assert_eq!(pool.as_slice()[4096 + 127], 4);
    assert!(session.is_connected());
}

#[test]
fn a_stream_taken_to_be_read_refuses_a_write() {
    let (_memory, _producer, fds, pool) = good();
    let (_host, mut session) = host_with(layout(), 3, fds);
    let mut stream = session.open_stream(7, "s", platform()).unwrap();
    assert!(!stream.is_writable());
    assert_eq!(stream.write(0, 0, &[1]), Err(Error::StreamReadOnly));
    assert_eq!(pool.as_slice()[0], 0);
    assert_eq!(Status::from(Error::StreamReadOnly), Status::ACCESS);
    // Reading a buffer needs nothing more.
    pool.write_bytes(4096, &[3]).unwrap();
    let mut out = [0u8; 1];
    stream.read(1, 0, &mut out).unwrap();
    assert_eq!(out, [3]);
}

#[test]
fn a_stream_of_handles_is_filled_buffer_by_buffer_within_each_buffers_own_bytes() {
    let (layout, _producer, fds, maps) = good_handles(3);
    let (_host, mut session) = handles_host(layout, 5, fds);
    let mut stream = session.open_stream_writable(7, "s", platform()).unwrap();
    assert!(stream.has_buffer_handles() && stream.is_writable());
    stream.write(2, 120, &[7; 8]).unwrap();
    assert_eq!(&maps[2].as_slice()[120..128], [7; 8]);
    assert!(maps[0].as_slice().iter().all(|b| *b == 0));
    assert!(maps[1].as_slice().iter().all(|b| *b == 0));
    // The descriptor is a page long, as a heap rounds it; the buffer is its first 128 bytes.
    assert!(matches!(
        stream.write(2, 121, &[7; 8]),
        Err(Error::Stream(_))
    ));
    assert_eq!(maps[2].as_slice()[128], 0);
    assert!(matches!(stream.write(3, 0, &[1]), Err(Error::Stream(_))));
    let mut out = [0u8; 8];
    stream.read(2, 120, &mut out).unwrap();
    assert_eq!(out, [7; 8]);
}

/// What a buffer's memory was told, in order: which buffer, and what.
type Calls = Arc<std::sync::Mutex<Vec<(usize, &'static str)>>>;

/// One buffer's memory, saying what is done to it.
struct Told {
    inner: Box<dyn ByteMemory>,
    buffer: usize,
    calls: Calls,
}

impl Told {
    fn say(&self, what: &'static str) {
        self.calls.lock().unwrap().push((self.buffer, what));
    }
}

impl ByteMemory for Told {
    fn bytes(&self) -> &[u8] {
        self.inner.bytes()
    }

    fn write(&self, offset: usize, data: &[u8]) -> bool {
        self.say("write");
        self.inner.write(offset, data)
    }

    fn begin_read(&self) {
        self.say("begin read");
    }

    fn end_read(&self) {
        self.say("end read");
    }
}

/// The Linux platform, with each buffer's memory wrapped in a [`Told`], numbered as mapped.
#[derive(Debug, Default)]
struct Telling {
    calls: Calls,
    mapped: std::sync::atomic::AtomicUsize,
    writable: std::sync::Mutex<Vec<bool>>,
}

impl StreamPlatform for Telling {
    fn recv_with_descriptors(
        &self,
        socket: std::os::fd::BorrowedFd<'_>,
        buf: &mut [u8],
        fds: &mut Vec<OwnedFd>,
    ) -> std::io::Result<usize> {
        LinuxStreams.recv_with_descriptors(socket, buf, fds)
    }

    fn map_ring(
        &self,
        fd: std::os::fd::BorrowedFd<'_>,
        bytes: usize,
    ) -> std::io::Result<Arc<dyn WordMemory>> {
        StreamPlatform::map_ring(&LinuxStreams, fd, bytes)
    }

    fn map_buffers(
        &self,
        fd: std::os::fd::BorrowedFd<'_>,
        bytes: usize,
        writable: bool,
    ) -> std::io::Result<Box<dyn ByteMemory>> {
        self.writable.lock().unwrap().push(writable);
        StreamPlatform::map_buffers(&LinuxStreams, fd, bytes, writable)
    }

    fn map_buffer(
        &self,
        fd: std::os::fd::BorrowedFd<'_>,
        bytes: usize,
        writable: bool,
    ) -> std::io::Result<Box<dyn ByteMemory>> {
        self.writable.lock().unwrap().push(writable);
        Ok(Box::new(Told {
            inner: StreamPlatform::map_buffer(&LinuxStreams, fd, bytes, writable)?,
            buffer: self.mapped.fetch_add(1, Ordering::SeqCst),
            calls: Arc::clone(&self.calls),
        }))
    }

    fn wait_doorbell(
        &self,
        fd: std::os::fd::BorrowedFd<'_>,
        timeout: Duration,
    ) -> std::io::Result<bool> {
        LinuxStreams.wait_doorbell(fd, timeout)
    }
}

#[test]
fn a_buffer_of_its_own_is_told_when_the_consumer_reads_and_writes_it() {
    let (layout, mut producer, fds, _maps) = good_handles(3);
    let (_host, mut session) = handles_host(layout, 5, fds);
    let telling = Arc::new(Telling::default());
    let mut stream = session
        .open_stream_writable(7, "s", Arc::clone(&telling) as _)
        .unwrap();
    assert_eq!(*telling.writable.lock().unwrap(), [true; 3]);
    let told = || std::mem::take(&mut *telling.calls.lock().unwrap());
    assert!(told().is_empty(), "mapping a buffer is no access");

    // A frame is read in place from the moment it is taken until it is given back.
    assert!(producer.push_frame(slot(1, 128)).unwrap());
    let frame = stream.try_next().unwrap().unwrap();
    assert_eq!(told(), [(1, "begin read")]);
    assert_eq!(stream.bytes(&frame).len(), 128);
    assert!(told().is_empty());
    stream.release(frame).unwrap();
    assert_eq!(told(), [(1, "end read")]);
    assert_eq!(stream.try_next().unwrap(), None);
    assert!(told().is_empty(), "no frame, no access");

    // A write goes to the buffer's memory, which brackets it itself; a copy out is bracketed.
    stream.write(2, 0, &[1, 2]).unwrap();
    assert_eq!(told(), [(2, "write")]);
    let mut out = [0u8; 2];
    stream.read(2, 0, &mut out).unwrap();
    assert_eq!(told(), [(2, "begin read"), (2, "end read")]);
    assert_eq!(out, [1, 2]);
    // What is refused touches nothing.
    assert!(stream.write(2, 127, &[0; 2]).is_err());
    assert!(stream.read(3, 0, &mut out).is_err());
    assert!(told().is_empty());
}

#[test]
fn a_stream_taken_to_be_read_is_mapped_read_only() {
    let (layout, _producer, fds, _maps) = good_handles(2);
    let (_host, mut session) = handles_host(layout, 4, fds);
    let telling = Arc::new(Telling::default());
    let mut stream = session
        .open_stream(7, "s", Arc::clone(&telling) as _)
        .unwrap();
    assert_eq!(*telling.writable.lock().unwrap(), [false; 2]);
    assert_eq!(stream.write(0, 0, &[1]), Err(Error::StreamReadOnly));
    assert!(telling.calls.lock().unwrap().is_empty());

    let (_memory, _producer, fds, _pool) = good();
    let (_host, mut session) = host_with(self::layout(), 3, fds);
    let telling = Arc::new(Telling::default());
    session
        .open_stream(7, "s", Arc::clone(&telling) as _)
        .unwrap();
    assert_eq!(*telling.writable.lock().unwrap(), [false]);
}
