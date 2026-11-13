//! A client that is listening for events sees a device that comes up later. The first three
//! cases hold on the old daemon too. `a_client_that_listed_a_device_off_hears_it_run` is the one
//! that failed before the daemon sent an event for every change: a device that went off and came
//! back between two of the daemon's looks was never sent, so a client that listed it while off
//! kept that state forever. That is the case the bridge's one-second re-check papered over.

mod common;

use std::time::{Duration, Instant};

use common::{start_board, start_with};
use lemnos_client::{Client, DeviceState};
use lemnos_core::{Config as DriverConfig, Lifecycle, Line, Result, contract, driver};
use lemnos_host::entry;
use lemnos_sim::SimBoard;

const EVENT_WAIT: Duration = Duration::from_secs(5);

/// Pumps `watcher` until an event for `device` in `state` arrives. The client only reads its
/// socket inside a call, so each pass waits briefly for a value, as the bridge does.
fn an_event_for(watcher: &Client, device: u32, state: DeviceState) -> bool {
    let deadline = Instant::now() + EVENT_WAIT;
    let mut into = [0; 8];
    while Instant::now() < deadline {
        let _ = watcher.next_value(&mut into, Duration::from_millis(5));
        while let Some(event) = watcher.next_event() {
            if event.device == device && event.state == state {
                return true;
            }
        }
    }
    false
}

#[test]
fn a_device_switched_on_after_subscribing_is_announced() {
    let running = start_with("[[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n");
    let client = Client::connect(&running.socket, "t").unwrap();
    let watcher = Client::connect(&running.socket, "w").unwrap();
    client.set_power(0, false).unwrap();
    assert!(an_event_for(&watcher, 0, DeviceState::Off));
    client.set_power(0, true).unwrap();
    assert!(
        an_event_for(&watcher, 0, DeviceState::Running),
        "no Running event after the device came up"
    );
}

#[contract("late-plug", version = 1)]
pub trait LatePlug {
    #[method(1)]
    fn answer(&mut self) -> lemnos_core::Result<u32>;
}

#[driver("late-plug", provides = [LatePlug])]
pub struct Plug {
    #[needs]
    _wire: Line,
}

impl Lifecycle for Plug {
    type Needs = PlugNeeds;

    fn start(needs: PlugNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self { _wire: needs._wire })
    }
}

impl LatePlug for Plug {
    fn answer(&mut self) -> Result<u32> {
        Ok(42)
    }
}

/// Waits for `done`, for up to a few seconds.
fn eventually(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + EVENT_WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_device_that_is_retrying_then_binds_at_a_hint_is_announced() {
    let board = SimBoard::new();
    let mut running = start_board(
        "[[devices]]\nid = \"cam\"\ndriver = \"late-plug\"\nbus = \"port-a\"\n",
        vec![entry::<Plug>()],
        board.clone(),
        None,
    );
    running
        .service
        .as_mut()
        .unwrap()
        .watch(Box::new(board.hints()))
        .unwrap();
    let watcher = Client::connect(&running.socket, "w").unwrap();
    // Its bus is missing: the device fails to bind and waits to retry. The watcher reads that from
    // the list, which is what a client holds when the device then comes up.
    eventually(|| watcher.list().unwrap()[0].state == DeviceState::BackingOff);
    board.add_line("port-a");
    assert!(
        an_event_for(&watcher, 0, DeviceState::Running),
        "no Running event after the hint bound the device"
    );
}

#[test]
fn a_refused_describe_of_an_off_device_does_not_hide_its_later_event() {
    let running = start_with("[[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n");
    let client = Client::connect(&running.socket, "t").unwrap();
    let watcher = Client::connect(&running.socket, "w").unwrap();
    client.set_power(0, false).unwrap();
    assert!(an_event_for(&watcher, 0, DeviceState::Off));
    assert!(watcher.describe(0).is_err());
    client.set_power(0, true).unwrap();
    assert!(
        an_event_for(&watcher, 0, DeviceState::Running),
        "no Running event after a refused describe"
    );
}

#[test]
fn a_client_that_listed_a_device_off_hears_it_run() {
    let running = start_with("[[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n");
    let client = Client::connect(&running.socket, "t").unwrap();
    client.set_power(0, false).unwrap();
    // The bridge's own start: a connection made lazily, a list, and a describe that is refused
    // because the device is off. Then the device comes up with no pause in between.
    let watcher = Client::connecting(&running.socket, "w");
    assert_eq!(watcher.list().unwrap()[0].state, DeviceState::Off);
    assert!(watcher.describe(0).is_err());
    client.set_power(0, true).unwrap();
    assert!(
        an_event_for(&watcher, 0, DeviceState::Running),
        "the list said off and no event said it came up"
    );
}
