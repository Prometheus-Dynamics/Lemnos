//! A driver that declares a `HostClock` need reads the backend's clock: the simulated one under sim,
//! so a test moves it by hand. A backend with no clock refuses the bind, and says which need it was.

mod test_options;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lemnos_core::{
    Config, HostClock, HostClockHandle, Lifecycle, LocalCaller, MethodId, Provider, Result, Status,
    Tag, contract, driver,
};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, entry};
use lemnos_sim::{SimBoard, SimClock};

#[contract("clock-read", version = 1)]
pub trait ClockRead {
    #[method(1)]
    fn read_ns(&mut self) -> Result<u64>;
}

/// Declares the clock as a need and reads it on each call, and nothing else.
#[driver("clock-probe", provides = [ClockRead])]
pub struct ClockProbe {
    #[needs]
    clock: HostClockHandle,
}

impl Lifecycle for ClockProbe {
    type Needs = ClockProbeNeeds;

    fn start(needs: ClockProbeNeeds, _cfg: &Config<'_>) -> Result<Self> {
        Ok(Self { clock: needs.clock })
    }
}

impl ClockRead for ClockProbe {
    fn read_ns(&mut self) -> Result<u64> {
        Ok(self.clock.now_ns())
    }
}

/// The same probe, with the clock optional: it binds without one.
#[driver("clock-probe-optional", provides = [ClockRead])]
pub struct OptionalProbe {
    #[needs(optional)]
    clock: Option<HostClockHandle>,
}

impl Lifecycle for OptionalProbe {
    type Needs = OptionalProbeNeeds;

    fn start(needs: OptionalProbeNeeds, _cfg: &Config<'_>) -> Result<Self> {
        Ok(Self { clock: needs.clock })
    }
}

impl ClockRead for OptionalProbe {
    fn read_ns(&mut self) -> Result<u64> {
        Ok(self.clock.as_ref().map_or(u64::MAX, |clock| clock.now_ns()))
    }
}

/// A backend that hands out a board's devices exactly as the sim does, and no clock.
struct NoClock(SimBoard);

impl Provider for NoClock {
    fn provided(&self, need: &str) -> bool {
        self.0.provided(need)
    }

    fn device(&mut self, need: &str, tag: Tag) -> Result<LocalCaller> {
        self.0.device(need, tag)
    }
}

/// A clock that always reads the same time, as a firmware's static clock would.
struct Fixed;

impl HostClock for Fixed {
    fn now_ns(&self) -> u64 {
        42
    }
}

static FIXED: Fixed = Fixed;

/// The sim's devices with a clock of its own, apart from the one the host times the device with,
/// and a count of how often the host asked for it.
struct OwnClock {
    sim: SimBoard,
    asked: Arc<AtomicUsize>,
}

impl Provider for OwnClock {
    fn provided(&self, need: &str) -> bool {
        self.sim.provided(need)
    }

    fn device(&mut self, need: &str, tag: Tag) -> Result<LocalCaller> {
        self.sim.device(need, tag)
    }

    fn host_clock(&mut self, _need: &str) -> Result<HostClockHandle> {
        self.asked.fetch_add(1, Ordering::Relaxed);
        Ok(HostClockHandle::shared(&FIXED))
    }
}

#[contract("clock-free", version = 1)]
pub trait ClockFree {
    #[method(1)]
    fn value(&mut self) -> Result<u32>;
}

/// A driver that declares no clock.
#[driver("clock-free", provides = [ClockFree])]
pub struct ClockFreeDriver {}

impl Lifecycle for ClockFreeDriver {
    type Needs = ClockFreeDriverNeeds;

    fn start(_needs: ClockFreeDriverNeeds, _cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {})
    }
}

impl ClockFree for ClockFreeDriver {
    fn value(&mut self) -> Result<u32> {
        Ok(7)
    }
}

const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"clock\"\n\
    [[devices]]\nid = \"probe\"\ndriver = \"clock-probe\"\n\
    [[devices]]\nid = \"optional\"\ndriver = \"clock-probe-optional\"\n";

fn read(host: &Host, id: &str) -> u64 {
    let index = host.device(id).unwrap().index;
    let mut reply = [0; 8];
    host.call(
        index,
        MethodId {
            contract: 0,
            method: 1,
        },
        &[],
        &mut reply,
    )
    .unwrap();
    u64::from_le_bytes(reply)
}

fn sim_host() -> (Host, SimClock) {
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(BOARD).unwrap(),
        &[entry::<ClockProbe>(), entry::<OptionalProbe>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        test_options::options(),
    )
    .unwrap();
    (host, clock)
}

#[test]
fn a_driver_that_declares_the_clock_reads_the_sim_clock() {
    let (host, clock) = sim_host();
    clock.advance(Duration::from_millis(1_500));
    assert_eq!(read(&host, "probe"), clock.now_ns());
    assert_eq!(read(&host, "probe"), 1_500_000_000);
    clock.advance(Duration::from_millis(250));
    assert_eq!(read(&host, "probe"), 1_750_000_000);
    assert_eq!(read(&host, "optional"), 1_750_000_000);
}

#[test]
fn a_backend_without_a_clock_refuses_the_bind_by_name() {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"clock\"\n\
        [[devices]]\nid = \"probe\"\ndriver = \"clock-probe\"\n";
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<ClockProbe>()],
        Box::new(NoClock(SimBoard::new())),
        Arc::new(SimBoard::new().clock()),
        test_options::options(),
    )
    .unwrap();
    let state = host.device("probe").unwrap().state;
    let DeviceState::Backoff { reason, .. } = state else {
        panic!("the bind should be refused, not {state:?}");
    };
    assert!(
        reason.contains("no host clock") && reason.contains("\"clock\""),
        "the refusal should name the clock need: {reason}"
    );
    let index = host.device("probe").unwrap().index;
    let mut reply = [0; 8];
    let status = host
        .call(
            index,
            MethodId {
                contract: 0,
                method: 1,
            },
            &[],
            &mut reply,
        )
        .unwrap_err();
    assert_eq!(status, Status::NO_DEVICE);
}

#[test]
fn an_optional_clock_is_absent_when_the_backend_has_none() {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"clock\"\n\
        [[devices]]\nid = \"optional\"\ndriver = \"clock-probe-optional\"\n";
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<OptionalProbe>()],
        Box::new(NoClock(SimBoard::new())),
        Arc::new(SimBoard::new().clock()),
        test_options::options(),
    )
    .unwrap();
    assert_eq!(read(&host, "optional"), u64::MAX);
}

#[test]
fn the_clock_is_the_backends_not_the_one_the_host_ticks_on() {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"clock\"\n\
        [[devices]]\nid = \"probe\"\ndriver = \"clock-probe\"\n";
    let host_clock = SimBoard::new().clock();
    host_clock.advance(Duration::from_secs(9));
    let asked = Arc::new(AtomicUsize::new(0));
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<ClockProbe>()],
        Box::new(OwnClock {
            sim: SimBoard::new(),
            asked: asked.clone(),
        }),
        Arc::new(host_clock),
        test_options::options(),
    )
    .unwrap();
    assert_eq!(read(&host, "probe"), 42);
    assert!(asked.load(Ordering::Relaxed) > 0);
}

#[test]
fn a_driver_that_declares_no_clock_is_never_given_one() {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"clock\"\n\
        [[devices]]\nid = \"free\"\ndriver = \"clock-free\"\n";
    let asked = Arc::new(AtomicUsize::new(0));
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<ClockFreeDriver>()],
        Box::new(OwnClock {
            sim: SimBoard::new(),
            asked: asked.clone(),
        }),
        Arc::new(SimBoard::new().clock()),
        test_options::options(),
    )
    .unwrap();
    assert!(matches!(
        host.device("free").unwrap().state,
        DeviceState::Running
    ));
    let index = host.device("free").unwrap().index;
    let mut reply = [0; 4];
    host.call(
        index,
        MethodId {
            contract: 0,
            method: 1,
        },
        &[],
        &mut reply,
    )
    .unwrap();
    assert_eq!(u32::from_le_bytes(reply), 7);
    assert_eq!(
        asked.load(Ordering::Relaxed),
        0,
        "the host asked the backend for a clock"
    );
}
