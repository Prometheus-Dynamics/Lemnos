//! A keep-alive device is ticked only while the daemon's liveness says it is healthy. Its ticks
//! are not held up by other drivers: a driver that panics or hangs leaves it fed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use lemnos_core::{Config, Lifecycle, MethodId, Result, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{Host, Liveness, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

#[contract("probe", version = 1)]
pub trait Probe {
    /// Ticks so far.
    #[method(1)]
    fn ticks(&mut self) -> Result<u32>;
}

/// Ticks once a millisecond. `panic` makes every tick panic; `hang_ms` makes each tick sleep.
#[driver("probe", provides = [Probe])]
pub struct Ticks {
    count: Arc<AtomicU32>,
    panic: bool,
    hang: Duration,
}

impl Lifecycle for Ticks {
    type Needs = TicksNeeds;
    const TICKS: bool = true;

    fn start(_: TicksNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            count: Arc::new(AtomicU32::new(0)),
            panic: cfg.bool("panic").unwrap_or(false),
            hang: Duration::from_millis(cfg.int("hang_ms").unwrap_or(0) as u64),
        })
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        assert!(!self.panic, "a driver that panics on every tick");
        if !self.hang.is_zero() {
            std::thread::sleep(self.hang);
        }
        self.count.fetch_add(1, Ordering::SeqCst);
        Some(now_ns + 1_000_000)
    }
}

impl Probe for Ticks {
    fn ticks(&mut self) -> Result<u32> {
        Ok(self.count.load(Ordering::SeqCst))
    }
}

/// A liveness the test sets.
struct Gate(Arc<AtomicBool>);

impl Liveness for Gate {
    fn alive(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

struct Rig {
    host: Host,
    clock: SimClock,
    alive: Arc<AtomicBool>,
}

/// A board with a keep-alive `dog`, and `others` more ticking drivers (`hang_ms` for each).
fn rig(others: &str, set_gate: bool, options: Options) -> Rig {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"keepalive\"\n\
         [[devices]]\nid = \"dog\"\ndriver = \"probe\"\nkeepalive = true\n{others}"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(&text).unwrap(),
        &[entry::<Ticks>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        options,
    )
    .unwrap();
    let alive = Arc::new(AtomicBool::new(true));
    if set_gate {
        host.set_liveness(Arc::new(Gate(Arc::clone(&alive))));
    }
    Rig { host, clock, alive }
}

impl Rig {
    fn ticks(&self, id: &str) -> u32 {
        let index = self.host.device(id).unwrap().index;
        let mut reply = [0u8; 4];
        let method = MethodId {
            contract: 0,
            method: 1,
        };
        self.host.call(index, method, &[], &mut reply).unwrap();
        u32::from_le_bytes(reply)
    }

    /// One millisecond on the simulated clock, then the host's due ticks.
    fn step(&self, ms: u64) {
        for _ in 0..ms {
            self.clock.advance(Duration::from_millis(1));
            self.host.run_ticks();
        }
    }
}

#[test]
fn a_keepalive_device_is_ticked_while_the_daemon_is_alive() {
    let rig = rig("", true, Options::default());
    rig.step(100);
    assert!(
        rig.ticks("dog") >= 90,
        "ticked each millisecond: {}",
        rig.ticks("dog")
    );
}

#[test]
fn a_keepalive_device_is_not_ticked_when_the_daemon_is_not_healthy_and_resumes_when_it_is() {
    let rig = rig("", true, Options::default());
    rig.step(50);
    assert!(rig.ticks("dog") > 0);

    rig.alive.store(false, Ordering::SeqCst);
    let before = rig.ticks("dog");
    rig.step(500);
    assert_eq!(
        rig.ticks("dog"),
        before,
        "no tick while the daemon is not healthy"
    );

    rig.alive.store(true, Ordering::SeqCst);
    rig.step(50);
    assert!(rig.ticks("dog") > before, "ticks again once it is healthy");
}

#[test]
fn a_keepalive_device_is_not_ticked_without_a_liveness_at_all() {
    let rig = rig("", false, Options::default());
    rig.step(200);
    assert_eq!(
        rig.ticks("dog"),
        0,
        "the gate is closed until the daemon says it is alive"
    );
}

#[test]
fn an_ordinary_device_is_ticked_whatever_the_liveness_says() {
    let others = "[[devices]]\nid = \"plain\"\ndriver = \"probe\"\n";
    let rig = rig(others, true, Options::default());
    rig.alive.store(false, Ordering::SeqCst);
    rig.step(100);
    assert!(rig.ticks("plain") >= 90);
    assert_eq!(rig.ticks("dog"), 0);
}

#[test]
fn a_driver_that_panics_on_every_tick_does_not_stop_the_keepalive_device() {
    let others = "[[devices]]\nid = \"bad\"\ndriver = \"probe\"\nconfig = { panic = true }\n";
    let rig = rig(others, true, Options::default());
    rig.step(200);
    assert!(
        rig.ticks("dog") >= 190,
        "fed through the other driver's panics: {}",
        rig.ticks("dog")
    );
}

#[test]
fn a_driver_that_hangs_does_not_stop_the_keepalive_device() {
    let options = Options {
        call_deadline: Duration::from_millis(20),
        ..Options::default()
    };
    let others = "[[devices]]\nid = \"stuck\"\ndriver = \"probe\"\nconfig = { hang_ms = 400 }\n";
    let rig = rig(others, true, options);
    // The stuck driver's first tick takes 400 ms, past the call deadline. The keep-alive device
    // still gets its ticks in that time.
    let start = rig.ticks("dog");
    std::thread::sleep(Duration::from_millis(150));
    rig.step(150);
    assert!(
        rig.ticks("dog") > start + 50,
        "fed while another driver hangs"
    );
}
