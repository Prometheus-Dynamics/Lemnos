//! A job that ends without a result is a zero-buffer entry on the frame ring, in order with the
//! frames: every refusal reason reaches the consumer that posted the job, with its identity and
//! end time, and a bad result keeps its buffer and says it failed.

use std::sync::Arc;

use lemnos_core::{
    BusKind, CallerId, Config, Driver, Manifest, Need, NeedKind, Resources, Result, StreamKind,
};
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_sim::SimBoard;
use lemnos_wire::ring::{frame, reason};

const CONSUMER: CallerId = CallerId(7);
const FRAME: usize = 64;

/// A job-driven stream with no behaviour of its own: the test drives the producer end.
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

fn start() -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"job-source\"\n\
         [devices.streams.frames]\nbuffers = 4\nbuffer_bytes = {FRAME}\njob_slots = 2\njob_bytes = 16\n"
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

const REASONS: [u32; 6] = [
    reason::FAILED,
    reason::TIMEOUT,
    reason::NOT_LENT,
    reason::IDENTITY_MISMATCH,
    reason::SIZE_OR_STRIDE,
    reason::WEDGED,
];

#[test]
fn every_refusal_reason_reaches_the_consumer_that_posted_the_job() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    for (at, &code) in REASONS.iter().enumerate() {
        let identity = 1000 + at as u64;
        producer
            .publish_job_end(identity, code, 5_000 + at as u64)
            .unwrap();
        let slot = consumer
            .pop_frame()
            .unwrap()
            .expect("the job end is on the ring");
        assert_eq!(slot.buffer, frame::NO_BUFFER, "a job end names no buffer");
        assert_eq!(slot.bytes, 0);
        assert_eq!(slot.identity, identity, "{}", reason::word(code));
        assert_eq!(slot.reason, code, "{}", reason::word(code));
        assert_eq!(slot.end_ns, 5_000 + at as u64);
        assert!(
            consumer.pop_frame().unwrap().is_none(),
            "nothing else is pending"
        );
    }
    // A result the block flagged bad keeps its buffer, and carries the failed reason.
    let buffer = producer.ask_buffer().unwrap().expect("a free buffer");
    producer
        .publish(StreamFrame {
            buffer,
            bytes: 0,
            sequence: 9,
            identity: 2000,
            timestamp_ns: 1,
            end_ns: 0,
        })
        .unwrap();
    let slot = consumer.pop_frame().unwrap().expect("the failed result");
    assert_eq!(slot.buffer, buffer);
    assert_eq!(slot.bytes, 0);
    assert_eq!(slot.reason, reason::FAILED);
    assert!(consumer.release(slot.buffer));
    // A job end that names no reason is refused, not put on the ring.
    assert!(producer.publish_job_end(3000, reason::NONE, 0).is_err());
}

#[test]
fn a_job_end_with_no_consumer_attached_is_counted_dropped() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    producer.publish_job_end(1, reason::TIMEOUT, 10).unwrap();
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(stats.dropped, 1, "{stats:?}");
}
