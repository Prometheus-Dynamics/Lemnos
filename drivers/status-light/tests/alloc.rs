//! Steady state allocates nothing, including when callers come and go.

#[allow(dead_code)]
mod support;

use std::sync::atomic::{AtomicUsize, Ordering};

use lemnos_contract_light::Light;
use lemnos_core::{
    CallerId, Config, Dev, Health, Lifecycle, LocalCaller, MethodId, Result, Router,
};
use lemnos_driver_status_light::StatusLight;
use lemnos_measure::test_alloc::{Counting, allocations};
use support::{fill, plain, settle};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A `pixels/1` stand-in that allocates nothing: it counts what it is sent.
struct Quiet {
    shows: AtomicUsize,
}

impl Router for Quiet {
    fn call(
        &self,
        _device: u32,
        method: MethodId,
        _args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize> {
        match method.method {
            1 => {
                reply[..2].copy_from_slice(&4u16.to_le_bytes());
                Ok(2)
            }
            2 => Ok(0),
            3 => {
                self.shows.fetch_add(1, Ordering::Relaxed);
                Ok(0)
            }
            _ => Err(lemnos_core::Status::NOT_FOUND),
        }
    }

    fn health(&self, _device: u32) -> Health {
        Health::Ok
    }
}

#[test]
fn steady_state_allocates_nothing_with_callers_coming_and_going() {
    let quiet: &'static Quiet = Box::leak(Box::new(Quiet {
        shows: AtomicUsize::new(0),
    }));
    let needs = lemnos_driver_status_light::StatusLightNeeds {
        pixels: Dev::new(LocalCaller::new(quiet, 0, 0)),
    };
    let mut light = <StatusLight as Lifecycle>::start(needs, &Config::new(&plain(4))).unwrap();
    let red = fill(0xff0000);
    let blue = fill(0x0000ff);
    // Warm up: the first draw and every change of look.
    settle(&mut light, 0, 10_000);
    for round in 0..4u64 {
        <StatusLight as Lifecycle>::caller(&mut light, CallerId(5));
        light.show(0, 200, &red).unwrap();
        <StatusLight as Lifecycle>::caller(&mut light, CallerId(7));
        light.show(1, 50, &blue).unwrap();
        settle(&mut light, round * 10, 10_000);
    }

    let ((), count) = allocations(|| {
        for round in 0..200u64 {
            <StatusLight as Lifecycle>::caller(&mut light, CallerId(5));
            light.show(0, 200, &red).unwrap();
            <StatusLight as Lifecycle>::caller(&mut light, CallerId(7));
            light.show(1, 50, &blue).unwrap();
            settle(&mut light, 100 + round * 10, 100_000);
            <StatusLight as Lifecycle>::caller_gone(&mut light, CallerId(5));
            settle(&mut light, 100 + round * 10 + 1, 100_000);
            <StatusLight as Lifecycle>::caller_gone(&mut light, CallerId(7));
            settle(&mut light, 100 + round * 10 + 2, 100_000);
        }
    });
    assert_eq!(count, 0, "allocations in steady state");
}
