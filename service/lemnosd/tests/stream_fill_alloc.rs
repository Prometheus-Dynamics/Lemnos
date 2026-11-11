//! A consumer filling a job's buffers allocates nothing per job: the client's writes, the
//! service's ticks and the driver's reads together. Sealed memfds stand in for the dma-bufs.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::StandIn;
use common::filled::{App, BUFFERS, IMAGE, Job, KEY, made, start};
use lemnos_measure::test_alloc::{Counting, TOTAL};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn jobs_on_buffers_the_client_fills_allocate_nothing_anywhere_in_the_process() {
    let running = start(Arc::new(StandIn::default()), "");
    let mut app = App::attach(&running);
    // Made once: two images and two keys, and what the driver makes of each pair.
    let images: Vec<Vec<u8>> = (0..2u8)
        .map(|seed| (0..IMAGE).map(|at| at as u8 ^ seed).collect())
        .collect();
    let keys: [[u8; KEY]; 2] = [[3; KEY], [0xA5; KEY]];
    let expected: Vec<Vec<u8>> = (0..2).map(|n| made(&images[n], &keys[n])).collect();
    let mut one = |n: u64| {
        let buffer = (n % u64::from(BUFFERS)) as u32;
        let which = (n % 2) as usize;
        let job = Job {
            identity: n,
            config: buffer,
            input: buffer,
            output: buffer,
        };
        app.run(job, &keys[which], &images[which], &expected[which]);
    };
    // Threads start and settle.
    for n in 0..300 {
        one(n);
    }
    std::thread::sleep(Duration::from_millis(300));
    for n in 300..400 {
        one(n);
    }

    let before = TOTAL.load(Ordering::Relaxed);
    for n in 400..1400 {
        one(n);
    }
    let after = TOTAL.load(Ordering::Relaxed);
    assert_eq!(after - before, 0, "allocations in 1000 jobs");
}
