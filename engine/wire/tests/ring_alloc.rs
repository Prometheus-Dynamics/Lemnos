//! The steady-state frame path of a stream allocates nothing: a frame is published, taken, read,
//! released and its buffer reclaimed, and a job posted and collected, all without allocating once
//! the stream is attached.

#![cfg(feature = "testing")]

use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_wire::ring::Layout;
use lemnos_wire::testing::Harness;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn a_thousand_frames_and_jobs_through_a_stream_allocate_nothing() {
    let layout = Layout::new(4, 4096, 4, 64).expect("layout");
    let (mut host, mut consumer) = Harness::new(layout, true, false).expect("stream");
    let mut out = [0_u8; 64];
    // One round first: whatever the first use of each structure allocates is not steady state.
    for round in 0..2_u64 {
        let frames = if round == 0 { 1 } else { 1000 };
        let ((), count) = allocations(|| {
            for sequence in 0..frames {
                let buffer = host.take().expect("a free buffer");
                assert!(host.publish(buffer, 16, sequence, 7, 9));
                let frame = consumer.pop_frame().expect("no error").expect("a frame");
                assert_eq!(frame.buffer, buffer);
                assert!(consumer.release(frame.buffer));
                host.reclaim();
                assert!(consumer.jobs().post(&[3; 8]).expect("post"));
                assert!(host.pop_job(&mut out).expect("no error").is_some());
            }
        });
        if round == 1 {
            assert_eq!(count, 0, "steady-state frames and jobs must not allocate");
        }
    }
}
