//! The late-reader guarantee of a frame stream, measured on the real ring.
//!
//! A producer keeps `working` buffers in its own hands at the moment it asks for its next buffer
//! (a driver that publishes before it asks has none; a device with a queue of `working + 1` buffers
//! has `working`). A reader that is `lag` frames behind its newest frame at every producer period
//! has `lag` frames unread at each ask. The stream loses nothing while a free buffer is there at
//! every ask, which is `lag <= buffers - 1 - working`. Past that, the oldest unread frame is
//! replaced and counted in `dropped`, and the newest never is.

use std::collections::VecDeque;
use std::sync::Arc;

use lemnos_core::CallerId;
use lemnos_fake_devices::FakeCounter;
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_sim::SimBoard;

const CONSUMER: CallerId = CallerId(7);
const FRAME: usize = 64;
const PERIODS: u64 = 3000;

fn start(buffers: u32) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = {buffers}\nbuffer_bytes = {FRAME}\njob_slots = 2\njob_bytes = 16\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start(
        &parse(&text).unwrap(),
        &[entry::<FakeCounter>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap()
}

/// Every frame's bytes are a function of its sequence, so a buffer refilled under a reader shows.
fn pattern(sequence: u64) -> [u8; FRAME] {
    let mut out = [0u8; FRAME];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = (sequence >> ((i % 8) * 8)) as u8 ^ (i as u8).wrapping_mul(13);
    }
    out
}

#[derive(Default)]
struct Run {
    /// Sequences the reader took, in order.
    seen: Vec<u64>,
    /// Frames the producer had no buffer to publish.
    missed: u64,
    /// Frames whose bytes were not the pattern of their sequence.
    bad_bytes: u64,
    dropped: u64,
    returned: u64,
}

/// Runs a producer of `periods` frames, `working` buffers in its hands at each ask, and a reader
/// `lag` frames behind: at period `p` it takes the frame `p - lag` (after the producer's ask of
/// period `p - 1`), so `lag` frames are unread at every ask.
fn run(buffers: u32, working: u32, lag: u64) -> Run {
    assert!(
        buffers > working,
        "a producer needs its working set and one more"
    );
    let host = start(buffers);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    let mut queue: VecDeque<u32> = VecDeque::new();
    for _ in 0..=working {
        queue.push_back(producer.next_buffer().unwrap().expect("a buffer"));
    }
    let mut out = Run::default();
    let read = |consumer: &mut lemnos_wire::ring::Consumer, out: &mut Run| {
        // A replaced frame reads as a skip, so the first pop may come back empty.
        for _ in 0..3 {
            if let Some(slot) = consumer.pop_frame().unwrap() {
                let mut got = [0u8; FRAME];
                assert!(consumer.read(slot.buffer, 0, &mut got[..slot.bytes as usize]));
                if got[..slot.bytes as usize] != pattern(slot.sequence)[..slot.bytes as usize] {
                    out.bad_bytes += 1;
                }
                out.seen.push(slot.sequence);
                assert!(consumer.release(slot.buffer));
                return;
            }
        }
    };
    for p in 0..PERIODS {
        if p >= lag {
            read(&mut consumer, &mut out);
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
        // The ask: the device takes a buffer back when it has one to give.
        if let Some(buffer) = producer.next_buffer().unwrap() {
            queue.push_back(buffer);
        }
    }
    // The frames still unread at the end.
    for _ in 0..lag {
        read(&mut consumer, &mut out);
    }
    let stats = host.stream_stats(0, "frames").unwrap();
    out.dropped = stats.dropped;
    out.returned = stats.returned;
    assert_eq!(stats.faults, 0, "{stats:?}");
    out
}

fn assert_loss_free(buffers: u32, working: u32, lag: u64) {
    let out = run(buffers, working, lag);
    let expected: Vec<u64> = (0..PERIODS).collect();
    assert_eq!(out.seen, expected, "B={buffers} W={working} lag={lag}");
    assert_eq!(out.dropped, 0, "B={buffers} W={working} lag={lag}");
    assert_eq!(out.missed, 0);
    assert_eq!(out.bad_bytes, 0);
}

fn assert_beyond(buffers: u32, working: u32, lag: u64) {
    let out = run(buffers, working, lag);
    assert!(
        out.dropped > 0,
        "B={buffers} W={working} lag={lag}: nothing replaced"
    );
    assert_eq!(
        out.dropped as usize + out.seen.len(),
        PERIODS as usize,
        "every frame is either taken or counted as replaced"
    );
    assert_eq!(
        out.seen.last(),
        Some(&(PERIODS - 1)),
        "the newest frame is never replaced"
    );
    assert!(
        out.seen.windows(2).all(|pair| pair[0] < pair[1]),
        "in order"
    );
    assert_eq!(out.missed, 0, "no frame lost for want of a buffer");
    assert_eq!(out.bad_bytes, 0);
}

/// Each (buffers, working) and the reader lag N = buffers - 1 - working that it must tolerate.
macro_rules! guarantee {
    ($($name:ident: ($buffers:expr, $working:expr);)*) => {$(
        mod $name {
            use super::*;
            const B: u32 = $buffers;
            const W: u32 = $working;
            const N: u64 = (B - 1 - W) as u64;

            #[test]
            fn a_reader_at_most_n_periods_late_loses_nothing() {
                assert_loss_free(B, W, N);
                assert_loss_free(B, W, 1);
            }

            #[test]
            fn a_reader_n_plus_one_late_loses_the_oldest_and_counts_them() {
                assert_beyond(B, W, N + 1);
            }
        }
    )*};
}

guarantee! {
    b2_w0: (2, 0);
    b3_w0: (3, 0);
    b4_w0: (4, 0);
    b4_w1: (4, 1);
    b5_w2: (5, 2);
    b6_w3: (6, 3);
    b8_w4: (8, 4);
}
