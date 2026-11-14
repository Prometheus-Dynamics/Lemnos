//! A power-on is one step of a device's state revision when its bind works the first time, and two
//! when the bind misses once: a `retrying`, then a `running`. Each is a change the host counts, with
//! no bridge in the path. The bridge counts what it observes, so a step the host makes is not
//! always a step the bridge publishes; that is checked where the bridge is in the path.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use lemnos_core::{Config, Lifecycle, Result, Status, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

/// One set per test, picked by the device's `key`, since tests share the process.
struct Shared {
    binds: AtomicU32,
    /// The next bind fails, once.
    miss: AtomicBool,
}

const KEYS: usize = 2;
static SHARED: [Shared; KEYS] = [const {
    Shared {
        binds: AtomicU32::new(0),
        miss: AtomicBool::new(false),
    }
}; KEYS];

#[contract("stepper", version = 1)]
pub trait Stepping {
    /// How many times the driver has been bound.
    #[method(1)]
    fn binds(&mut self) -> Result<u32>;
}

#[driver("stepper", provides = [Stepping])]
pub struct Stepper {
    shared: &'static Shared,
}

impl Lifecycle for Stepper {
    type Needs = StepperNeeds;
    const TICKS: bool = true;

    fn start(_: StepperNeeds, cfg: &Config<'_>) -> Result<Self> {
        let shared = &SHARED[cfg.int("key").ok_or(Status::INVALID_ARGUMENT)? as usize];
        shared.binds.fetch_add(1, Ordering::Relaxed);
        if shared.miss.swap(false, Ordering::Relaxed) {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self { shared })
    }

    fn tick(&mut self, _now_ns: u64) -> Option<u64> {
        None
    }
}

impl Stepping for Stepper {
    fn binds(&mut self) -> Result<u32> {
        Ok(self.shared.binds.load(Ordering::Relaxed))
    }
}

fn host(key: usize) -> (Host, SimClock) {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"d\"\ndriver = \"stepper\"\nconfig = {{ key = {key} }}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(&text).unwrap(),
        &[entry::<Stepper>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (host, clock)
}

fn revision(host: &Host) -> u64 {
    host.device("d").unwrap().revision
}

fn state(host: &Host) -> DeviceState {
    host.device("d").unwrap().state
}

fn binds(shared: &Shared) -> u32 {
    shared.binds.load(Ordering::Relaxed)
}

/// A power-on whose bind works the first time is exactly one step, and switching off is one.
#[test]
fn a_power_on_whose_bind_works_is_one_step() {
    let (host, _clock) = host(0);
    let shared = &SHARED[0];
    assert_eq!(state(&host), DeviceState::Running);
    let running = revision(&host);

    host.off("d").unwrap();
    assert_eq!(revision(&host), running + 1, "switching off is one step");

    let off = revision(&host);
    let bound = binds(shared);
    host.on("d").unwrap();
    assert_eq!(state(&host), DeviceState::Running);
    assert_eq!(binds(shared), bound + 1, "one bind for the power-on");
    assert_eq!(revision(&host), off + 1, "one power-on is one step");
}

/// A bind that misses once is two steps: the miss is a change (`retrying`), and the retry that
/// works is another (`running`). The host announces both, so the count of steps is the count of
/// changes, and a bridge that observes both moves its generation by two.
#[test]
fn a_power_on_whose_bind_misses_once_is_two_steps() {
    let (host, clock) = host(1);
    let shared = &SHARED[1];
    host.off("d").unwrap();
    let off = revision(&host);

    shared.miss.store(true, Ordering::Relaxed);
    host.on("d").unwrap();
    assert!(
        matches!(state(&host), DeviceState::Backoff { .. }),
        "the missed bind waits: {:?}",
        state(&host)
    );
    assert_eq!(revision(&host), off + 1, "the miss is one step: retrying");

    clock.advance(Duration::from_secs(1));
    host.tick();
    assert_eq!(state(&host), DeviceState::Running);
    assert_eq!(
        revision(&host),
        off + 2,
        "and the retry that works is one more: running"
    );
}
