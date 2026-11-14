//! A client that describes or calls a device is not refused because the driver happens to be in
//! its tick or in a short call: it waits its turn on the device's lane, within the call deadline.
//! A device whose driver never comes back still gets the typed refusal, on time.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lemnos_core::{
    Config, ConfigError, Key, Kind, Lifecycle, MethodId, Result, Status, contract, driver,
};
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

/// A gate the test holds closed. The driver parks on it when `armed`.
struct Gate {
    armed: AtomicBool,
    entered: Mutex<bool>,
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    const fn new() -> Self {
        Self {
            armed: AtomicBool::new(false),
            entered: Mutex::new(false),
            open: Mutex::new(false),
            changed: Condvar::new(),
        }
    }

    /// Called by the driver: says it is inside, then waits (bounded, so a failing test ends).
    fn park(&self, limit: Duration) {
        *self.entered.lock().unwrap() = true;
        self.changed.notify_all();
        let until = Instant::now() + limit;
        let mut open = self.open.lock().unwrap();
        while !*open {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            open = self.changed.wait_timeout(open, left).unwrap().0;
        }
    }

    fn wait_entered(&self) {
        let until = Instant::now() + Duration::from_secs(10);
        let mut entered = self.entered.lock().unwrap();
        while !*entered {
            let left = until.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "the driver never got inside");
            entered = self.changed.wait_timeout(entered, left).unwrap().0;
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

static GATES: [Gate; 4] = [Gate::new(), Gate::new(), Gate::new(), Gate::new()];

#[contract("example.parked", version = 1)]
pub trait Parked {
    #[method(1)]
    fn value(&mut self) -> Result<u32>;
    /// Parks on the gate, inside the call.
    #[method(2)]
    fn park(&mut self) -> Result<()>;
}

#[driver("parker", provides = [Parked])]
pub struct Parker {
    gate: usize,
}

impl Lifecycle for Parker {
    type Needs = ParkerNeeds;
    const TICKS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[Key::optional("gate", Kind::Int)])
    }

    fn start(_: ParkerNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            gate: cfg.int("gate").unwrap_or(0) as usize,
        })
    }

    fn tick(&mut self, _: u64) -> Option<u64> {
        let gate = &GATES[self.gate];
        if gate.armed.swap(false, Ordering::AcqRel) {
            gate.park(Duration::from_secs(10));
        }
        None
    }
}

impl Parked for Parker {
    fn value(&mut self) -> Result<u32> {
        Ok(7)
    }

    fn park(&mut self) -> Result<()> {
        GATES[self.gate].park(Duration::from_millis(600));
        Ok(())
    }
}

fn host(gate: usize, options: Options) -> (Arc<Host>, SimClock) {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"p\"\ndriver = \"parker\"\nconfig = {{ gate = {gate} }}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(&text).unwrap(),
        &[entry::<Parker>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        options,
    )
    .unwrap();
    (Arc::new(host), clock)
}

fn method(method: u16) -> MethodId {
    MethodId {
        contract: 0,
        method,
    }
}

/// What a client asks of a device: each one's answer, and whether the gate was already open when
/// it came back (a client that came back before then was answered while the driver was inside).
type Answers = Vec<(&'static str, Result<()>, bool)>;

/// Runs the client asks on their own threads while the driver is parked, lets them reach the
/// host, holds the gate a moment, opens it and returns what each got.
fn ask_while_parked(host: &Arc<Host>, gate: &'static Gate) -> Answers {
    let index = host.device("p").unwrap().index;
    let reached = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    for what in ["describe", "value"] {
        let host = Arc::clone(host);
        let reached = Arc::clone(&reached);
        threads.push(std::thread::spawn(move || {
            reached.store(true, Ordering::Release);
            let answer = match what {
                "describe" => host.describe(index).map(drop),
                _ => host.call(index, method(1), &[], &mut [0; 4]).map(drop),
            };
            let open = *gate.open.lock().unwrap();
            (what, answer, open)
        }));
    }
    while !reached.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    std::thread::sleep(Duration::from_millis(100));
    gate.release();
    threads.into_iter().map(|t| t.join().unwrap()).collect()
}

fn assert_waited(answers: &Answers) {
    for (what, answer, gate_was_open) in answers {
        assert_eq!(
            *answer,
            Ok(()),
            "{what} while the driver was inside must wait, not be refused"
        );
        assert!(
            *gate_was_open,
            "{what} came back while the driver was inside"
        );
    }
}

#[test]
fn describe_and_a_value_call_wait_for_a_tick_in_progress() {
    let (host, _) = host(0, Options::default());
    let gate = &GATES[0];
    gate.armed.store(true, Ordering::Release);
    let ticker = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || host.run_ticks())
    };
    gate.wait_entered();
    let answers = ask_while_parked(&host, gate);
    ticker.join().unwrap();
    assert_waited(&answers);
}

#[test]
fn describe_and_a_value_call_wait_for_a_short_client_call_in_progress() {
    let (host, _) = host(1, Options::default());
    let gate = &GATES[1];
    let index = host.device("p").unwrap().index;
    let caller = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || host.call(index, method(2), &[], &mut []))
    };
    gate.wait_entered();
    let answers = ask_while_parked(&host, gate);
    assert_eq!(caller.join().unwrap(), Ok(0));
    assert_waited(&answers);
}

#[test]
fn a_driver_that_never_comes_back_gives_the_typed_refusal_on_time() {
    let options = Options {
        call_deadline: Duration::from_millis(100),
        ..Options::default()
    };
    let (host, _) = host(2, options);
    let gate = &GATES[2];
    let index = host.device("p").unwrap().index;
    let caller = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || host.call(index, method(2), &[], &mut []))
    };
    gate.wait_entered();
    // Inside the hung call: the describe waits for its turn, and is refused when its own bound is
    // up. Never BUSY, never for ever.
    let started = Instant::now();
    let answer = host.describe(index);
    assert!(
        matches!(answer, Err(s) if s == Status::TIMED_OUT || s == Status::NO_DEVICE),
        "{answer:?}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(caller.join().unwrap(), Err(Status::TIMED_OUT));
    // Once the host has contained it, the device is not described by a driver that is gone.
    assert_eq!(host.describe(index).err(), Some(Status::NO_DEVICE));
    gate.release();
}
