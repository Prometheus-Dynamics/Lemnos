//! Once a device's health has been sent, looking at it again sends nothing and allocates nothing.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::start_board;
use lemnos_client::{Client, DeviceState};
use lemnos_core::{Config as DriverConfig, Health, Lifecycle, Result, contract, driver};
use lemnos_host::entry;
use lemnos_measure::test_alloc::{Counting, TOTAL};
use lemnos_sim::SimBoard;

const WAIT: Duration = Duration::from_secs(10);

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[contract("quiet-degraded", version = 1)]
pub trait QuietDegraded {
    #[method(1)]
    fn ping(&mut self) -> lemnos_core::Result<u32>;
}

/// Whether the device has gone degraded. The test turns it on once its client is connected, so
/// the change is sent to that client and not to nobody.
static DEGRADED: AtomicBool = AtomicBool::new(false);

/// A driver that is healthy, then degraded at the test's word, and stays so on every look.
#[driver("quiet-degraded", provides = [QuietDegraded])]
pub struct Quiet {}

impl Lifecycle for Quiet {
    type Needs = QuietNeeds;

    fn start(_: QuietNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self {})
    }

    fn health(&self) -> Health {
        if DEGRADED.load(Ordering::SeqCst) {
            Health::Degraded("it stays degraded")
        } else {
            Health::Ok
        }
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
    // The device is bound before the test changes it: its bind is a change of its own, and must
    // not land in the window.
    let deadline = Instant::now() + WAIT;
    while !client
        .list()
        .unwrap()
        .iter()
        .any(|device| device.name == "q" && device.state == DeviceState::Running)
    {
        assert!(Instant::now() < deadline, "the device never came up");
        std::thread::sleep(Duration::from_millis(10));
    }
    // The change happens with the client connected, so the next look sends it: that is the one
    // event. Wait for it, rather than for a fixed time, which a loaded machine can miss.
    DEGRADED.store(true, Ordering::SeqCst);
    while !has_event(&client) {
        assert!(
            Instant::now() < deadline,
            "the device's change was never sent"
        );
    }
    // Settle: the change of the bind, if it was still landing, is sent after the one above. Wait
    // until a stretch goes by with nothing sent.
    loop {
        std::thread::sleep(Duration::from_millis(300));
        if !has_event(&client) {
            break;
        }
    }
    let before = TOTAL.load(Ordering::Relaxed);
    // Four looks at the interval the event thread keeps.
    std::thread::sleep(Duration::from_millis(1100));
    let after = TOTAL.load(Ordering::Relaxed);
    assert_eq!(after - before, 0, "the steady daemon allocated");
    assert!(!has_event(&client), "a steady device sent an event");
}

/// Reads whatever the daemon has sent (the client reads the connection only when asked), and
/// says whether an event was among it. Drains the queue.
fn has_event(client: &Client) -> bool {
    let _ = client.next_value(&mut [0u8; 8], Duration::from_millis(20));
    let mut any = false;
    while client.next_event().is_some() {
        any = true;
    }
    any
}
