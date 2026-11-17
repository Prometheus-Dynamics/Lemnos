//! An off request on a device that is waiting to retry its bind (`Backoff`): the retry is cancelled, the
//! device reads Off, and no bind is attempted while it is off, however long the clock runs. An on
//! then resumes the normal bind and the failure rule.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_core::{Config, Lifecycle, Result, Status, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, OffReason, Options, entry};
use lemnos_sim::SimBoard;

/// The attempt counter is process-wide: one test at a time.
static SERIAL: Mutex<()> = Mutex::new(());
static ATTEMPTS: AtomicU32 = AtomicU32::new(0);

#[contract("example.refuser", version = 1)]
pub trait Refuser {
    #[method(1)]
    fn answer(&mut self) -> Result<u32>;
}

/// A driver whose bind is always refused: it waits to retry, and each retry is one bind attempt.
#[driver("refuser", provides = [Refuser])]
pub struct Refusing {}

impl Lifecycle for Refusing {
    type Needs = RefusingNeeds;

    fn start(_: RefusingNeeds, _: &Config<'_>) -> Result<Self> {
        ATTEMPTS.fetch_add(1, Ordering::AcqRel);
        Err(Status::NO_DEVICE)
    }
}

impl Refuser for Refusing {
    fn answer(&mut self) -> Result<u32> {
        Ok(0)
    }
}

fn host() -> (Host, lemnos_sim::SimClock) {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"r\"\ndriver = \"refuser\"\n";
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<Refusing>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (host, clock)
}

/// A device waiting to retry, switched off, stays off: its retry is gone and nothing binds it, even
/// after the clock passes the 8 s cap several times. On resumes the bind and the failure rule.
#[test]
fn off_cancels_a_pending_retry_and_nothing_binds_it_while_off() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    ATTEMPTS.store(0, Ordering::Release);
    let (host, clock) = host();
    assert!(matches!(
        host.device("r").unwrap().state,
        DeviceState::Backoff { .. }
    ));
    assert!(host.next_retry_ns().is_some(), "waiting to retry");
    assert_eq!(ATTEMPTS.load(Ordering::Acquire), 1);

    host.off("r").unwrap();
    assert_eq!(
        host.device("r").unwrap().state,
        DeviceState::Off(OffReason::Manual)
    );
    assert_eq!(
        host.next_retry_ns(),
        None,
        "the retry is cancelled, not parked"
    );

    for _ in 0..4 {
        clock.advance(Duration::from_secs(10));
        host.tick();
        assert_eq!(
            host.device("r").unwrap().state,
            DeviceState::Off(OffReason::Manual)
        );
    }
    assert_eq!(
        ATTEMPTS.load(Ordering::Acquire),
        1,
        "no bind attempt while off"
    );

    host.on("r").unwrap();
    assert_eq!(
        ATTEMPTS.load(Ordering::Acquire),
        2,
        "on makes one bind attempt"
    );
    assert!(matches!(
        host.device("r").unwrap().state,
        DeviceState::Backoff { .. }
    ));
    assert!(
        host.next_retry_ns().is_some(),
        "and the failure rule schedules the next"
    );
    clock.advance(Duration::from_secs(10));
    host.tick();
    assert_eq!(
        ATTEMPTS.load(Ordering::Acquire),
        3,
        "the failure rule retries under the cap"
    );
    host.shutdown();
}
