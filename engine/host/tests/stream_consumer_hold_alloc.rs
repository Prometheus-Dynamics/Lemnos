//! The steady state of a stream whose consumer declared its hold allocates nothing: the publish, the
//! consumer's take and give-back, and the attached reader's pop, once the path has run.

mod test_options;

use std::sync::Arc;

use lemnos_core::CallerId;
use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::{Host, entry};
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_sim::{SimBoard, SimFrameSource};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const CONSUMER: CallerId = CallerId(3);

fn start() -> Host {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = 6\nbuffer_bytes = 64\nreaders = 2\n";
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start(
        &parse(text).unwrap(),
        &[entry::<FakeCounter>()],
        Box::new(sim),
        Arc::new(clock),
        test_options::options(),
    )
    .unwrap()
}

#[test]
fn steady_state_with_a_declared_hold_allocates_nothing() {
    let host = start();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let mut source = SimFrameSource::new(64);
    let mut consumer = host
        .stream_attach_declared(0, "frames", CONSUMER, 3)
        .unwrap_or_else(|refused| panic!("{}", refused.text))
        .consumer_declared(3)
        .unwrap();
    let mut step = |consumer: &mut lemnos_wire::ring::Consumer| {
        let writer = producer.writer();
        assert!(
            source
                .produce(&mut producer, &mut |buffer, bytes| writer
                    .write(buffer, 0, bytes))
                .unwrap()
        );
        let frame = consumer
            .pop_entry()
            .unwrap()
            .and_then(|entry| entry.frame())
            .expect("a frame");
        assert!(consumer.release(frame.buffer));
    };
    // Warm the path: its first frames make what they need.
    for _ in 0..16 {
        step(&mut consumer);
    }
    let ((), allocated) = allocations(|| {
        for _ in 0..256 {
            step(&mut consumer);
        }
    });
    assert_eq!(allocated, 0, "the steady state allocated {allocated} times");
}
