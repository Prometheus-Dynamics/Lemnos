//! Once running, the service allocates nothing: not for calls, not for values.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{COUNTER, start};
use lemnos_client::Session;
use lemnos_measure::test_alloc::{Counting, TOTAL};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn calls_and_values_allocate_nothing_anywhere_in_the_process() {
    let running = start();
    let mut session = Session::connect(&running.socket, "t").unwrap();
    let mut call = session.open("a", COUNTER).unwrap();
    let mut watched = session.open("b", COUNTER).unwrap();
    session.subscribe(&mut watched, 1, 500, false).unwrap();
    let mut reply = [0; 16];
    let mut value = [0; 16];
    let mut exercise = |session: &mut Session, rounds: usize| {
        for _ in 0..rounds {
            session.call(&mut call, 1, &[], &mut reply).unwrap();
            session
                .next_value(&mut value, Duration::from_millis(5))
                .unwrap();
        }
    };
    // Threads start, buffers reach their working size, and the event thread
    // has its first look.
    exercise(&mut session, 300);
    std::thread::sleep(Duration::from_millis(300));
    exercise(&mut session, 100);

    let before = TOTAL.load(Ordering::Relaxed);
    exercise(&mut session, 1000);
    let after = TOTAL.load(Ordering::Relaxed);
    assert_eq!(after - before, 0);
}
