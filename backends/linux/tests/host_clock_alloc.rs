//! The Linux host clock a driver reads allocates nothing once it is handed over.

use lemnos_core::Provider;
use lemnos_linux::LinuxBoard;
use lemnos_measure::test_alloc::{Counting, allocations};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn reading_the_host_clock_allocates_nothing() {
    let mut board = LinuxBoard::new();
    let clock = board.host_clock("clock").unwrap();
    let mut last = clock.now_ns();
    let (backwards, count) = allocations(|| {
        let mut backwards = 0;
        for _ in 0..10_000 {
            let now = clock.now_ns();
            if now < last {
                backwards += 1;
            }
            last = now;
        }
        backwards
    });
    assert_eq!(count, 0, "allocations in 10000 clock reads");
    assert_eq!(backwards, 0, "the clock went backwards");
}
