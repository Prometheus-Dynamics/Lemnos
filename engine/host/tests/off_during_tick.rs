//! An off request that arrives while the device's tick is running. The tick holds the device's
//! instance for as long as it runs, so the off cannot take the instance at that moment. The off must
//! still switch the device off: once the tick lets go, the driver is dropped (what releases its
//! hardware, a gadget's bound function included). Fails on the host code that gave up on the
//! instance when its lock was taken: `off` returned, the state said off, and the instance (and the
//! hardware it held) stayed up, which is how a switch-off was lost under load.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lemnos_core::{Config, Lifecycle, Result, driver};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, OffReason, Options, entry};
use lemnos_sim::SimBoard;

/// The next tick of the device, when `HOLD` is set, says it has started and waits for `RELEASE`.
static HOLD: AtomicBool = AtomicBool::new(false);
static ENTERED: Mutex<Option<Sender<()>>> = Mutex::new(None);
static RELEASE: Mutex<Option<Receiver<()>>> = Mutex::new(None);
/// How many instances of the driver were dropped.
static DROPS: AtomicU32 = AtomicU32::new(0);

const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
[[devices]]\nid = \"gate\"\ndriver = \"gate\"\n";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A driver with the default off, so an off drops its instance. Its tick can be held open.
#[driver("gate", provides = [])]
pub struct Gate {}

impl Lifecycle for Gate {
    type Needs = GateNeeds;
    const TICKS: bool = true;

    fn start(_: GateNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {})
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if HOLD.swap(false, Ordering::AcqRel) {
            if let Some(entered) = lock(&ENTERED).as_ref() {
                let _ = entered.send(());
            }
            if let Some(release) = lock(&RELEASE).as_ref() {
                let _ = release.recv();
            }
        }
        Some(now_ns + 1_000_000)
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::AcqRel);
    }
}

#[test]
fn an_off_during_a_tick_still_drops_the_driver_once_the_tick_lets_go() {
    let (entered_tx, entered_rx) = channel();
    let (release_tx, release_rx) = channel();
    *lock(&ENTERED) = Some(entered_tx);
    *lock(&RELEASE) = Some(release_rx);

    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(BOARD).unwrap(),
        &[entry::<Gate>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        // A tick held here is longer than the default call deadline, which would mark the device
        // hung. The deadline is set well past the hold, so the device is not hung.
        Options {
            call_deadline: Duration::from_secs(60),
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(DROPS.load(Ordering::Acquire), 0);

    HOLD.store(true, Ordering::Release);
    clock.advance(Duration::from_millis(10));
    std::thread::scope(|s| {
        let ticking = s.spawn(|| host.tick());
        entered_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the device's tick started");

        // The off arrives while the tick holds the instance. Give it its chance to finish first,
        // as a daemon's request would: a switch-off that is lost returns at once.
        let off = s.spawn(|| host.off("gate").unwrap());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !off.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }

        release_tx.send(()).unwrap();
        ticking.join().unwrap();
        off.join().unwrap();
    });

    assert_eq!(
        host.device("gate").unwrap().state,
        DeviceState::Off(OffReason::Manual)
    );
    assert_eq!(
        DROPS.load(Ordering::Acquire),
        1,
        "the off dropped the driver: the tick that held it let go, and the off did not give up"
    );
    host.shutdown();
}
