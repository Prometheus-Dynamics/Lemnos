//! A driver whose resources are dead for good says so, and the host binds it again under the
//! failure rule: never sooner than the backoff allows, however often it asks.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use lemnos_core::{Config, Health, Lifecycle, MethodId, Result, Status, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

const DEAD: &str = "what it was bound with is dead";

/// One set per test, picked by the device's `key`, since tests share the process.
struct Shared {
    binds: AtomicU32,
    ticks: AtomicU32,
    /// The next tick finds its resources dead.
    die: AtomicBool,
}

const KEYS: usize = 8;
static SHARED: [Shared; KEYS] = [const {
    Shared {
        binds: AtomicU32::new(0),
        ticks: AtomicU32::new(0),
        die: AtomicBool::new(false),
    }
}; KEYS];

#[contract("leaver", version = 1)]
pub trait Leaving {
    /// Finds its resources dead, in a method.
    #[method(1)]
    fn leave(&mut self) -> Result<u32>;
}

#[driver("leaver", provides = [Leaving])]
pub struct Leaver {
    shared: &'static Shared,
    /// Dead from its first tick, every time it is bound.
    always: bool,
    period: u64,
    gone: bool,
}

impl Lifecycle for Leaver {
    type Needs = LeaverNeeds;
    const TICKS: bool = true;

    fn start(_: LeaverNeeds, cfg: &Config<'_>) -> Result<Self> {
        let shared = &SHARED[cfg.int("key").ok_or(Status::INVALID_ARGUMENT)? as usize];
        shared.binds.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            shared,
            always: cfg.bool("always").unwrap_or(false),
            period: cfg.int("period_ns").unwrap_or(0) as u64,
            gone: false,
        })
    }

    fn health(&self) -> Health {
        if self.gone {
            Health::Gone(DEAD)
        } else {
            Health::Ok
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.shared.ticks.fetch_add(1, Ordering::Relaxed);
        if self.always || self.shared.die.swap(false, Ordering::Relaxed) {
            self.gone = true;
        }
        (self.period > 0).then(|| now_ns + self.period)
    }
}

impl Leaving for Leaver {
    fn leave(&mut self) -> Result<u32> {
        self.gone = true;
        Ok(7)
    }
}

/// The device `d`, with `device` added to its board entry.
fn host_with(key: usize, config: &str, device: &str) -> (Host, SimClock, &'static Shared) {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"d\"\ndriver = \"leaver\"\nconfig = {{ key = {key}{config} }}\n{device}"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(&text).unwrap(),
        &[entry::<Leaver>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (host, clock, &SHARED[key])
}

fn host(key: usize, config: &str) -> (Host, SimClock, &'static Shared) {
    host_with(key, config, "")
}

fn binds(shared: &Shared) -> u32 {
    shared.binds.load(Ordering::Relaxed)
}

fn state(host: &Host) -> DeviceState {
    host.device("d").unwrap().state
}

/// The wait the device is in, in whole seconds, with its reason and attempt.
fn waiting(host: &Host, clock: &SimClock) -> (u64, u32, String) {
    match state(host) {
        DeviceState::Backoff {
            attempts,
            next_retry_ns,
            reason,
        } => (
            Duration::from_nanos(next_retry_ns - clock.now_ns()).as_secs(),
            attempts,
            reason,
        ),
        other => panic!("not waiting: {other:?}"),
    }
}

#[test]
fn a_driver_that_only_ticks_is_bound_again_when_it_says_its_resources_are_dead() {
    let (host, clock, shared) = host(0, ", period_ns = 1000");
    host.tick();
    assert_eq!(state(&host), DeviceState::Running);
    assert_eq!(binds(shared), 1);

    // Nobody calls it. Its own tick finds the handle dead.
    shared.die.store(true, Ordering::Relaxed);
    clock.advance(Duration::from_nanos(1000));
    host.tick();
    assert_eq!(waiting(&host, &clock), (1, 1, DEAD.to_string()));
    assert_eq!(host.device("d").unwrap().health, Health::Failed("retrying"));
    assert_eq!(binds(shared), 1, "not before its wait is over");
    assert_eq!(host.next_tick_ns(), None, "and not ticked while it is down");

    clock.advance(Duration::from_millis(999));
    host.tick();
    assert_eq!(binds(shared), 1);
    clock.advance(Duration::from_millis(1));
    host.tick();
    assert_eq!(state(&host), DeviceState::Running);
    assert_eq!(binds(shared), 2, "a new instance, bound afresh");
    assert_eq!(host.device("d").unwrap().health, Health::Ok);
}

#[test]
fn a_method_can_find_them_dead_and_its_own_answer_still_stands() {
    let (host, clock, shared) = host(1, "");
    let index = host.device("d").unwrap().index;
    let method = MethodId {
        contract: 0,
        method: 1,
    };
    let mut reply = [0; 4];
    assert_eq!(host.call(index, method, &[], &mut reply), Ok(4));
    assert_eq!(u32::from_le_bytes(reply), 7);
    assert_eq!(waiting(&host, &clock), (1, 1, DEAD.to_string()));
    assert_eq!(
        host.call(index, method, &[], &mut reply),
        Err(Status::NO_DEVICE),
        "the old instance is gone"
    );

    clock.advance(Duration::from_secs(1));
    host.tick();
    assert_eq!(state(&host), DeviceState::Running);
    assert_eq!(binds(shared), 2);
}

#[test]
fn a_driver_that_asks_after_every_bind_is_bound_once_per_backoff_step() {
    let (host, clock, shared) = host(2, ", always = true");
    let mut waits = Vec::new();
    for step in 1..=7 {
        // Every bind works, and the first tick after it asks for another.
        for _ in 0..50 {
            host.tick();
        }
        assert_eq!(binds(shared), step, "one bind a step, however often asked");
        let (wait, _, reason) = waiting(&host, &clock);
        assert_eq!(reason, DEAD, "and why it waits is there to read");
        waits.push(wait);
        clock.advance(Duration::from_secs(wait) - Duration::from_nanos(1));
        for _ in 0..50 {
            host.tick();
        }
        assert_eq!(binds(shared), step, "nothing before the wait is over");
        clock.advance(Duration::from_nanos(1));
    }
    assert_eq!(waits, [1, 2, 4, 8, 8, 8, 8]);
}

#[test]
fn the_wait_starts_again_from_the_shortest_once_a_driver_has_stayed_up() {
    let (host, clock, shared) = host(3, ", period_ns = 1000");
    for wait in [1, 2, 4] {
        host.tick();
        assert_eq!(state(&host), DeviceState::Running);
        shared.die.store(true, Ordering::Relaxed);
        clock.advance(Duration::from_nanos(1000));
        host.tick();
        assert_eq!(waiting(&host, &clock).0, wait);
        clock.advance(Duration::from_secs(wait));
    }
    host.tick();
    assert_eq!(state(&host), DeviceState::Running);

    // Up for as long as the cap: what went before is forgotten.
    clock.advance(Options::default().retry_cap);
    shared.die.store(true, Ordering::Relaxed);
    host.tick();
    assert_eq!(waiting(&host, &clock).0, 1);
}

#[test]
fn switching_it_on_by_hand_starts_the_wait_again_too() {
    let (host, clock, shared) = host(4, ", period_ns = 1000");
    for wait in [1, 2] {
        host.tick();
        shared.die.store(true, Ordering::Relaxed);
        clock.advance(Duration::from_nanos(1000));
        host.tick();
        assert_eq!(waiting(&host, &clock).0, wait);
        clock.advance(Duration::from_secs(wait));
    }
    host.off("d").unwrap();
    host.on("d").unwrap();
    assert_eq!(state(&host), DeviceState::Running);
    shared.die.store(true, Ordering::Relaxed);
    host.tick();
    assert_eq!(waiting(&host, &clock).0, 1);
}

#[test]
fn asking_for_ever_never_switches_a_device_off_unless_the_board_says_so() {
    let (host, clock, shared) = host(5, ", always = true");
    for _ in 0..40 {
        host.tick();
        let (wait, ..) = waiting(&host, &clock);
        clock.advance(Duration::from_secs(wait));
    }
    assert_eq!(binds(shared), 40, "still being bound");

    let (host, clock, shared) = host_with(6, ", always = true", "auto_off_after = 3\n");
    for _ in 0..10 {
        host.tick();
        clock.advance(Duration::from_secs(8));
    }
    assert!(matches!(state(&host), DeviceState::Off(_)));
    assert_eq!(binds(shared), 3);
}
