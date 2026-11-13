//! Taking a stream against a service that may not follow the rules.

mod common;

use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{FakeHost, StreamHost, StreamScript};
use lemnos_client::{Access, Error, Session, StreamPlatform};
use lemnos_core::Status;
use lemnos_linux_stream::LinuxStreams;
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_wire::ring::{Layout, RingError, STATE_CLOSED, fds};
use lemnos_wire::{MAX_HANDLE_BUFFERS, feature};

fn platform() -> Arc<dyn StreamPlatform> {
    Arc::new(LinuxStreams)
}

fn layout() -> Layout {
    Layout::new(2, 128, 1, 8).unwrap()
}

/// A good shared-memory stream's host and the descriptors it sends: eight of them.
fn good() -> (StreamHost, Vec<OwnedFd>) {
    StreamHost::make(layout(), &[], true)
}

fn host_with(layout: Layout, claimed: u8, fds: Vec<OwnedFd>) -> (FakeHost, Session) {
    let host = FakeHost::start();
    host.control
        .features
        .store(feature::SHARED_RINGS, Ordering::SeqCst);
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
    let (mut host, fds) = good();
    let (_fake, mut session) = host_with(layout(), 8, fds);
    let mut stream = session.open_stream(7, "s", platform()).unwrap();
    assert_eq!(stream.layout(), &layout());
    // The buffer length the stream open carried, for a consumer to size and check frames by.
    assert_eq!(stream.buffer_len(), 128);
    assert_eq!(stream.buffer_count(), 2);
    assert!(host.write(1, 0, &[5, 6, 7]));
    assert!(host.push(1, 3, 11, 12, 13));
    let frame = stream.try_next().unwrap().unwrap();
    assert_eq!((frame.buffer(), frame.len()), (1, 3));
    assert_eq!(
        (frame.sequence(), frame.identity(), frame.timestamp_ns()),
        (11, 12, 13)
    );
    assert_eq!(stream.bytes(&frame), [5, 6, 7]);
    assert!(
        !host.reclaim(1),
        "a frame held by the consumer keeps its buffer"
    );
    stream.release(frame).unwrap();
    assert!(host.reclaim(1), "a released frame's buffer comes back");
    assert!(stream.is_open());
}

#[test]
fn a_refusal_comes_back_as_one() {
    let host = FakeHost::start();
    host.control
        .features
        .store(feature::SHARED_RINGS, Ordering::SeqCst);
    let mut session = Session::connect(&host.path, "t").unwrap();
    let error = session.open_stream(1, "s", platform()).unwrap_err();
    assert!(
        matches!(&error, Error::Refused { status, .. } if *status == Status::NOT_FOUND),
        "{error:?}"
    );
}

#[test]
fn fewer_descriptors_than_claimed_is_refused_and_ends_the_connection() {
    let (_, mut fds) = good();
    fds.pop();
    let (_fake, mut session) = host_with(layout(), 8, fds);
    let error = session.open_stream(1, "s", platform()).unwrap_err();
    assert_eq!(error, Error::Wire(lemnos_wire::WireError::Malformed));
    assert!(!session.is_connected());
}

#[test]
fn the_wrong_number_of_descriptors_is_refused() {
    let (_, mut fds) = good();
    fds.pop();
    // Seven sent and claimed, where a shared stream needs eight.
    let (_fake, mut session) = host_with(layout(), 7, fds);
    assert_eq!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::BadLayout)
    );
}

#[test]
fn a_layout_out_of_range_is_refused() {
    let (_, fds) = good();
    let bad = Layout {
        frame_slots: 3,
        ..layout()
    };
    let (_fake, mut session) = host_with(bad, 8, fds);
    assert_eq!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::BadLayout)
    );
}

#[test]
fn a_ring_descriptor_shorter_than_the_layout_is_refused_without_mapping_it() {
    let (_, mut fds) = good();
    fds[fds::RING] = sealed_memfd(c"short", 4096).unwrap();
    let (_fake, mut session) = host_with(layout(), 8, fds);
    assert!(matches!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::Shm(_))
    ));
}

#[test]
fn a_pool_descriptor_of_the_wrong_length_is_refused() {
    let (_, mut fds) = good();
    fds[fds::BUFFERS] = sealed_memfd(c"short", 16).unwrap();
    let (_fake, mut session) = host_with(layout(), 8, fds);
    assert!(matches!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::Shm(_))
    ));
}

#[test]
fn a_header_that_doesnt_match_is_refused() {
    // A descriptor of the right size that is not a ring: the shm crate checks the header.
    let (_, mut fds) = good();
    fds[fds::RING] = sealed_memfd(c"blank", 1 << 16).unwrap();
    let (_fake, mut session) = host_with(layout(), 8, fds);
    assert!(matches!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::Shm(_))
    ));
}

#[test]
fn a_producer_gone_bad_is_an_error_on_the_stream_and_the_session_is_fine() {
    let (mut host, fds) = good();
    let (_fake, mut session) = host_with(layout(), 8, fds);
    let mut stream = session.open_stream(1, "s", platform()).unwrap();
    // A frame whose payload is no frame at all. The reader counts it as lost, as it does a frame
    // the ring overran: it is skipped, not an error. (A second writer on the ring would fight the
    // host's own, so nothing is published after it.)
    assert!(host.push_raw(&[0xFF; 3]));
    assert!(stream.try_next().unwrap().is_none());
    assert!(stream.is_open());
    assert!(session.list().is_ok());
}

#[test]
fn a_frame_naming_a_buffer_outside_the_pool_is_an_error_and_never_read() {
    let (mut host, fds) = good();
    let (_fake, mut session) = host_with(layout(), 8, fds);
    let mut stream = session.open_stream(1, "s", platform()).unwrap();
    let mut payload = [0u8; 64];
    let n = shm_ring_buffers::entry::write(
        &mut payload,
        1,
        &[(shm_ring_buffers::BufferIndex(1 << 20), 0)],
        &[0; lemnos_wire::ring::FRAME_BYTES],
    )
    .unwrap();
    assert!(host.push_raw(&payload[..n]));
    // The entry is lost, not read: nothing is handed out, and the session carries on.
    assert!(stream.try_next().unwrap().is_none());
    assert!(session.is_connected());
}

#[test]
fn waiting_ends_when_the_stream_ends_or_time_passes() {
    let (mut host, fds) = good();
    let (_fake, mut session) = host_with(layout(), 8, fds);
    let mut stream = session.open_stream(1, "s", platform()).unwrap();
    assert!(
        stream
            .next_frame(Duration::from_millis(5))
            .unwrap()
            .is_none(),
        "nothing published"
    );
    host.end(STATE_CLOSED);
    assert!(!stream.is_open());
    assert!(
        stream
            .next_frame(Duration::from_secs(30))
            .unwrap()
            .is_none(),
        "doesn't wait out the timeout"
    );
}

#[test]
fn the_stream_stays_usable_after_the_session_is_dropped() {
    let (mut host, fds) = good();
    let (_fake, mut session) = host_with(layout(), 8, fds);
    let mut stream = session.open_stream(1, "s", platform()).unwrap();
    drop(session);
    assert!(host.push(0, 0, 1, 2, 3));
    let frame = stream.next_frame(Duration::from_secs(5)).unwrap().unwrap();
    assert_eq!(frame.identity(), 2);
    assert!(frame.is_empty());
    stream.release(frame).unwrap();
}

// Streams whose buffers are a descriptor each. Sealed memfds stand in for the dma-bufs.

const KIND_STREAM_OPEN: u16 = 17;
const KIND_STREAM_OPEN_HANDLES: u16 = 22;

/// What a stream of `buffers` buffers and `planes` planes is claimed to send: the base
/// descriptors, then one per plane of each buffer.
fn claimed_for(buffers: u32, planes: u32) -> u8 {
    (fds::BUFFERS as u32 + buffers * planes) as u8
}

/// A good stream of `buffers` buffers of 128 bytes, each its own descriptor a page long as a heap
/// would round it: the host, and the descriptors in the order they travel.
fn good_handles(buffers: u32) -> (Layout, StreamHost, Vec<OwnedFd>) {
    let layout = Layout::new(buffers, 128, 1, 8).unwrap();
    let (host, fds) = StreamHost::make(layout, &[], false);
    (layout, host, fds)
}

/// A service that says it has streams of handles and answers the next stream with one.
fn handles_host(layout: Layout, claimed: u8, fds: Vec<OwnedFd>) -> (FakeHost, Session) {
    let host = FakeHost::start();
    host.control.features.store(
        feature::STREAM_HANDLES | feature::SHARED_RINGS,
        Ordering::SeqCst,
    );
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

#[test]
fn a_service_from_before_handles_is_asked_the_old_way_and_its_streams_still_open() {
    let (_, fds) = good();
    let (host, mut session) = host_with(layout(), 8, fds);
    // `features` is 0, as every service before this addition sent it.
    host.control.features.store(0, Ordering::SeqCst);
    let stream = session.open_stream(7, "s", platform()).unwrap();
    assert!(!stream.has_buffer_handles());
    // To such a service the new kind is an unknown one, which would end the connection.
    assert_eq!(opens(&host), [KIND_STREAM_OPEN]);
}

#[test]
fn a_service_with_handles_is_asked_the_new_way_and_still_answers_shared_memory_as_before() {
    let (mut host, fds) = good();
    let (fake, mut session) = handles_host(layout(), 8, fds);
    fake.control.answer_handles.store(false, Ordering::SeqCst);
    let mut stream = session.open_stream(7, "s", platform()).unwrap();
    assert_eq!(opens(&fake), [KIND_STREAM_OPEN_HANDLES]);
    assert!(!stream.has_buffer_handles());
    assert!(host.write(1, 0, &[5, 6, 7]));
    assert!(host.push(1, 3, 1, 0, 0));
    let frame = stream.try_next().unwrap().unwrap();
    assert_eq!(stream.bytes(&frame), [5, 6, 7]);
}

#[test]
fn a_stream_of_handles_reads_each_frame_from_its_own_buffers_memory() {
    let (layout, mut host, fds) = good_handles(3);
    let (fake, mut session) = handles_host(layout, claimed_for(3, 1), fds);
    let mut stream = session.open_stream(7, "s", platform()).unwrap();
    assert_eq!(opens(&fake), [KIND_STREAM_OPEN_HANDLES]);
    assert!(stream.has_buffer_handles());
    assert_eq!(stream.layout(), &layout);
    assert_eq!(stream.buffer_len(), 128);
    assert_eq!(stream.buffer_count(), 3);
    for buffer in 0..3 {
        assert!(host.write(buffer, 0, &[buffer as u8 + 10; 128]));
    }
    // Out of order, so a frame is read from the buffer it names and not the next one along.
    for buffer in [2, 0, 1] {
        assert!(host.push(buffer, 128, u64::from(buffer), 0, 0));
        let frame = stream.try_next().unwrap().unwrap();
        assert_eq!(frame.buffer(), buffer);
        assert_eq!(stream.bytes(&frame), [buffer as u8 + 10; 128]);
        stream.release(frame).unwrap();
        assert!(host.reclaim(buffer));
    }
    // A frame is never read past its buffer's own bytes, whatever the heap rounded up to.
    assert!(host.push(0, 64, 9, 0, 0));
    let frame = stream.try_next().unwrap().unwrap();
    assert_eq!(stream.bytes(&frame).len(), 64);
    assert_eq!(fake.control.stream_closes.load(Ordering::SeqCst), 0);
}

#[test]
fn a_stream_at_the_limit_of_buffers_arrives_whole_in_one_message() {
    let (layout, mut host, fds) = good_handles(MAX_HANDLE_BUFFERS);
    assert_eq!(fds.len(), fds::BUFFERS + MAX_HANDLE_BUFFERS as usize);
    let (_fake, mut session) = handles_host(layout, claimed_for(MAX_HANDLE_BUFFERS, 1), fds);
    let mut stream = session.open_stream(7, "s", platform()).unwrap();
    let last = MAX_HANDLE_BUFFERS - 1;
    assert!(host.write(last, 0, &[9, 9]));
    assert!(host.push(last, 2, 1, 0, 0));
    let frame = stream.try_next().unwrap().unwrap();
    assert_eq!(stream.bytes(&frame), [9, 9]);
}

/// What opening a stream of handles comes to when the answer's numbers are `claimed`
/// descriptors for `layout`, with `fds` really sent.
fn refused(layout: Layout, claimed: u8, fds: Vec<OwnedFd>) -> (Error, bool) {
    let (_fake, mut session) = handles_host(layout, claimed, fds);
    let error = session.open_stream(1, "s", platform()).unwrap_err();
    (error, session.is_connected())
}

const MALFORMED: Error = Error::Wire(lemnos_wire::WireError::Malformed);

fn extra() -> OwnedFd {
    sealed_memfd(c"extra", 4096).unwrap()
}

#[test]
fn a_count_of_descriptors_that_isnt_two_more_than_the_buffers_is_refused() {
    // One too many claimed and sent: the count agrees with what came, and not with the buffers.
    let (layout, _host, mut fds) = good_handles(3);
    fds.push(extra());
    assert_eq!(
        refused(layout, claimed_for(3, 1) + 1, fds),
        (MALFORMED, false)
    );
    // One too few.
    let (layout, _host, mut fds) = good_handles(3);
    fds.pop();
    assert_eq!(
        refused(layout, claimed_for(3, 1) - 1, fds),
        (MALFORMED, false)
    );
}

#[test]
fn fewer_or_more_descriptors_than_the_answer_claims_is_refused() {
    let (layout, _host, mut fds) = good_handles(3);
    fds.pop();
    assert_eq!(refused(layout, claimed_for(3, 1), fds), (MALFORMED, false));
    let (layout, _host, mut fds) = good_handles(3);
    fds.push(extra());
    assert_eq!(refused(layout, claimed_for(3, 1), fds), (MALFORMED, false));
}

#[test]
fn more_buffers_than_one_message_carries_is_refused_whatever_else_agrees() {
    // 129 buffers, every descriptor claimed and sent: consistent, and over the limit.
    let (layout, _host, fds) = good_handles(MAX_HANDLE_BUFFERS + 1);
    assert_eq!(fds.len(), fds::BUFFERS + MAX_HANDLE_BUFFERS as usize + 1);
    assert_eq!(
        refused(layout, claimed_for(MAX_HANDLE_BUFFERS + 1, 1), fds),
        (MALFORMED, false)
    );
}

#[test]
fn no_buffers_is_refused() {
    let (layout, _host, mut fds) = good_handles(1);
    fds.truncate(fds::BUFFERS);
    let none = Layout {
        buffers: 0,
        ..layout
    };
    assert_eq!(refused(none, fds::BUFFERS as u8, fds), (MALFORMED, false));
}

#[test]
fn handles_nobody_asked_for_are_refused() {
    // The service never said it has streams of handles, so it was asked the old way, and
    // answers with handles all the same.
    let (layout, _host, fds) = good_handles(3);
    let host = FakeHost::start();
    host.control
        .features
        .store(feature::SHARED_RINGS, Ordering::SeqCst);
    host.control.answer_handles.store(true, Ordering::SeqCst);
    *host.control.stream.lock().unwrap() = Some(StreamScript {
        layout,
        claimed: claimed_for(3, 1),
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
    let long = Layout::new(3, 8192, 1, 8).unwrap();
    let (_host, mut fds) = StreamHost::make(long, &[], false);
    // Buffer 1's descriptor, a page where the layout says two.
    fds[fds::BUFFERS + 1] = sealed_memfd(c"short", 4096).unwrap();
    let (fake, mut session) = handles_host(long, claimed_for(3, 1), fds);
    assert!(matches!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::Shm(_))
    ));
    assert_eq!(closes(&fake), 1, "the service was told the stream is free");
    assert!(session.list().is_ok(), "and the connection carries on");
}

#[test]
fn a_buffer_that_could_still_shrink_is_refused() {
    let (layout, _host, mut fds) = good_handles(2);
    let unsealed = lemnos_linux_sys::dma_buf::memfd(c"unsealed").unwrap();
    std::fs::File::from(unsealed.try_clone().unwrap())
        .set_len(4096)
        .unwrap();
    fds[fds::BUFFERS] = unsealed;
    let (_fake, mut session) = handles_host(layout, claimed_for(2, 1), fds);
    assert!(matches!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::Shm(_))
    ));
}

/// A platform whose kernel calls fail: a dma-buf bracket that returns an error.
#[derive(Debug)]
struct Refusing;

impl StreamPlatform for Refusing {
    fn recv_with_descriptors(
        &self,
        socket: BorrowedFd<'_>,
        buf: &mut [u8],
        fds: &mut Vec<OwnedFd>,
    ) -> std::io::Result<usize> {
        LinuxStreams.recv_with_descriptors(socket, buf, fds)
    }

    fn dma_sync(&self, _: BorrowedFd<'_>, _: Access, _: bool) -> std::io::Result<()> {
        Err(std::io::Error::other("no kernel support"))
    }
}

#[test]
fn a_platform_that_cant_map_one_buffer_fails_cleanly_and_gives_the_stream_back() {
    // The new design has no mapping step a platform can refuse: a failed kernel bracket changes
    // nothing about where the bytes are, so the stream still opens and its frames still read.
    let (layout, mut host, fds) = good_handles(3);
    let (fake, mut session) = handles_host(layout, claimed_for(3, 1), fds);
    let mut stream = session.open_stream(7, "s", Arc::new(Refusing)).unwrap();
    assert!(host.write(1, 0, &[4; 128]));
    assert!(host.push(1, 128, 1, 0, 0));
    let frame = stream.try_next().unwrap().unwrap();
    assert_eq!(stream.bytes(&frame), [4; 128]);
    stream.release(frame).unwrap();
    assert_eq!(closes(&fake), 0);
    assert!(session.is_connected());
}

// Streams the consumer fills.

#[test]
fn a_stream_taken_to_be_filled_takes_writes_and_the_producer_sees_them() {
    let (mut host, fds) = good();
    let (_fake, mut session) = host_with(layout(), 8, fds);
    let mut stream = session.open_stream_writable(7, "s", platform()).unwrap();
    assert!(stream.is_writable());
    stream.write(1, 5, &[1, 2, 3]).unwrap();
    assert_eq!(&host.bytes(1)[5..8], [1, 2, 3]);
    assert_eq!(&host.bytes(0)[5..8], [0, 0, 0], "buffer 0 is untouched");
    // And reads back what the producer left in a buffer that is no frame.
    assert!(host.write(0, 100, &[9; 28]));
    let mut out = [0u8; 28];
    stream.read(0, 100, &mut out).unwrap();
    assert_eq!(out, [9; 28]);
}

#[test]
fn a_write_outside_a_buffer_is_refused_and_lands_nowhere() {
    let (host, fds) = good();
    let (_fake, mut session) = host_with(layout(), 8, fds);
    let mut stream = session.open_stream_writable(7, "s", platform()).unwrap();
    let before = [host.bytes(0), host.bytes(1)];
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
    assert!(
        before == [host.bytes(0), host.bytes(1)],
        "nothing was written"
    );
    // The last byte of a buffer is inside it.
    stream.write(1, 127, &[4]).unwrap();
    assert_eq!(host.bytes(1)[127], 4);
    assert!(session.is_connected());
}

#[test]
fn a_stream_taken_to_be_read_refuses_a_write() {
    let (mut host, fds) = good();
    let (_fake, mut session) = host_with(layout(), 8, fds);
    let mut stream = session.open_stream(7, "s", platform()).unwrap();
    assert!(!stream.is_writable());
    assert_eq!(stream.write(0, 0, &[1]), Err(Error::StreamReadOnly));
    assert_eq!(host.bytes(0)[0], 0);
    assert_eq!(Status::from(Error::StreamReadOnly), Status::ACCESS);
    // Reading a buffer needs nothing more.
    assert!(host.write(1, 0, &[3]));
    let mut out = [0u8; 1];
    stream.read(1, 0, &mut out).unwrap();
    assert_eq!(out, [3]);
}

#[test]
fn a_stream_of_handles_is_filled_buffer_by_buffer_within_each_buffers_own_bytes() {
    let (layout, host, fds) = good_handles(3);
    let (_fake, mut session) = handles_host(layout, claimed_for(3, 1), fds);
    let mut stream = session.open_stream_writable(7, "s", platform()).unwrap();
    assert!(stream.has_buffer_handles() && stream.is_writable());
    stream.write(2, 120, &[7; 8]).unwrap();
    assert_eq!(&host.bytes(2)[120..128], [7; 8]);
    assert!(host.bytes(0).iter().all(|b| *b == 0));
    assert!(host.bytes(1).iter().all(|b| *b == 0));
    // The descriptor is a page long, as a heap rounds it; the buffer is its first 128 bytes.
    assert!(matches!(
        stream.write(2, 121, &[7; 8]),
        Err(Error::Stream(_))
    ));
    assert_eq!(host.bytes(2)[128], 0, "past the buffer's own bytes");
    assert!(matches!(stream.write(3, 0, &[1]), Err(Error::Stream(_))));
    let mut out = [0u8; 8];
    stream.read(2, 120, &mut out).unwrap();
    assert_eq!(out, [7; 8]);
}

/// What a buffer's descriptor was told, in order: which descriptor, and how the consumer
/// accessed it.
type Calls = Arc<std::sync::Mutex<Vec<(i32, Access, bool)>>>;

/// The Linux platform, recording each bracket the consumer puts around a dma-buf.
#[derive(Debug, Default)]
struct Telling {
    calls: Calls,
}

impl Telling {
    fn take(&self) -> Vec<(i32, Access, bool)> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

impl StreamPlatform for Telling {
    fn recv_with_descriptors(
        &self,
        socket: BorrowedFd<'_>,
        buf: &mut [u8],
        fds: &mut Vec<OwnedFd>,
    ) -> std::io::Result<usize> {
        LinuxStreams.recv_with_descriptors(socket, buf, fds)
    }

    fn dma_sync(&self, fd: BorrowedFd<'_>, access: Access, begin: bool) -> std::io::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push((fd.as_raw_fd(), access, begin));
        Ok(())
    }
}

#[test]
fn a_buffer_of_its_own_is_told_when_the_consumer_reads_and_writes_it() {
    let (layout, mut host, fds) = good_handles(3);
    let (_fake, mut session) = handles_host(layout, claimed_for(3, 1), fds);
    let telling = Arc::new(Telling::default());
    let mut stream = session
        .open_stream_writable(7, "s", Arc::clone(&telling) as _)
        .unwrap();
    let raw = |stream: &lemnos_client::FrameStream, buffer: u32| {
        stream.buffer_handle(buffer).unwrap().as_raw_fd()
    };
    let (r0, r1, r2) = (raw(&stream, 0), raw(&stream, 1), raw(&stream, 2));
    assert!(telling.take().is_empty(), "mapping a buffer is no access");

    // A frame is read in place from the moment it is taken until it is given back.
    assert!(host.push(1, 128, 1, 0, 0));
    let frame = stream.try_next().unwrap().unwrap();
    assert_eq!(telling.take(), [(r1, Access::Read, true)]);
    assert_eq!(stream.bytes(&frame).len(), 128);
    assert!(telling.take().is_empty());
    stream.release(frame).unwrap();
    assert_eq!(telling.take(), [(r1, Access::Read, false)]);
    assert!(stream.try_next().unwrap().is_none());
    assert!(telling.take().is_empty(), "no frame, no access");

    // A write is bracketed around its copy; a copy out is bracketed too.
    stream.write(2, 0, &[1, 2]).unwrap();
    assert_eq!(
        telling.take(),
        [(r2, Access::Write, true), (r2, Access::Write, false)]
    );
    let mut out = [0u8; 2];
    stream.read(2, 0, &mut out).unwrap();
    assert_eq!(
        telling.take(),
        [(r2, Access::Read, true), (r2, Access::Read, false)]
    );
    assert_eq!(out, [1, 2]);
    // What is refused touches nothing.
    assert!(stream.write(2, 127, &[0; 2]).is_err());
    assert!(stream.read(3, 0, &mut out).is_err());
    assert!(telling.take().is_empty());
    let _ = r0;
}

#[test]
fn a_stream_taken_to_be_read_is_mapped_read_only() {
    let (layout, _host, fds) = good_handles(2);
    let (_fake, mut session) = handles_host(layout, claimed_for(2, 1), fds);
    let telling = Arc::new(Telling::default());
    let mut stream = session
        .open_stream(7, "s", Arc::clone(&telling) as _)
        .unwrap();
    assert!(!stream.is_writable());
    assert_eq!(stream.write(0, 0, &[1]), Err(Error::StreamReadOnly));
    assert!(telling.take().is_empty());

    let (_host, fds) = good();
    let (_fake, mut session) = host_with(self::layout(), 8, fds);
    let telling = Arc::new(Telling::default());
    let stream = session
        .open_stream(7, "s", Arc::clone(&telling) as _)
        .unwrap();
    assert!(!stream.is_writable());
}

/// A good stream of `buffers` buffers of two planes, 128 and 64 bytes, each plane its own
/// descriptor as long as the plane: the host, and the descriptors in the order they travel
/// (buffer 0's planes first).
fn good_planes(buffers: u32) -> (Layout, StreamHost, Vec<OwnedFd>) {
    let layout = Layout::new(buffers, 192, 1, 8).unwrap();
    let (host, fds) = StreamHost::make(layout, &[128, 64], false);
    (layout, host, fds)
}

/// `plane_bytes` for planes of 128 and 64 bytes.
fn two_planes() -> Vec<u8> {
    [128u32, 64]
        .iter()
        .flat_map(|len| len.to_le_bytes())
        .collect()
}

#[test]
fn a_stream_of_planes_reads_each_plane_of_a_frame_from_its_own_memory() {
    let (layout, mut host, fds) = good_planes(2);
    let (fake, mut session) = handles_host(layout, claimed_for(2, 2), fds);
    *fake.control.plane_bytes.lock().unwrap() = two_planes();
    let mut stream = session.open_stream_writable(7, "s", platform()).unwrap();
    assert_eq!(stream.planes().lengths(), &[128, 64]);
    // Buffer 1's planes are pool buffers 2 and 3.
    assert!(host.write(2, 0, &[1; 128]));
    assert!(host.write(3, 0, &[2; 64]));
    assert!(host.push(1, 192, 1, 0, 0));
    let frame = stream.try_next().unwrap().unwrap();
    assert_eq!(stream.plane(&frame, 0), [1; 128]);
    assert_eq!(stream.plane(&frame, 1), [2; 64]);
    assert_eq!(stream.bytes(&frame), [1; 128], "the first plane");
    assert!(stream.plane(&frame, 2).is_empty(), "no such plane");
    stream.release(frame).unwrap();
    assert!(host.reclaim(1));
    // A frame shorter than its buffer ends in the plane it ends in.
    assert!(host.push(0, 130, 2, 0, 0));
    let frame = stream.try_next().unwrap().unwrap();
    assert_eq!(stream.plane(&frame, 0).len(), 128);
    assert_eq!(stream.plane(&frame, 1).len(), 2);
    stream.release(frame).unwrap();
    // `write` and `read` address the planes end to end, across the seam too.
    stream.write(0, 126, &[7, 7, 8, 8]).unwrap();
    assert_eq!(&host.bytes(0)[126..], [7, 7]);
    assert_eq!(&host.bytes(1)[..2], [8, 8]);
    let mut out = [0; 4];
    stream.read(0, 126, &mut out).unwrap();
    assert_eq!(out, [7, 7, 8, 8]);
    assert_eq!(
        stream.write(0, 190, &[0; 3]).unwrap_err(),
        Error::Stream(RingError::BadLength),
        "past the last plane"
    );
}

#[test]
fn planes_that_dont_add_up_to_the_buffer_or_descriptors_that_dont_match_them_are_refused() {
    // Planes of 128 and 63 for a buffer of 192.
    let (layout, _host, fds) = good_planes(2);
    let (fake, mut session) = handles_host(layout, claimed_for(2, 2), fds);
    *fake.control.plane_bytes.lock().unwrap() = [128u32, 63]
        .iter()
        .flat_map(|len| len.to_le_bytes())
        .collect();
    assert_eq!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        MALFORMED
    );
    // Planes that do, with a descriptor for one plane per buffer only.
    let (layout, _host, mut fds) = good_planes(2);
    fds.truncate(fds::BUFFERS + 2);
    let (fake, mut session) = handles_host(layout, claimed_for(2, 1), fds);
    *fake.control.plane_bytes.lock().unwrap() = two_planes();
    assert_eq!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        MALFORMED
    );
    // A length cut short.
    let (layout, _host, fds) = good_planes(2);
    let (fake, mut session) = handles_host(layout, claimed_for(2, 2), fds);
    *fake.control.plane_bytes.lock().unwrap() = vec![128, 0, 0, 0, 64];
    assert_eq!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        MALFORMED
    );
}

#[test]
fn a_plane_descriptor_shorter_than_its_plane_is_refused_before_mapping() {
    let (layout, _host, mut fds) = good_planes(2);
    // Buffer 1's second plane, a byte short.
    fds[fds::BUFFERS + 3] = sealed_memfd(c"short", 63).unwrap();
    let (fake, mut session) = handles_host(layout, claimed_for(2, 2), fds);
    *fake.control.plane_bytes.lock().unwrap() = two_planes();
    assert!(matches!(
        session.open_stream(1, "s", platform()).unwrap_err(),
        Error::Stream(RingError::Shm(_))
    ));
    assert_eq!(closes(&fake), 1, "given back");
}

/// A service that doesn't set `SHARED_RINGS` answers streams with the older layout, which this build
/// does not read: the open is refused before anything is asked of the service.
#[test]
fn a_service_without_shared_rings_is_refused_before_it_is_asked() {
    let host = FakeHost::start();
    let mut session = Session::connect(&host.path, "t").unwrap();
    let error = session.open_stream(1, "s", platform()).unwrap_err();
    assert!(
        matches!(&error, Error::Refused { status, .. } if *status == Status::NOT_SUPPORTED),
        "{error:?}"
    );
}
