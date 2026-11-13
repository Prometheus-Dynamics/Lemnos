//! The frame path allocates nothing once running.

use std::sync::Arc;

use lemnos_core::CallerId;
use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_sim::{SimBoard, SimFrameSource};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn a_thousand_frames_through_the_host_allocate_nothing() {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n\
                [devices.streams.frames]\nbuffers = 4\nbuffer_bytes = 64\n";
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<FakeCounter>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap();
    let mut producer = host.stream_producer(0, "frames").unwrap();
    let writer = producer.writer();
    let mut consumer = host
        .stream_attach(0, "frames", CallerId(3))
        .unwrap()
        .consumer()
        .unwrap();
    let mut source = SimFrameSource::new(64);
    let mut frame = |sequence: u64| {
        let mut write = |buffer, bytes: &[u8]| writer.write(buffer, 0, bytes);
        assert!(source.produce(&mut producer, &mut write).unwrap());
        let got = consumer.pop_frame().unwrap().unwrap();
        assert_eq!(got.sequence, sequence);
        assert_eq!(got.identity, 0);
        assert!(consumer.release(got.buffer));
    };
    for sequence in 0..10 {
        frame(sequence);
    }
    let ((), count) = allocations(|| {
        for sequence in 10..1010 {
            frame(sequence);
        }
    });
    assert_eq!(count, 0, "allocations in 1000 frames");
}
