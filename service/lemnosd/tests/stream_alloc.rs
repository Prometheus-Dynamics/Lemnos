//! The whole process allocates nothing per frame: producer, consumer and service together.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::start_with;
use lemnos_client::Session;
use lemnos_linux_stream::LinuxStreams;
use lemnos_measure::test_alloc::{Counting, TOTAL};
use lemnos_sim::SimFrameSource;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn frames_allocate_nothing_anywhere_in_the_process() {
    let running = start_with(
        "[[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = 4\nbuffer_bytes = 4096\n",
    );
    let mut session = Session::connect(&running.socket, "t").unwrap();
    let mut stream = session
        .open_stream(0, "frames", Arc::new(LinuxStreams))
        .unwrap();
    let service = running.service.as_ref().unwrap();
    let mut producer = service.host().stream_producer(0, "frames").unwrap();
    let writer = producer.writer();
    let mut source = SimFrameSource::new(4096);
    let mut frame = |sequence: u64| {
        let mut write = |buffer, bytes: &[u8]| writer.write(buffer, 0, bytes);
        assert!(source.produce(&mut producer, &mut write).unwrap());
        let got = stream.next_frame(Duration::from_secs(5)).unwrap().unwrap();
        assert_eq!(got.sequence(), sequence);
        assert_eq!(got.identity(), SimFrameSource::identity(sequence));
        assert_eq!(stream.bytes(&got).len(), 4096);
        stream.release(got).unwrap();
    };
    // Threads start and settle.
    for sequence in 0..300 {
        frame(sequence);
    }
    std::thread::sleep(Duration::from_millis(300));
    for sequence in 300..400 {
        frame(sequence);
    }

    let before = TOTAL.load(Ordering::Relaxed);
    for sequence in 400..1400 {
        frame(sequence);
    }
    let after = TOTAL.load(Ordering::Relaxed);
    assert_eq!(after - before, 0, "allocations in 1000 frames");
    drop(session);
}
