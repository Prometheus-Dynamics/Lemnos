//! What an off request means for a driver that holds its hardware (`HOLDS_OFF`), and for one that
//! does not. A kept instance is neither ticked nor served while the device is off; an off that
//! fails, or panics, drops the instance and says so in the device's state.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lemnos_core::{Config, Key, Kind, Lifecycle, MethodId, Result, Status, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, OffReason, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

/// The counters are process-wide, and every test uses them: one test at a time.
static SERIAL: Mutex<()> = Mutex::new(());
static TICKS: AtomicU32 = AtomicU32::new(0);
static SERVED: AtomicU32 = AtomicU32::new(0);
static BINDS: AtomicU32 = AtomicU32::new(0);

#[contract("example.holder", version = 1)]
pub trait Holder {
    /// Counts the request: the device serves it only while it is on.
    #[method(1)]
    fn count(&mut self) -> Result<u32>;
}

const MODE: &[Key] = &[Key::optional("mode", Kind::Str)];

/// A driver that holds its hardware across off. Its `mode` setting says what its `off` does.
#[driver("holder", provides = [Holder])]
pub struct Holding {
    mode: &'static str,
}

impl Lifecycle for Holding {
    type Needs = HoldingNeeds;
    const TICKS: bool = true;
    const HOLDS_OFF: bool = true;

    fn check_config<'a>(
        cfg: &Config<'a>,
    ) -> core::result::Result<(), lemnos_core::ConfigError<'a>> {
        cfg.check_keys(MODE)
    }

    fn start(_: HoldingNeeds, cfg: &Config<'_>) -> Result<Self> {
        BINDS.fetch_add(1, Ordering::AcqRel);
        Ok(Self {
            mode: match cfg.str("mode") {
                Some("error") => "error",
                Some("panic") => "panic",
                _ => "ok",
            },
        })
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        TICKS.fetch_add(1, Ordering::AcqRel);
        Some(now_ns + 1_000_000)
    }

    fn off(&mut self) -> Result<()> {
        match self.mode {
            "error" => Err(Status::IO),
            "panic" => panic!("off panicked"),
            _ => Ok(()),
        }
    }

    fn on(&mut self) -> Result<()> {
        Ok(())
    }
}

impl Holder for Holding {
    fn count(&mut self) -> Result<u32> {
        SERVED.fetch_add(1, Ordering::AcqRel);
        Ok(SERVED.load(Ordering::Acquire))
    }
}

/// A driver with the default off: it is dropped on off, and bound again on on.
#[driver("plain", provides = [Holder])]
pub struct Plain {}

impl Lifecycle for Plain {
    type Needs = PlainNeeds;
    const TICKS: bool = true;

    fn start(_: PlainNeeds, _: &Config<'_>) -> Result<Self> {
        BINDS.fetch_add(1, Ordering::AcqRel);
        Ok(Self {})
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        TICKS.fetch_add(1, Ordering::AcqRel);
        Some(now_ns + 1_000_000)
    }
}

impl Holder for Plain {
    fn count(&mut self) -> Result<u32> {
        SERVED.fetch_add(1, Ordering::AcqRel);
        Ok(SERVED.load(Ordering::Acquire))
    }
}

fn host_with(devices: &str) -> (Host, SimClock) {
    let text =
        format!("format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n{devices}");
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(&text).unwrap(),
        &[entry::<Holding>(), entry::<Plain>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (host, clock)
}

fn reset() {
    TICKS.store(0, Ordering::Release);
    SERVED.store(0, Ordering::Release);
    BINDS.store(0, Ordering::Release);
}

fn count(host: &Host, device: u32) -> lemnos_core::Result<u32> {
    let mut reply = [0; 4];
    host.call(
        device,
        MethodId {
            contract: 0,
            method: 1,
        },
        &[],
        &mut reply,
    )?;
    Ok(u32::from_le_bytes(reply))
}

fn index(host: &Host, id: &str) -> u32 {
    host.device(id).unwrap().index
}

/// A driver with the default off still drops its instance on off and binds a new one on on.
#[test]
fn a_driver_without_off_is_dropped_on_off_and_bound_again_on_on() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    reset();
    let (host, clock) = host_with("[[devices]]\nid = \"p\"\ndriver = \"plain\"\n");
    let p = index(&host, "p");
    assert_eq!(BINDS.load(Ordering::Acquire), 1);

    host.off("p").unwrap();
    assert_eq!(
        host.device("p").unwrap().state,
        DeviceState::Off(OffReason::Manual)
    );
    assert_eq!(
        count(&host, p),
        Err(Status::NO_DEVICE),
        "dropped: nothing serves it"
    );
    let ticks = TICKS.load(Ordering::Acquire);
    clock.advance(Duration::from_secs(10));
    host.tick();
    assert_eq!(
        TICKS.load(Ordering::Acquire),
        ticks,
        "dropped: nothing ticks it"
    );

    host.on("p").unwrap();
    assert_eq!(host.device("p").unwrap().state, DeviceState::Running);
    assert_eq!(
        BINDS.load(Ordering::Acquire),
        2,
        "bound again: the default on is a rebind"
    );
    host.shutdown();
}

/// A kept instance is not ticked or served while off, and `on` switches it back on without a bind.
#[test]
fn a_kept_instance_is_not_ticked_or_served_while_off() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    reset();
    let (host, clock) = host_with("[[devices]]\nid = \"h\"\ndriver = \"holder\"\n");
    let h = index(&host, "h");
    clock.advance(Duration::from_millis(5));
    host.tick();
    assert!(TICKS.load(Ordering::Acquire) > 0, "running: it ticks");
    assert_eq!(count(&host, h), Ok(1), "running: it serves");
    let (ticks, served) = (
        TICKS.load(Ordering::Acquire),
        SERVED.load(Ordering::Acquire),
    );

    host.off("h").unwrap();
    assert_eq!(
        host.device("h").unwrap().state,
        DeviceState::Off(OffReason::Manual)
    );
    assert_eq!(
        count(&host, h),
        Err(Status::NO_DEVICE),
        "off: its requests are refused"
    );
    clock.advance(Duration::from_secs(10));
    host.tick();
    assert_eq!(
        TICKS.load(Ordering::Acquire),
        ticks,
        "off: it is not ticked"
    );
    assert_eq!(
        SERVED.load(Ordering::Acquire),
        served,
        "off: nothing reached the driver"
    );
    assert_eq!(
        BINDS.load(Ordering::Acquire),
        1,
        "kept: the instance was not rebound"
    );

    host.on("h").unwrap();
    assert_eq!(host.device("h").unwrap().state, DeviceState::Running);
    assert_eq!(
        BINDS.load(Ordering::Acquire),
        1,
        "on: switched on in place, no bind"
    );
    clock.advance(Duration::from_millis(5));
    host.tick();
    assert!(TICKS.load(Ordering::Acquire) > ticks, "on: it ticks again");
    assert_eq!(count(&host, h), Ok(served + 1), "on: it serves again");
    host.shutdown();
}

/// An error from off drops the instance and reports it: the device is off, failed with the status.
#[test]
fn an_error_from_off_drops_the_instance_and_reports_it() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    reset();
    let (host, _) =
        host_with("[[devices]]\nid = \"e\"\ndriver = \"holder\"\nconfig = { mode = \"error\" }\n");
    let e = index(&host, "e");
    host.off("e").unwrap();
    assert_eq!(
        host.device("e").unwrap().state,
        DeviceState::Off(OffReason::Failed(Status::IO))
    );
    assert_eq!(count(&host, e), Err(Status::NO_DEVICE), "dropped");
    host.shutdown();
}

/// A panic from off is contained the same way: dropped, and reported as the panic status.
#[test]
fn a_panic_from_off_drops_the_instance_and_reports_it() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    reset();
    let (host, _) =
        host_with("[[devices]]\nid = \"x\"\ndriver = \"holder\"\nconfig = { mode = \"panic\" }\n");
    host.off("x").unwrap();
    assert_eq!(
        host.device("x").unwrap().state,
        DeviceState::Off(OffReason::Failed(Status::PANIC))
    );
    host.shutdown();
}
