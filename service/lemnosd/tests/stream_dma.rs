//! Streams whose buffers are dma-bufs, over the real socket: one descriptor per buffer to the
//! consumer, and the same memory behind the producer's handle.
//!
//! Sealed memfds stand in for the dma-bufs, so nothing here opens a device node. With
//! `LEMNOS_TEST_DMA_HEAP` set to a heap (`/dev/dma_heap/system`), one test runs the same on real
//! dma-bufs from it; without, that test says it was skipped.

#![allow(clippy::print_stderr)]

mod common;

use std::io::{self, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Running, StandIn, start_with_buffers};
use lemnos_client::{Error, FrameStream, Session, StreamPlatform};
use lemnos_core::Status;
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;
use lemnos_host::stream::{ProducerEndpoint, StreamMemory};
use lemnos_linux_stream::LinuxStreams;
use lemnos_linux_sys::dma_buf;
use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::scm::recv_with_fds;
use lemnos_wire::{Frame, MAX_HANDLE_BUFFERS, VERSION, append, decode, feature};
use lemnosd::DmaHeaps;

const DEVICE: u32 = 0;
const DMA: &str = "frames";
const SHARED: &str = "plain";
const BUFFERS: u32 = 4;
/// Not a whole page, so a buffer's own bytes and what a heap rounds it to differ.
const BYTES: usize = 5000;

fn board(heap: &str) -> String {
    format!(
        "[[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = {BUFFERS}\nbuffer_bytes = {BYTES}\n\
         memory = \"dma-buf\"\n{heap}\
         [devices.streams.plain]\nbuffers = 2\nbuffer_bytes = 128\n"
    )
}

fn service() -> (Running, Arc<StandIn>) {
    service_from(StandIn::default())
}

fn service_from(source: StandIn) -> (Running, Arc<StandIn>) {
    let source = Arc::new(source);
    let running = start_with_buffers(&board(""), Arc::clone(&source) as _);
    (running, source)
}

fn platform() -> Arc<dyn StreamPlatform> {
    Arc::new(LinuxStreams)
}

fn host(running: &Running) -> &lemnos_host::Host {
    running.service.as_ref().unwrap().host()
}

fn open(running: &Running) -> (Session, FrameStream) {
    let mut session = Session::connect(&running.socket, "consumer").unwrap();
    let stream = session.open_stream(DEVICE, DMA, platform()).unwrap();
    (session, stream)
}

fn producer(running: &Running) -> ProducerEndpoint {
    host(running).stream_producer(DEVICE, DMA).unwrap()
}

fn frame(buffer: u32, sequence: u64) -> StreamFrame {
    StreamFrame {
        buffer,
        bytes: BYTES as u32,
        sequence,
        identity: sequence + 100,
        timestamp_ns: 0,
    }
}

/// The memory behind `buffer`'s handle, as hardware given the descriptor reaches it: a hold
/// and a mapping of its own.
fn hardware(producer: &ProducerEndpoint, buffer: u32) -> (OwnedFd, Mapping) {
    let handle = producer
        .buffer_handle(buffer)
        .expect("the stream has handles")
        .expect("a consumer is behind the buffer");
    let fd = handle.fd().try_clone_to_owned().unwrap();
    let map = Mapping::new(fd.as_fd(), BYTES, 0, true).unwrap();
    (fd, map)
}

/// A frame through the handle and through `write`, and what the consumer reads of it.
fn a_frame_crosses(producer: &mut ProducerEndpoint, stream: &mut FrameStream, sequence: u64) {
    let buffer = producer.next_buffer().unwrap().unwrap();
    let (_fd, map) = hardware(producer, buffer);
    let stamp = sequence as u8;
    // "Hardware" fills the buffer through its handle, and the driver patches the end in software.
    map.write_bytes(0, &vec![stamp; BYTES]).unwrap();
    assert_eq!(producer.write(buffer, BYTES - 2, &[0xAB, 0xCD]), Ok(true));
    // `read` reaches what hardware wrote.
    let mut back = [0u8; 3];
    assert_eq!(producer.read(buffer, 10, &mut back), Ok(true));
    assert_eq!(back, [stamp; 3]);
    producer.publish(frame(buffer, sequence)).unwrap();

    let got = stream.next_frame(Duration::from_secs(5)).unwrap().unwrap();
    assert_eq!(got.buffer(), buffer);
    assert_eq!(got.sequence(), sequence);
    assert_eq!(got.identity(), sequence + 100);
    let bytes = stream.bytes(&got);
    assert_eq!(bytes.len(), BYTES);
    assert!(bytes[..BYTES - 2].iter().all(|b| *b == stamp));
    assert_eq!(bytes[BYTES - 2..], [0xAB, 0xCD]);
    stream.release(got).unwrap();
}

#[test]
fn a_board_with_a_dma_buf_stream_attaches_and_the_handles_memory_is_what_the_consumer_reads() {
    let (running, source) = service();
    assert_eq!(
        host(&running).stream_memory(DEVICE, DMA),
        Some(StreamMemory::DmaBuf)
    );
    let (mut session, mut stream) = open(&running);
    assert!(stream.has_buffer_handles());
    assert_eq!(stream.layout().buffers, BUFFERS);
    // One buffer each, a page multiple long, from the heap the board file left as the default.
    assert_eq!(
        *source.asked.lock().unwrap(),
        [("system".to_string(), 8192, BUFFERS)]
    );
    let mut producing = producer(&running);
    // More frames than buffers, so every buffer goes round.
    for sequence in 0..3 * u64::from(BUFFERS) {
        a_frame_crosses(&mut producing, &mut stream, sequence);
    }
    let stats = host(&running).stream_stats(DEVICE, DMA).unwrap();
    assert_eq!(stats.delivered, 3 * u64::from(BUFFERS));
    assert_eq!((stats.dropped, stats.faults), (0, 0));
    // The connection went unused for all of that, and still works.
    assert!(session.list().unwrap().iter().any(|d| d.name == "s"));
    session.close_stream(stream).unwrap();
}

#[test]
fn every_buffer_is_its_own_memory() {
    let (running, _source) = service();
    let (_session, _stream) = open(&running);
    let mut producing = producer(&running);
    let mut seen = Vec::new();
    for value in 0..BUFFERS {
        let buffer = producing.next_buffer().unwrap().unwrap();
        let (fd, map) = hardware(&producing, buffer);
        map.write_bytes(0, &[value as u8 + 1]).unwrap();
        seen.push((buffer, dma_buf::identity(fd.as_fd()).unwrap(), map));
    }
    for (index, (buffer, identity, map)) in seen.iter().enumerate() {
        assert_eq!(map.as_slice()[0], index as u8 + 1, "buffer {buffer}");
        let same = seen.iter().filter(|other| other.1 == *identity).count();
        assert_eq!(same, 1, "buffer {buffer} shares its dma-buf with another");
    }
}

#[test]
fn detaching_and_attaching_again_gives_fresh_memory() {
    let (running, source) = service();
    let (mut session, mut stream) = open(&running);
    let mut producing = producer(&running);
    a_frame_crosses(&mut producing, &mut stream, 1);
    let held = producing.next_buffer().unwrap().unwrap();
    let (old_fd, old_map) = hardware(&producing, held);
    old_map.write_bytes(0, &[0x11; 16]).unwrap();
    let old = dma_buf::identity(old_fd.as_fd()).unwrap();

    session.close_stream(stream).unwrap();
    // Nothing is behind the buffer now, and it is given up by publishing it.
    assert!(producing.buffer_handle(held).unwrap().is_none());
    producing.publish(frame(held, 2)).unwrap();

    let mut stream = session.open_stream(DEVICE, DMA, platform()).unwrap();
    assert_eq!(source.asked.lock().unwrap().len(), 2, "allocated again");
    let mut fresh = Vec::new();
    for _ in 0..BUFFERS {
        let buffer = producing.next_buffer().unwrap().unwrap();
        let (fd, map) = hardware(&producing, buffer);
        assert_ne!(dma_buf::identity(fd.as_fd()).unwrap(), old);
        assert!(map.as_slice().iter().all(|b| *b == 0), "buffer {buffer}");
        fresh.push(buffer);
    }
    // Hardware still holding the old buffer writes where the new consumer never looks.
    old_map.write_bytes(0, &[0x22; 16]).unwrap();
    for buffer in fresh {
        producing.publish(frame(buffer, 3)).unwrap();
        let got = stream.next_frame(Duration::from_secs(5)).unwrap().unwrap();
        assert!(stream.bytes(&got).iter().all(|b| *b == 0));
    }
}

fn refused(error: &Error) -> Option<(Status, &str)> {
    match error {
        Error::Refused { status, text } => Some((*status, text.as_str())),
        _ => None,
    }
}

#[test]
fn a_heap_that_is_missing_or_refuses_fails_the_attach_and_shared_memory_is_never_used_instead() {
    for (kind, status) in [
        (io::ErrorKind::NotFound, Status::NOT_FOUND),
        (io::ErrorKind::PermissionDenied, Status::ACCESS),
        (io::ErrorKind::OutOfMemory, Status::NO_MEMORY),
        (io::ErrorKind::Other, Status::IO),
    ] {
        let (running, source) = service();
        *source.fail.lock().unwrap() = Some(kind);
        let mut session = Session::connect(&running.socket, "consumer").unwrap();
        let error = session.open_stream(DEVICE, DMA, platform()).unwrap_err();
        assert_eq!(
            refused(&error),
            Some((status, "can't get dma-bufs for the stream")),
            "{kind:?}"
        );
        let stats = host(&running).stream_stats(DEVICE, DMA).unwrap();
        assert!(!stats.attached);
        assert_eq!(stats.attachments, 0);
        // The connection, and the stream once the heap is there, are fine.
        assert!(session.open_stream(DEVICE, SHARED, platform()).is_ok());
        *source.fail.lock().unwrap() = None;
        let stream = session.open_stream(DEVICE, DMA, platform()).unwrap();
        assert!(stream.has_buffer_handles());
    }
}

#[test]
fn the_kernels_heaps_are_looked_up_by_name_and_one_that_isnt_there_is_not_found() {
    // The real source, pointed at a directory with no heaps in it.
    let empty = std::env::temp_dir().join(format!("lemnosd-no-heaps-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    let running = start_with_buffers(&board(""), Arc::new(DmaHeaps::at(&empty)));
    let mut session = Session::connect(&running.socket, "consumer").unwrap();
    let error = session.open_stream(DEVICE, DMA, platform()).unwrap_err();
    assert_eq!(
        refused(&error).map(|(status, _)| status),
        Some(Status::NOT_FOUND)
    );
    assert!(!host(&running).stream_stats(DEVICE, DMA).unwrap().attached);
    std::fs::remove_dir_all(&empty).unwrap();
}

#[test]
fn a_source_that_hands_back_too_few_or_too_short_buffers_is_refused_before_anything_is_mapped() {
    for source in [
        StandIn {
            fewer: true,
            ..StandIn::default()
        },
        StandIn {
            short: true,
            ..StandIn::default()
        },
    ] {
        let (running, _source) = service_from(source);
        let mut session = Session::connect(&running.socket, "consumer").unwrap();
        let error = session.open_stream(DEVICE, DMA, platform()).unwrap_err();
        assert_eq!(refused(&error).map(|(status, _)| status), Some(Status::IO));
        assert!(!host(&running).stream_stats(DEVICE, DMA).unwrap().attached);
    }
}

#[test]
fn the_heap_the_board_file_names_is_the_one_asked() {
    let source = Arc::new(StandIn::default());
    let running = start_with_buffers(&board("heap = \"linux,cma\"\n"), Arc::clone(&source) as _);
    let (_session, _stream) = open(&running);
    assert_eq!(source.asked.lock().unwrap()[0].0, "linux,cma");
}

#[test]
fn a_stream_with_as_many_buffers_as_one_message_carries_attaches_and_delivers() {
    let source = Arc::new(StandIn::default());
    let devices = format!(
        "[[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = {MAX_HANDLE_BUFFERS}\nbuffer_bytes = 64\n\
         memory = \"dma-buf\"\n"
    );
    let running = start_with_buffers(&devices, Arc::clone(&source) as _);
    let (_session, mut stream) = open(&running);
    let mut producing = producer(&running);
    for sequence in 0..u64::from(MAX_HANDLE_BUFFERS) {
        let buffer = producing.next_buffer().unwrap().unwrap();
        assert_eq!(producing.write(buffer, 0, &[buffer as u8, 0xEE]), Ok(true));
        producing
            .publish(StreamFrame {
                bytes: 2,
                ..frame(buffer, sequence)
            })
            .unwrap();
        let got = stream.next_frame(Duration::from_secs(5)).unwrap().unwrap();
        assert_eq!(stream.bytes(&got), [got.buffer() as u8, 0xEE]);
        // Held, so the next frame is in the next buffer and every one is used.
    }
    assert!(producing.next_buffer().unwrap().is_none());
}

/// A client written against the protocol, with nothing checking its behaviour.
struct RawClient {
    socket: UnixStream,
    buffer: Vec<u8>,
    filled: usize,
    fds: Vec<OwnedFd>,
    features: u64,
}

impl RawClient {
    fn connect(path: &Path) -> Self {
        let socket = UnixStream::connect(path).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut client = Self {
            socket,
            buffer: vec![0; 1 << 16],
            filled: 0,
            fds: Vec::new(),
            features: 0,
        };
        client.send(&Frame::Hello {
            layout: lemnos_wire::LAYOUT_HASH,
            lemnos: lemnos_wire::LemnosName(lemnos_wire::LEMNOS),
            version: VERSION,
            name: "raw",
            events: false,
        });
        client.features = client.next(|frame| match frame {
            Frame::Welcome { features, .. } => Some(features),
            _ => None,
        });
        client
    }

    fn send(&mut self, frame: &Frame<'_>) {
        let mut out = Vec::new();
        append(frame, &mut out).unwrap();
        self.socket.write_all(&out).unwrap();
    }

    fn next<R>(&mut self, mut want: impl FnMut(Frame<'_>) -> Option<R>) -> R {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut used = 0;
            let mut found = None;
            while let Ok(Some((frame, n))) = decode(&self.buffer[used..self.filled]) {
                used += n;
                found = want(frame);
                if found.is_some() {
                    break;
                }
            }
            self.buffer.copy_within(used..self.filled, 0);
            self.filled -= used;
            if let Some(found) = found {
                return found;
            }
            assert!(Instant::now() < deadline, "no such frame came");
            let n = recv_with_fds(
                self.socket.as_fd(),
                &mut self.buffer[self.filled..],
                &mut self.fds,
            )
            .expect("the socket");
            assert!(n > 0, "the service closed the connection");
            self.filled += n;
        }
    }

    fn still_answers(&mut self) -> bool {
        self.send(&Frame::List { id: 9 });
        self.next(|frame| matches!(frame, Frame::ListDone { id: 9 }).then_some(true))
    }
}

/// What a stream being opened was answered with.
#[derive(Debug, PartialEq)]
enum Answer {
    /// StreamReady: buffers, descriptors claimed.
    Ready(u32, u8),
    /// StreamHandles: buffers, descriptors claimed.
    Handles(u32, u8),
    Refusal(i32, String),
}

fn answer(raw: &mut RawClient, id: u32) -> Answer {
    raw.next(|frame| match frame {
        Frame::StreamReady {
            id: got,
            buffers,
            descriptors,
            ..
        } if got == id => Some(Answer::Ready(buffers, descriptors)),
        Frame::StreamHandles {
            id: got,
            buffers,
            descriptors,
            ..
        } if got == id => Some(Answer::Handles(buffers, descriptors)),
        Frame::Refusal {
            id: got,
            status,
            text,
        } if got == id => Some(Answer::Refusal(status, text.to_string())),
        _ => None,
    })
}

#[test]
fn the_service_says_it_has_streams_of_handles() {
    let (running, _source) = service();
    let raw = RawClient::connect(&running.socket);
    assert_eq!(
        raw.features & feature::STREAM_HANDLES,
        feature::STREAM_HANDLES
    );
    assert_eq!(
        raw.features & !feature::ALL,
        0,
        "and no bit this build doesn't know"
    );
}

#[test]
fn a_client_from_before_handles_is_refused_a_dma_buf_stream_cleanly_and_keeps_everything_else() {
    let (running, source) = service();
    // Such a client knows only StreamOpen.
    let mut old = RawClient::connect(&running.socket);
    old.send(&Frame::StreamOpen {
        id: 1,
        device: DEVICE,
        name: DMA,
    });
    assert_eq!(
        answer(&mut old, 1),
        Answer::Refusal(
            Status::NOT_SUPPORTED.0,
            "the stream's buffers are dma-bufs, which this client can't take".to_string()
        )
    );
    assert!(old.fds.is_empty(), "no descriptor came with the refusal");
    // Nothing was taken or allocated for it, so a client that can take the stream gets it.
    assert!(source.asked.lock().unwrap().is_empty());
    let stats = host(&running).stream_stats(DEVICE, DMA).unwrap();
    assert!(!stats.attached);
    assert_eq!(stats.attachments, 0);
    // Its connection carries on, and a stream of shared memory opens for it as it always did.
    assert!(old.still_answers());
    old.send(&Frame::StreamOpen {
        id: 2,
        device: DEVICE,
        name: SHARED,
    });
    assert_eq!(answer(&mut old, 2), Answer::Ready(2, 3));
    assert_eq!(old.fds.len(), 3);
    let (_session, _stream) = open(&running);
}

#[test]
fn asking_the_new_way_gets_handles_for_dma_bufs_and_the_old_answer_for_shared_memory() {
    let (running, _source) = service();
    let mut raw = RawClient::connect(&running.socket);
    raw.send(&Frame::StreamOpenHandles {
        id: 1,
        device: DEVICE,
        name: SHARED,
    });
    assert_eq!(answer(&mut raw, 1), Answer::Ready(2, 3));
    assert_eq!(raw.fds.len(), 3);
    raw.fds.clear();

    raw.send(&Frame::StreamOpenHandles {
        id: 2,
        device: DEVICE,
        name: DMA,
    });
    assert_eq!(
        answer(&mut raw, 2),
        Answer::Handles(BUFFERS, 2 + BUFFERS as u8)
    );
    assert_eq!(
        raw.fds.len(),
        2 + BUFFERS as usize,
        "all in the one message"
    );
    // The ring, the doorbell, then a buffer each: every buffer at least its bytes long, and
    // none of them able to shrink under whoever maps it.
    for buffer in &raw.fds[2..] {
        assert!(dma_buf::size(buffer.as_fd()).unwrap() >= BYTES as u64);
        assert!(
            std::fs::File::from(buffer.try_clone().unwrap())
                .set_len(0)
                .is_err()
        );
    }
    // Unknown streams and a second consumer are refused as for StreamOpen.
    raw.send(&Frame::StreamOpenHandles {
        id: 3,
        device: DEVICE,
        name: "nothing",
    });
    assert_eq!(
        answer(&mut raw, 3),
        Answer::Refusal(Status::NOT_FOUND.0, "can't open the stream".to_string())
    );
    let mut second = RawClient::connect(&running.socket);
    second.send(&Frame::StreamOpenHandles {
        id: 4,
        device: DEVICE,
        name: DMA,
    });
    assert_eq!(
        answer(&mut second, 4),
        Answer::Refusal(Status::BUSY.0, "can't open the stream".to_string())
    );
    assert!(raw.still_answers());
}

#[test]
fn the_same_on_dma_bufs_from_a_real_heap_when_one_is_named() {
    let Some(path) = std::env::var_os("LEMNOS_TEST_DMA_HEAP") else {
        eprintln!("skipped: LEMNOS_TEST_DMA_HEAP names no heap");
        return;
    };
    let path = Path::new(&path);
    let (dir, heap) = (path.parent().unwrap(), path.file_name().unwrap());
    let heap = heap.to_str().unwrap();
    let running = start_with_buffers(
        &board(&format!("heap = \"{heap}\"\n")),
        Arc::new(DmaHeaps::at(dir)),
    );
    let (mut session, mut stream) = open(&running);
    assert!(stream.has_buffer_handles());
    let mut producing = producer(&running);
    for sequence in 0..3 * u64::from(BUFFERS) {
        a_frame_crosses(&mut producing, &mut stream, sequence);
    }
    // They are dma-bufs, each at least a buffer long.
    let held = producing.next_buffer().unwrap().unwrap();
    let (old_fd, old_map) = hardware(&producing, held);
    assert!(dma_buf::is_dma_buf(old_fd.as_fd()).unwrap());
    assert!(dma_buf::size(old_fd.as_fd()).unwrap() >= BYTES as u64);
    let old = dma_buf::identity(old_fd.as_fd()).unwrap();
    old_map.write_bytes(0, &[0x11; 16]).unwrap();

    // A new consumer gets new dma-bufs, zeroed by the kernel.
    session.close_stream(stream).unwrap();
    assert!(producing.buffer_handle(held).unwrap().is_none());
    producing.publish(frame(held, 0)).unwrap();
    let mut stream = session.open_stream(DEVICE, DMA, platform()).unwrap();
    for _ in 0..BUFFERS {
        let buffer = producing.next_buffer().unwrap().unwrap();
        let (fd, map) = hardware(&producing, buffer);
        assert!(dma_buf::is_dma_buf(fd.as_fd()).unwrap());
        assert_ne!(dma_buf::identity(fd.as_fd()).unwrap(), old);
        assert!(map.as_slice().iter().all(|b| *b == 0));
        producing.publish(frame(buffer, 1)).unwrap();
        let got = stream.next_frame(Duration::from_secs(5)).unwrap().unwrap();
        stream.release(got).unwrap();
    }
    a_frame_crosses(&mut producing, &mut stream, 7);
    eprintln!("ran on dma-bufs from {}", path.to_string_lossy());
}

#[test]
fn the_consumer_holds_each_buffers_own_handle_and_it_is_the_memory_the_producer_fills() {
    let (running, _source) = service();
    let (mut session, mut stream) = open(&running);
    let mut producing = producer(&running);
    for _ in 0..BUFFERS {
        let buffer = producing.next_buffer().unwrap().unwrap();
        let (fd, map) = hardware(&producing, buffer);
        let ours = stream.buffer_handle(buffer).expect("a handle per buffer");
        assert_eq!(
            dma_buf::identity(ours).unwrap(),
            dma_buf::identity(fd.as_fd()).unwrap(),
            "buffer {buffer}"
        );
        // What lands through the handle the producer lent is what the consumer's own handle
        // reaches, mapped apart from the stream's mapping.
        map.write_bytes(7, &[buffer as u8 + 1]).unwrap();
        let theirs = Mapping::new(ours, BYTES, 0, false).unwrap();
        assert_eq!(theirs.as_slice()[7], buffer as u8 + 1);
        producing.publish(frame(buffer, u64::from(buffer))).unwrap();
        let got = stream.next_frame(Duration::from_secs(5)).unwrap().unwrap();
        stream.release(got).unwrap();
    }
    assert!(stream.buffer_handle(BUFFERS).is_none(), "no such buffer");
    let plain = session.open_stream(DEVICE, SHARED, platform()).unwrap();
    assert!(!plain.has_buffer_handles());
    assert!(plain.buffer_handle(0).is_none(), "shared memory has none");
}
