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
    FrameSlot, Layout, Producer, RingError, STATE_CLOSED, WordMemory, offsets,
};

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
