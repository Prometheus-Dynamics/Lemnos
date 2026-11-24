//! A device's last stall is kept after it recovers and reaches a client (`last_stalls`): its
//! reason, cleared from the device when it works again, can still be read. Before the stall was
//! kept, a client that listed after a quick recovery read an empty reason and could not tell why
//! its stream had stalled.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::start_board;
use lemnos_client::{Client, DeviceHealth, Error, Stall};
use lemnos_core::{Config as DriverConfig, Health, Lifecycle, Result, contract, driver};
use lemnos_host::entry;
use lemnos_sim::SimBoard;

/// Whether the driver reports itself failed.
static FAILED: AtomicBool = AtomicBool::new(false);

#[contract("stall-knob", version = 1)]
pub trait StallKnob {
    #[method(1)]
    fn ping(&mut self) -> Result<u32>;
}

/// A ticking driver that reports failed (recovering by itself) while the test says so.
#[driver("stall-knob", provides = [StallKnob])]
pub struct Knob {}

impl Lifecycle for Knob {
    type Needs = KnobNeeds;
    const TICKS: bool = true;

    fn start(_: KnobNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self {})
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        Some(now_ns + 1_000_000)
    }

    fn health(&self) -> Health {
        if FAILED.load(Ordering::Relaxed) {
            Health::Failed("the test failed it")
        } else {
            Health::Ok
        }
    }
}

impl StallKnob for Knob {
    fn ping(&mut self) -> Result<u32> {
        Ok(1)
    }
}

/// Asks for the last stalls until `done` accepts the device's, for at most two seconds.
fn stall_until(client: &Client, done: impl Fn(&Stall) -> bool) -> Option<Stall> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        // No answer in time under load: look again, until the deadline.
        let stalls = match client.last_stalls() {
            Ok(stalls) => stalls,
            Err(Error::TimedOut) => Vec::new(),
            Err(e) => panic!("last_stalls: {e:?}"),
        };
        let stall = stalls.into_iter().find(|s| s.name == "k");
        if stall.as_ref().is_some_and(&done) || Instant::now() > deadline {
            return stall;
        }
        std::thread::yield_now();
    }
}

#[test]
fn a_recovered_device_lists_its_last_stall_with_the_reason_it_no_longer_shows() {
    let running = start_board(
        "[[devices]]\nid = \"k\"\ndriver = \"stall-knob\"\n",
        vec![entry::<Knob>()],
        SimBoard::new(),
        None,
    );
    let client = Client::connect(&running.socket, "w").unwrap();
    assert_eq!(client.last_stalls().unwrap(), Vec::new(), "never stalled");

    FAILED.store(true, Ordering::Relaxed);
    let standing = stall_until(&client, |_| true).expect("a standing stall");
    assert_eq!(
        (standing.ended_ns, standing.reason.as_str()),
        (None, "the test failed it")
    );

    FAILED.store(false, Ordering::Relaxed);
    let ended = stall_until(&client, |s| s.ended_ns.is_some()).expect("kept after the recovery");
    let back = client.list().unwrap().remove(0);
    assert_eq!(back.health, DeviceHealth::Ok);
    assert_eq!(back.reason, "", "the device's own reason is cleared");
    assert_eq!(
        (ended.since_ns, ended.reason.as_str()),
        (standing.since_ns, "the test failed it")
    );
    assert!(ended.ended_ns.unwrap() >= ended.since_ns, "{ended:?}");
}
