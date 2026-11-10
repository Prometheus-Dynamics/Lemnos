//! Frame streams over the real socket: descriptors by `SCM_RIGHTS`, real shared memory, and a
//! simulated producer in this process.

mod common;

use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use common::{Running, start_with};
use lemnos_client::{Error, FrameStream, Session, StreamPlatform};
use lemnos_core::Status;
use lemnos_hal::StreamEndpoint;
use lemnos_host::stream::ProducerEndpoint;
use lemnos_linux_stream::LinuxStreams;
use lemnos_linux_sys::scm::recv_with_fds;
use lemnos_sim::SimFrameSource;
use lemnos_wire::ring::{Layout, STATE_FAULT, offsets};
use lemnos_wire::{Frame, VERSION, append, decode};

const FRAME_BYTES: usize = 4096;
const DEVICE: u32 = 0;
const NAME: &str = "frames";
const BOARD: &str = "[[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
                     [devices.streams.frames]\nbuffers = 4\nbuffer_bytes = 4096\n\
                     [devices.streams.other]\nbuffers = 2\nbuffer_bytes = 128\n";

fn platform() -> Arc<dyn StreamPlatform> {
    Arc::new(LinuxStreams)
}

fn service() -> Running {
    start_with(BOARD)
}

fn host(running: &Running) -> &lemnos_host::Host {
    running.service.as_ref().unwrap().host()
}

fn open(running: &Running) -> (Session, FrameStream) {
    let mut session = Session::connect(&running.socket, "consumer").unwrap();
    let stream = session.open_stream(DEVICE, NAME, platform()).unwrap();
    (session, stream)
}

fn producer(running: &Running) -> ProducerEndpoint {
    host(running).stream_producer(DEVICE, NAME).unwrap()
}

fn produce(source: &mut SimFrameSource, producer: &mut ProducerEndpoint) -> bool {
    let writer = producer.writer();
    source
        .produce(producer, &mut |buffer, bytes| {
            writer.write(buffer, 0, bytes)
        })
        .unwrap()
}

fn poll_until(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(2));
    }
    true
}

#[test]
fn frames_cross_from_a_producer_thread_with_contents_order_and_identity_intact() {
    let running = service();
    let (mut session, mut stream) = open(&running);
    let mut producing = producer(&running);
    const FRAMES: u64 = 5000;

    let producer_thread = thread::spawn(move || {
        let mut source = SimFrameSource::new(FRAME_BYTES);
        while source.sequence() < FRAMES {
            if !produce(&mut source, &mut producing) {
                thread::yield_now();
            }
        }
        producing
    });

    let mut expected = vec![0u8; FRAME_BYTES];
    for sequence in 0..FRAMES {
        let frame = stream
            .next_frame(Duration::from_secs(5))
            .unwrap()
            .unwrap_or_else(|| panic!("frame {sequence} never came"));
        assert_eq!(frame.sequence(), sequence);
        assert_eq!(frame.identity(), SimFrameSource::identity(sequence));
        assert_eq!(frame.timestamp_ns(), sequence * 1000);
        assert_eq!(frame.len(), FRAME_BYTES);
        SimFrameSource::fill(sequence, &mut expected);
        assert_eq!(stream.bytes(&frame), &expected[..], "frame {sequence}");
        stream.release(frame).unwrap();
    }
    assert!(stream.try_next().unwrap().is_none());

    let mut producing = producer_thread.join().unwrap();
    producing.next_buffer().unwrap();
    let stats = host(&running).stream_stats(DEVICE, NAME).unwrap();
    assert_eq!(stats.published, FRAMES);
    assert_eq!(stats.delivered, FRAMES);
    assert_eq!(stats.dropped, 0);
    assert_eq!(stats.faults, 0);
    assert_eq!(stats.consumer_held, 0);
    assert_eq!(stats.free + stats.producer_held, 4);
    // The connection went unused for all of that, and still works.
    assert!(session.list().unwrap().iter().any(|d| d.name == "s"));
}

#[test]
fn jobs_travel_back_to_the_producer_on_the_ring() {
    let running = service();
    let (_session, mut stream) = open(&running);
    let mut producing = producer(&running);
    assert!(stream.post_job(&[9, 8, 7]).unwrap());
    let mut out = [0u8; 16];
    assert_eq!(producing.next_job(&mut out), Ok(Some(3)));
    assert_eq!(&out[..3], [9, 8, 7]);
}

#[test]
fn the_services_buffers_are_one_memfd_so_none_has_a_handle_for_hardware() {
    let running = service();
    let (_session, _stream) = open(&running);
    let mut producing = producer(&running);
    let buffer = producing.next_buffer().unwrap().unwrap();
    assert_eq!(
        producing.buffer_handle(buffer).map(|_| ()),
        Err(lemnos_hal::ErrorKind::Unsupported)
    );
    assert_eq!(producing.write(buffer, 0, &[1]), Ok(true));
}

#[test]
fn an_unknown_device_or_stream_is_refused_with_a_status() {
    let running = service();
    let mut session = Session::connect(&running.socket, "consumer").unwrap();
    for (device, name) in [(99, NAME), (DEVICE, "nothing")] {
        let error = session.open_stream(device, name, platform()).unwrap_err();
        assert!(
            matches!(&error, Error::Refused { status, .. } if *status == Status::NOT_FOUND),
            "{error:?}"
        );
    }
    assert!(session.open_stream(DEVICE, NAME, platform()).is_ok());
}

#[test]
fn a_second_consumer_is_refused_with_busy() {
    let running = service();
    let (_first, _stream) = open(&running);
    let mut second = Session::connect(&running.socket, "second").unwrap();
    let error = second.open_stream(DEVICE, NAME, platform()).unwrap_err();
    assert!(
        matches!(&error, Error::Refused { status, .. } if *status == Status::BUSY),
        "{error:?}"
    );
}

#[test]
fn a_client_disconnecting_tears_the_stream_down_and_frees_its_buffers() {
    let running = service();
    let (session, mut stream) = open(&running);
    let mut producing = producer(&running);
    let mut source = SimFrameSource::new(FRAME_BYTES);
    for _ in 0..3 {
        assert!(produce(&mut source, &mut producing));
    }
    let _kept = stream.next_frame(Duration::from_secs(5)).unwrap().unwrap();
    assert_eq!(
        host(&running)
            .stream_stats(DEVICE, NAME)
            .unwrap()
            .consumer_held,
        3
    );

    drop(session);

    assert!(poll_until(|| {
        let stats = host(&running).stream_stats(DEVICE, NAME).unwrap();
        !stats.attached && stats.free == 4
    }));
    assert!(!stream.is_open(), "the ring says it ended");
    // Another client gets all four buffers.
    let (_session, mut next) = open(&running);
    for _ in 0..4 {
        assert!(produce(&mut source, &mut producing));
    }
    for _ in 0..4 {
        let frame = next.next_frame(Duration::from_secs(5)).unwrap().unwrap();
        next.release(frame).unwrap();
    }
}

#[test]
fn closing_a_stream_keeps_the_connection_and_frees_the_stream() {
    let running = service();
    let (mut session, stream) = open(&running);
    session.close_stream(stream).unwrap();
    assert!(!host(&running).stream_stats(DEVICE, NAME).unwrap().attached);
    let again = session.open_stream(DEVICE, NAME, platform()).unwrap();
    assert!(host(&running).stream_stats(DEVICE, NAME).unwrap().attached);
    session.close_stream(again).unwrap();
}

#[test]
fn closing_a_stream_this_connection_doesnt_hold_is_refused() {
    let running = service();
    let (_holder, _stream) = open(&running);
    let mut raw = RawClient::connect(&running.socket);
    raw.send(&Frame::StreamClose {
        id: 1,
        device: DEVICE,
        name: NAME,
    });
    assert_eq!(raw.next_refusal_status(), Status::NOT_FOUND.0);
    assert!(host(&running).stream_stats(DEVICE, NAME).unwrap().attached);
}

/// A client written against the protocol, with nothing checking its behaviour.
struct RawClient {
    socket: UnixStream,
    buffer: Vec<u8>,
    filled: usize,
    fds: Vec<OwnedFd>,
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
        };
        client.send(&Frame::Hello {
            version: VERSION,
            name: "raw",
            events: false,
        });
        client.next(|frame| matches!(frame, Frame::Welcome { .. }).then_some(()));
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

    fn next_refusal_status(&mut self) -> i32 {
        self.next(|frame| match frame {
            Frame::Refusal { status, .. } => Some(status),
            _ => None,
        })
    }

    /// Opens a stream and returns its layout and descriptors.
    fn open_stream(&mut self) -> (Layout, Vec<OwnedFd>) {
        self.send(&Frame::StreamOpen {
            id: 3,
            device: DEVICE,
            name: NAME,
        });
        let layout = self.next(|frame| match frame {
            Frame::StreamReady {
                buffers,
                buffer_bytes,
                frame_slots,
                job_slots,
                job_bytes,
                descriptors,
                ..
            } => {
                assert_eq!(descriptors, 3);
                Some(Layout {
                    buffers,
                    buffer_bytes,
                    frame_slots,
                    job_slots,
                    job_bytes,
                })
            }
            _ => None,
        });
        assert_eq!(self.fds.len(), 3, "three descriptors came with it");
        (layout, std::mem::take(&mut self.fds))
    }

    fn still_answers(&mut self) -> bool {
        self.send(&Frame::List { id: 9 });
        self.next(|frame| matches!(frame, Frame::ListDone { id: 9 }).then_some(true))
    }
}

#[test]
fn descriptors_are_sealed_so_a_client_cant_resize_them() {
    let running = service();
    let mut raw = RawClient::connect(&running.socket);
    let (layout, mut fds) = raw.open_stream();
    let buffers = fds.remove(2);
    let bell = fds.remove(1);
    let ring = fds.remove(0);
    let ring = std::fs::File::from(ring);
    let pool = std::fs::File::from(buffers);
    assert!(ring.set_len(0).is_err());
    assert!(pool.set_len(0).is_err());
    assert_eq!(ring.metadata().unwrap().len(), layout.ring_bytes() as u64);
    assert_eq!(pool.metadata().unwrap().len(), layout.pool_bytes());
    drop(bell);
    assert!(raw.still_answers());
}

#[test]
fn a_client_that_corrupts_the_ring_loses_only_that_stream() {
    let running = service();
    let mut raw = RawClient::connect(&running.socket);
    let (layout, fds) = raw.open_stream();
    let mapped = LinuxStreams
        .map_ring(fds[0].as_fd(), layout.ring_bytes())
        .unwrap();
    let mut other = RawClient::connect(&running.socket);
    other.send(&Frame::StreamOpen {
        id: 4,
        device: DEVICE,
        name: "other",
    });
    other.next(|frame| matches!(frame, Frame::StreamReady { .. }).then_some(()));
    let mut producing = producer(&running);

    mapped.words()[offsets::RETURNS_HEAD].store(u64::MAX, Ordering::SeqCst);
    let _ = producing.next_buffer().unwrap();

    let stats = host(&running).stream_stats(DEVICE, NAME).unwrap();
    assert_eq!(stats.faults, 1);
    assert!(!stats.attached);
    assert_eq!(
        mapped.words()[offsets::STATE].load(Ordering::SeqCst),
        STATE_FAULT
    );
    assert!(
        host(&running)
            .stream_stats(DEVICE, "other")
            .unwrap()
            .attached
    );
    assert!(raw.still_answers(), "the connection is untouched");
    assert!(other.still_answers());
}

#[test]
fn a_client_that_disappears_without_reading_its_answer_releases_the_stream() {
    let running = service();
    let mut raw = RawClient::connect(&running.socket);
    raw.send(&Frame::StreamOpen {
        id: 5,
        device: DEVICE,
        name: NAME,
    });
    // Not reading the answer: gone straight away.
    drop(raw);
    assert!(poll_until(|| {
        !host(&running).stream_stats(DEVICE, NAME).unwrap().attached
    }));
}

#[test]
fn a_client_refused_a_stream_can_carry_on() {
    let running = service();
    let (_holder, _stream) = open(&running);
    let mut raw = RawClient::connect(&running.socket);
    raw.send(&Frame::StreamOpen {
        id: 1,
        device: 42,
        name: NAME,
    });
    assert_eq!(raw.next_refusal_status(), Status::NOT_FOUND.0);
    assert!(raw.still_answers());
}
