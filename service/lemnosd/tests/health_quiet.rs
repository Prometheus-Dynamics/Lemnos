//! Once a device's health has been sent, looking at it again sends nothing and allocates nothing.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::start_board;
use lemnos_client::Client;
use lemnos_core::{Config as DriverConfig, Health, Lifecycle, Result, contract, driver};
use lemnos_host::entry;
use lemnos_measure::test_alloc::{Counting, TOTAL};
use lemnos_sim::SimBoard;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[contract("quiet-degraded", version = 1)]
pub trait QuietDegraded {
    #[method(1)]
    fn ping(&mut self) -> lemnos_core::Result<u32>;
}

/// A driver that stays degraded, and says so on every look.
#[driver("quiet-degraded", provides = [QuietDegraded])]
pub struct Quiet {}

impl Lifecycle for Quiet {
    type Needs = QuietNeeds;

    fn start(_: QuietNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self {})
    }

    fn health(&self) -> Health {
        Health::Degraded("it stays degraded")
    }
}

impl QuietDegraded for Quiet {
    fn ping(&mut self) -> Result<u32> {
        Ok(1)
    }
}

#[test]
fn a_steady_device_is_looked_at_and_sends_and_allocates_nothing() {
    let running = start_board(
        "[[devices]]\nid = \"q\"\ndriver = \"quiet-degraded\"\n",
        vec![entry::<Quiet>()],
        SimBoard::new(),
        None,
    );
    let client = Client::connect(&running.socket, "w").unwrap();
    client.list().unwrap();
    // Threads start, the first look is taken, and the change is sent.
    std::thread::sleep(Duration::from_millis(600));
    let before = TOTAL.load(Ordering::Relaxed);
    // Four looks at the interval the event thread keeps.
    std::thread::sleep(Duration::from_millis(1100));
    let after = TOTAL.load(Ordering::Relaxed);
    assert_eq!(after - before, 0, "the steady daemon allocated");
    assert!(
        client.next_event().is_none(),
        "a steady device sent an event"
    );
}
