//! A job-driven stream: each frame is the result of a job the consumer posted, so an unread result
//! is never replaced. The producer's ask for a buffer that is not there is refused and counted.

use std::sync::Arc;

use lemnos_core::{
    BusKind, CallerId, Config, Driver, Manifest, Need, NeedKind, Resources, Result, StreamKind,
};
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_sim::SimBoard;

const CONSUMER: CallerId = CallerId(7);

/// A driver that serves `frames` as a job-driven stream: the kind is its declaration, which the
/// board file does not state.
struct JobSource;

impl Driver for JobSource {
    const MANIFEST: Manifest = Manifest {
        name: "job-source",
        needs: &[Need {
            name: "frames",
            kind: NeedKind::Bus(BusKind::Stream),
            optional: true,
            stream: Some(StreamKind::JobDriven),
        }],
        provides: &[],
    };

    fn bind(_: &mut Resources<'_>, _: &Config<'_>) -> Result<Self> {
        Ok(Self)
    }

    fn describe(&self, _: &mut lemnos_core::Description) {}

    fn dispatch(
        &mut self,
        _: lemnos_core::MethodId,
        _: &[u8],
        _: &mut lemnos_core::Reply<'_>,
    ) -> lemnos_core::Status {
        lemnos_core::Status::NOT_SUPPORTED
    }
}
const FRAME: usize = 64;
const PERIODS: u64 = 3000;

fn start(buffers: u32) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"job-source\"\n\
         [devices.streams.frames]\nbuffers = {buffers}\nbuffer_bytes = {FRAME}\njob_slots = 2\njob_bytes = 16\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start(
        &parse(&text).unwrap(),
        &[entry::<JobSource>()],
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

/// Runs a producer that asks for a buffer each period and publishes a result on it, with a
/// reader that takes a result every `every` periods (`0` never reads until the end).
/// Returns (sequences the reader saw in order, asks refused, bad bytes, dropped, faults).
fn run(every: u64) -> (Vec<u64>, u64, u64, u64, u64) {
    let host = start(4);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    let mut seen = Vec::new();
    let mut bad = 0;
    let read = |consumer: &mut lemnos_wire::ring::Consumer, seen: &mut Vec<u64>, bad: &mut u64| {
        if let Some(slot) = consumer.pop_frame().unwrap() {
            let mut got = [0u8; FRAME];
            assert!(consumer.read(slot.buffer, 0, &mut got[..slot.bytes as usize]));
            if got[..slot.bytes as usize] != pattern(slot.sequence)[..slot.bytes as usize] {
                *bad += 1;
            }
            seen.push(slot.sequence);
            assert!(consumer.release(slot.buffer));
        }
    };
    let mut refused = 0;
    for p in 0..PERIODS {
        if every > 0 && p % every == 0 {
            read(&mut consumer, &mut seen, &mut bad);
        }
        match producer.ask_buffer().unwrap() {
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
            None => refused += 1,
        }
    }
    // Whatever the reader has not taken yet: every published result, in order.
    for _ in 0..4 {
        read(&mut consumer, &mut seen, &mut bad);
    }
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(stats.faults, 0, "{stats:?}");
    assert_eq!(stats.refused, refused, "every refused ask is counted");
    (seen, refused, bad, stats.dropped, stats.faults)
}

#[test]
fn an_unread_result_is_never_replaced_and_the_job_without_a_buffer_is_refused() {
    // The reader takes nothing until the producer has run out of buffers: the four results it
    // has not read survive every later post, and every later job is refused, not replaced.
    let (seen, refused, bad, dropped, _) = run(0);
    assert_eq!(seen, [0, 1, 2, 3], "the first four results, unreplaced");
    assert_eq!(
        refused,
        PERIODS - 4,
        "each later job is refused and counted"
    );
    assert_eq!(bad, 0, "a result's bytes survive");
    assert_eq!(dropped, 0, "nothing is silently lost: nothing is replaced");
}

#[test]
fn a_reader_that_keeps_up_gets_every_result_and_no_job_is_refused() {
    let (seen, refused, bad, dropped, _) = run(1);
    assert_eq!(seen, (0..PERIODS).collect::<Vec<_>>());
    assert_eq!((refused, bad, dropped), (0, 0, 0));
}

/// A producer that collects what the consumer returned before each job, as the job servers do, and
/// takes a job's buffer from what it holds. A consumer that keeps up is refused nothing: the
/// collection that finds nothing is not counted.
#[test]
fn collecting_returns_before_each_job_counts_no_refusal_for_a_consumer_that_keeps_up() {
    let host = start(4);
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    let mut held = Vec::new();
    for p in 0..PERIODS {
        // Collect every buffer returned so far, until the collection finds none.
        while let Some(buffer) = producer.next_buffer().unwrap() {
            held.push(buffer);
        }
        let buffer = held
            .pop()
            .expect("a buffer is held when the consumer keeps up");
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
        // The consumer takes the result and releases it at once.
        let slot = consumer.pop_frame().unwrap().expect("the result");
        assert_eq!(slot.sequence, p);
        assert!(consumer.release(slot.buffer));
    }
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!((stats.refused, stats.faults), (0, 0), "{stats:?}");
}
