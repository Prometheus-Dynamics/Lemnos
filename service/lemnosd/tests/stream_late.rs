//! The late-reader guarantee through the daemon: a real client on the socket, and a producer in
//! this process. A producer keeps `working` buffers in its hands at each ask; a reader `lag` frames
//! behind loses nothing while `lag <= buffers - 1 - working`, and past that loses the oldest,
//! counted in the stream's `dropped`, never the newest. Every frame's bytes are checked.

mod common;

use std::collections::VecDeque;
use std::sync::Arc;

use common::start_with;
use lemnos_client::{FrameStream, Session, StreamPlatform};
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;
use lemnos_linux_stream::LinuxStreams;

const DEVICE: u32 = 0;
const NAME: &str = "frames";
const FRAME: usize = 64;
const PERIODS: u64 = 1500;

fn platform() -> Arc<dyn StreamPlatform> {
    Arc::new(LinuxStreams)
}

fn pattern(sequence: u64) -> [u8; FRAME] {
    let mut out = [0u8; FRAME];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = (sequence >> ((i % 8) * 8)) as u8 ^ (i as u8).wrapping_mul(13);
    }
    out
}

#[derive(Default)]
struct Outcome {
    seen: Vec<u64>,
    missed: u64,
    bad_bytes: u64,
    dropped: u64,
    margin: u32,
}

/// Runs the same producer and reader as the host-level test, through the socket.
fn run(buffers: u32, working: u32, lag: u64) -> Outcome {
    let board = format!(
        "[[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
         [devices.streams.{NAME}]\nbuffers = {buffers}\nbuffer_bytes = {FRAME}\nworking = {working}\n"
    );
    let running = start_with(&board);
    let host = running.service.as_ref().unwrap().host();
    let mut producer = host.stream_producer(DEVICE, NAME).unwrap();
    let mut session = Session::connect(&running.socket, "reader").unwrap();
    let mut stream: FrameStream = session.open_stream(DEVICE, NAME, platform()).unwrap();
    let margin = stream.margin();
    assert_eq!(stream.working(), working);
    assert_eq!(margin, buffers - 1 - working);

    let mut queue: VecDeque<u32> = VecDeque::new();
    for _ in 0..=working {
        queue.push_back(producer.next_buffer().unwrap().expect("a buffer"));
    }
    let mut out = Outcome {
        margin,
        ..Outcome::default()
    };
    let read = |stream: &mut FrameStream, out: &mut Outcome| {
        // A replaced frame reads as a skip, so the first try may come back empty.
        for _ in 0..3 {
            if let Some(frame) = stream.try_next().unwrap() {
                let len = frame.len();
                let sequence = frame.sequence();
                if stream.bytes(&frame)[..len] != pattern(sequence)[..len] {
                    out.bad_bytes += 1;
                }
                out.seen.push(sequence);
                stream.release(frame).unwrap();
                return;
            }
        }
    };
    for p in 0..PERIODS {
        if p >= lag {
            read(&mut stream, &mut out);
        }
        match queue.pop_front() {
            Some(buffer) => {
                assert!(producer.write(buffer, 0, &pattern(p)).unwrap());
                producer
                    .publish(StreamFrame {
                        buffer,
                        bytes: FRAME as u32,
                        sequence: p,
                        identity: 0,
                        timestamp_ns: p + 1,
                    })
                    .unwrap();
            }
            None => out.missed += 1,
        }
        if let Some(buffer) = producer.next_buffer().unwrap() {
            queue.push_back(buffer);
        }
    }
    for _ in 0..lag {
        read(&mut stream, &mut out);
    }
    let stats = host.stream_stats(DEVICE, NAME).unwrap();
    out.dropped = stats.dropped;
    assert_eq!(stats.faults, 0, "{stats:?}");
    out
}

#[test]
fn a_real_client_at_most_n_behind_loses_nothing_for_several_working_sets() {
    for (buffers, working) in [(4, 0), (4, 1), (5, 2)] {
        let n = u64::from(buffers - 1 - working);
        let out = run(buffers, working, n);
        assert_eq!(out.margin, buffers - 1 - working);
        assert_eq!(
            out.seen,
            (0..PERIODS).collect::<Vec<_>>(),
            "B={buffers} W={working}"
        );
        assert_eq!(out.dropped, 0, "B={buffers} W={working}");
        assert_eq!(out.missed, 0);
        assert_eq!(out.bad_bytes, 0);
    }
}

#[test]
fn a_real_client_n_plus_one_behind_loses_the_oldest_and_never_the_newest() {
    for (buffers, working) in [(4, 0), (4, 1), (5, 2)] {
        let lag = u64::from(buffers - 1 - working) + 1;
        let out = run(buffers, working, lag);
        assert!(out.dropped > 0, "B={buffers} W={working}");
        assert_eq!(out.dropped as usize + out.seen.len(), PERIODS as usize);
        assert_eq!(
            out.seen.last(),
            Some(&(PERIODS - 1)),
            "the newest is never replaced"
        );
        assert!(out.seen.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(out.missed, 0);
        assert_eq!(out.bad_bytes, 0);
    }
}

#[test]
fn a_board_working_set_at_least_its_buffers_is_refused() {
    let board = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
         [devices.streams.{NAME}]\nbuffers = 4\nbuffer_bytes = {FRAME}\nworking = 4\n"
    );
    assert!(lemnos_host::board::parse(&board).is_err());
}
