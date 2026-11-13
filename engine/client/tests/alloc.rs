//! Steady-state round trips allocate nothing on the client.

mod common;

use common::FakeHost;
use lemnos_client::{Client, Incoming, Session};
use lemnos_core::{Result, contract};
use lemnos_measure::test_alloc::{Counting, allocations};

#[contract("example.counter", version = 1)]
pub trait Counter {
    #[method(1)]
    fn add(&mut self, value: u32) -> Result<u32>;
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn a_thousand_polled_round_trips_allocate_nothing() {
    let host = FakeHost::start();
    let mut session = Session::connect_polled(&host.path, "t").unwrap();
    let handle = session.open("", "example.counter/1").unwrap();
    let args = [1u8; 32];
    let round_trip = |session: &mut Session| {
        let id = session.send_call(&handle, 1, &args, 64).unwrap();
        let mut answered = false;
        while !answered {
            session.poll(&mut |incoming| {
                if let Incoming::Reply { id: got, .. } = incoming {
                    answered = got == id;
                }
            });
        }
    };
    // The first trips grow the buffers to their working size.
    for _ in 0..10 {
        round_trip(&mut session);
    }
    let ((), count) = allocations(|| {
        for _ in 0..1000 {
            round_trip(&mut session);
        }
    });
    assert_eq!(count, 0);
}

#[test]
fn a_thousand_blocking_calls_allocate_nothing() {
    let host = FakeHost::start();
    let mut session = Session::connect(&host.path, "t").unwrap();
    let mut handle = session.open("", "example.counter/1").unwrap();
    let args = [2u8; 16];
    let mut reply = [0u8; 64];
    for _ in 0..10 {
        session.call(&mut handle, 1, &args, &mut reply).unwrap();
    }
    let ((), count) = allocations(|| {
        for _ in 0..1000 {
            session.call(&mut handle, 1, &args, &mut reply).unwrap();
        }
    });
    assert_eq!(count, 0);
}

/// A typed client's layout check runs once, when it opens: the calls after it are the same as
/// the untyped ones, with nothing added per call.
#[test]
fn after_a_typed_open_a_thousand_calls_allocate_nothing() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "t").unwrap();
    let mut counter = client.open::<dyn Counter>("").unwrap();
    for _ in 0..10 {
        counter.add(2).unwrap();
    }
    let ((), count) = allocations(|| {
        for _ in 0..1000 {
            counter.add(2).unwrap();
        }
    });
    assert_eq!(count, 0);
}
