//! Publish to hold, through a stream's shared memory: how long from the producer's publish until
//! the consumer holds the frame, measured in one process over the shm objects a stream is made of
//! (`cargo run --release -p lemnos-wire --features testing --example stream_hold`).
//!
//! Prints the median and the spread of the samples, and how many of the consumer's reads found the
//! frame already there. It makes no claim about another machine.

// The example's report is its output.
#![allow(clippy::print_stdout)]

use std::time::Instant;

use lemnos_wire::ring::Layout;
use lemnos_wire::testing::Harness;
use shm_ring::SampleTime;

const FRAMES: u64 = 200_000;

fn main() {
    let layout = Layout::new(4, 4096, 4, 64).expect("layout");
    let (mut host, mut consumer) = Harness::new(layout, true, false).expect("stream");
    let mut samples = Vec::with_capacity(FRAMES as usize);
    let mut ready_at_first_look = 0_u64;
    for sequence in 0..FRAMES {
        let buffer = host.take().expect("a free buffer");
        let started = Instant::now();
        assert!(host.publish(buffer, 16, sequence, 7, SampleTime::now().as_nanos()));
        let frame = consumer
            .pop_frame()
            .expect("no error")
            .expect("the frame is there");
        samples.push(started.elapsed().as_nanos() as u64);
        ready_at_first_look += 1;
        assert_eq!(frame.sequence, sequence);
        assert!(consumer.release(frame.buffer));
        host.reclaim();
    }
    samples.sort_unstable();
    let pick = |p: f64| samples[((samples.len() - 1) as f64 * p) as usize];
    println!(
        "publish to hold, {FRAMES} frames: median {} ns, p90 {} ns, p99 {} ns, max {} ns; {} of {FRAMES} found at the first look",
        pick(0.5),
        pick(0.9),
        pick(0.99),
        samples[samples.len() - 1],
        ready_at_first_look,
    );
}
