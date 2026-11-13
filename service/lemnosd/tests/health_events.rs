//! A client hears when a device's health changes, even when its state does not. Each case runs a
//! real daemon on the sim and reads its events through `lemnos-client`. The first case is the one
//! that failed before the daemon looked at health: a device that went from ok to degraded, and
//! back, with no state change, sent nothing until its next state change.

mod common;

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use common::start_board;
use lemnos_client::{Client, DeviceHealth, DeviceState, Event};
use lemnos_core::{Config as DriverConfig, Health, Lifecycle, Result, contract, driver};
use lemnos_host::entry;
use lemnos_sim::SimBoard;

/// How soon a change of health must reach a subscriber.
const BOUND: Duration = Duration::from_secs(1);

/// The health the knob reports: 0 ok, 1 degraded, anything else failed.
static LEVEL: AtomicU8 = AtomicU8::new(0);
/// The tests share the knob and the flapper, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

#[contract("health-knob", version = 1)]
pub trait HealthKnob {
    #[method(1)]
    fn ping(&mut self) -> Result<u32>;
}

/// A driver whose state never changes and whose health is whatever the test set.
#[driver("health-knob", provides = [HealthKnob])]
pub struct Knob {}

impl Lifecycle for Knob {
    type Needs = KnobNeeds;

    fn start(_: KnobNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self {})
    }

    fn health(&self) -> Health {
        match LEVEL.load(Ordering::Relaxed) {
            0 => Health::Ok,
            1 => Health::Degraded("the test degraded it"),
            _ => Health::Failed("the test failed it"),
        }
    }
}

impl HealthKnob for Knob {
    fn ping(&mut self) -> Result<u32> {
        Ok(1)
    }
}

#[contract("health-flapper", version = 1)]
pub trait HealthFlapper {
    #[method(1)]
    fn ping(&mut self) -> Result<u32>;
}

/// A driver that ticks a thousand times a second and whose health changes from tick to tick in a
/// pattern that does not repeat every look. A plain alternation would land on the same phase at
/// every look (250 ticks is an even number), so every look would see the same health.
#[driver("health-flapper", provides = [HealthFlapper])]
pub struct Flapper {
    lcg: u32,
}

impl Lifecycle for Flapper {
    type Needs = FlapperNeeds;
    const TICKS: bool = true;

    fn start(_: FlapperNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self { lcg: 1 })
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.lcg = self.lcg.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        Some(now_ns + 1_000_000)
    }

    fn health(&self) -> Health {
        if self.lcg >> 16 & 1 == 0 {
            Health::Ok
        } else {
            Health::Degraded("flapping")
        }
    }
}

impl HealthFlapper for Flapper {
    fn ping(&mut self) -> Result<u32> {
        Ok(1)
    }
}

/// Pumps `watcher` for up to `for_`, keeping every event for `device`. Stops early after an event
/// `stop` accepts. The client only reads its socket inside a call, so each pass waits briefly.
fn pump(
    watcher: &Client,
    device: u32,
    for_: Duration,
    mut stop: impl FnMut(&Event) -> bool,
) -> Vec<Event> {
    let deadline = Instant::now() + for_;
    let mut kept = Vec::new();
    let mut into = [0; 8];
    while Instant::now() < deadline {
        let _ = watcher.next_value(&mut into, Duration::from_millis(5));
        while let Some(event) = watcher.next_event() {
            if event.device != device {
                continue;
            }
            let done = stop(&event);
            kept.push(event);
            if done {
                return kept;
            }
        }
    }
    kept
}

const KNOB: &str = "[[devices]]\nid = \"k\"\ndriver = \"health-knob\"\n";

#[test]
fn a_health_change_with_no_state_change_reaches_the_subscriber_and_its_recovery_does_too() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    LEVEL.store(0, Ordering::Relaxed);
    let running = start_board(KNOB, vec![entry::<Knob>()], SimBoard::new(), None);
    let client = Client::connect(&running.socket, "w").unwrap();
    // Listing first is what makes the watcher one that keeps its state current.
    let listed = client.list().unwrap();
    assert_eq!(listed[0].state, DeviceState::Running);
    assert_eq!(listed[0].health, DeviceHealth::Ok);

    LEVEL.store(1, Ordering::Relaxed);
    let started = Instant::now();
    let seen = pump(&client, 0, BOUND, |event| {
        event.health == DeviceHealth::Degraded
    });
    let heard = seen
        .last()
        .filter(|event| event.health == DeviceHealth::Degraded);
    assert!(
        heard.is_some(),
        "no Degraded event within {BOUND:?} of the health change"
    );
    assert!(started.elapsed() < BOUND);
    assert_eq!(heard.unwrap().state, DeviceState::Running);

    // Exactly once: nothing more is sent for a change that has already been sent.
    let after = pump(&client, 0, Duration::from_millis(1000), |_| false);
    assert!(
        after.is_empty(),
        "a steady degraded device sent {} more event(s)",
        after.len()
    );

    LEVEL.store(0, Ordering::Relaxed);
    let started = Instant::now();
    let seen = pump(&client, 0, BOUND, |event| event.health == DeviceHealth::Ok);
    assert!(
        seen.last()
            .is_some_and(|event| event.health == DeviceHealth::Ok),
        "no Ok event within {BOUND:?} of the recovery"
    );
    assert!(started.elapsed() < BOUND);
    let after = pump(&client, 0, Duration::from_millis(1000), |_| false);
    assert!(after.is_empty(), "a recovered device sent more events");
}

#[test]
fn a_flapping_driver_sends_no_more_than_one_event_per_look() {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let running = start_board(
        "[[devices]]\nid = \"f\"\ndriver = \"health-flapper\"\n",
        vec![entry::<Flapper>()],
        SimBoard::new(),
        None,
    );
    let client = Client::connect(&running.socket, "w").unwrap();
    client.list().unwrap();
    let events = pump(&client, 0, Duration::from_secs(4), |_| false);
    // Looks are 250 ms apart, so four seconds hold about sixteen. Each event is a change, so no
    // two in a row agree on health; the bound is what a flood would break.
    assert!(
        events.len() >= 2,
        "a flapping device sent {} event(s)",
        events.len()
    );
    assert!(
        events.len() <= 20,
        "a flapping device flooded the subscriber: {} events in four seconds",
        events.len()
    );
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].health != pair[1].health)
    );
}
