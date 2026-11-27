//! A call, tick or the host's own work that runs past its deadline leaves one line, and the line
//! names what ran: the method and its caller, the tick, or the work and whose it was. Work that
//! comes for a device after it went hung is refused at once, so an abandoned instance leaves one
//! line however many callers come and go while it is stuck.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_core::{CallerId, Config, Lifecycle, MethodId, Result, Status, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

#[contract("sleeper", version = 1)]
pub trait Sleeping {
    /// Sleeps `ms` milliseconds on the call.
    #[method(1)]
    fn nap(&mut self, ms: u32) -> Result<()>;
    /// The tick `after` ticks from now (0: the next) sleeps `ms` milliseconds.
    #[method(2)]
    fn nap_in_tick(&mut self, ms: u32, after: u32) -> Result<()>;
}

#[driver("sleeper", provides = [Sleeping])]
pub struct Sleeper {
    tick_nap: u32,
    skip: u32,
}

impl Lifecycle for Sleeper {
    type Needs = SleeperNeeds;
    const TRACKS_CALLERS: bool = true;
    const TICKS: bool = true;

    fn start(_: SleeperNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {
            tick_nap: 0,
            skip: 0,
        })
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if self.skip > 0 {
            self.skip -= 1;
            return Some(now_ns + 1_000_000);
        }
        let nap = core::mem::take(&mut self.tick_nap);
        if nap > 0 {
            std::thread::sleep(Duration::from_millis(u64::from(nap)));
        }
        Some(now_ns + 1_000_000)
    }

    fn caller_gone(&mut self, _: CallerId) {}
}

impl Sleeping for Sleeper {
    fn nap(&mut self, ms: u32) -> Result<()> {
        std::thread::sleep(Duration::from_millis(u64::from(ms)));
        Ok(())
    }

    fn nap_in_tick(&mut self, ms: u32, after: u32) -> Result<()> {
        self.tick_nap = ms;
        self.skip = after;
        Ok(())
    }
}

static WARNINGS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn record_warning(text: &str) {
    WARNINGS.lock().unwrap().push(text.to_string());
}

/// The overrun lines about device `id`.
fn overruns(id: &str) -> Vec<String> {
    let start = format!("device {id} (driver sleeper): ");
    WARNINGS
        .lock()
        .unwrap()
        .iter()
        .filter(|w| w.starts_with(&start) && w.contains("ran past its"))
        .cloned()
        .collect()
}

fn host(id: &str) -> (Host, SimClock, u32) {
    lemnos_host::set_warning_sink(record_warning);
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"{id}\"\ndriver = \"sleeper\"\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(&text).unwrap(),
        &[entry::<Sleeper>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options {
            call_deadline: Duration::from_millis(500),
            ..Options::default()
        },
    )
    .unwrap();
    let index = host.device(id).unwrap().index;
    (host, clock, index)
}

const NAP: MethodId = MethodId {
    contract: 0,
    method: 1,
};
const NAP_IN_TICK: MethodId = MethodId {
    contract: 0,
    method: 2,
};

/// Waits out the abandoned work, then lets the host rebind the device.
fn comes_back(host: &Host, clock: &SimClock, id: &str) {
    let until = Instant::now() + Duration::from_secs(10);
    while host.device(id).unwrap().state != DeviceState::Running {
        assert!(Instant::now() < until, "device {id} never came back");
        std::thread::sleep(Duration::from_millis(20));
        clock.advance(Duration::from_secs(8));
        host.tick();
    }
}

#[test]
fn the_line_names_the_method_and_the_caller_by_the_name_it_gave() {
    let (host, clock, a) = host("named");
    host.name_caller(CallerId(7), "bench-client");
    assert_eq!(
        host.call_as(CallerId(7), a, NAP, &1500u32.to_le_bytes(), &mut []),
        Err(Status::TIMED_OUT)
    );
    assert_eq!(
        overruns("named"),
        [
            "device named (driver sleeper): the call sleeper.nap from caller 7 (bench-client) ran past its 500 ms deadline; its driver is abandoned until the call returns; job(s) none end interrupted"
        ]
    );
    comes_back(&host, &clock, "named");

    // A caller that gave no name is its number; the daemon's own calls are the daemon's.
    assert_eq!(
        host.call_as(CallerId(8), a, NAP, &1500u32.to_le_bytes(), &mut []),
        Err(Status::TIMED_OUT)
    );
    comes_back(&host, &clock, "named");
    assert_eq!(
        host.call(a, NAP, &1500u32.to_le_bytes(), &mut []),
        Err(Status::TIMED_OUT)
    );
    let lines = overruns("named");
    assert!(
        lines[1].contains(": the call sleeper.nap from caller 8 ran past"),
        "{lines:?}"
    );
    assert!(
        lines[2].contains(": the call sleeper.nap from the daemon ran past"),
        "{lines:?}"
    );
    assert_eq!(lines.len(), 3, "{lines:?}");
}

#[test]
fn a_name_goes_with_its_caller() {
    let (host, clock, a) = host("gone");
    host.name_caller(CallerId(5), "first");
    host.name_caller(CallerId(5), "again");
    assert_eq!(
        host.call_as(CallerId(5), a, NAP, &1500u32.to_le_bytes(), &mut []),
        Err(Status::TIMED_OUT)
    );
    assert!(overruns("gone")[0].contains("from caller 5 (again) ran past"));
    comes_back(&host, &clock, "gone");
    host.caller_gone(CallerId(5));
    assert_eq!(
        host.call_as(CallerId(5), a, NAP, &1500u32.to_le_bytes(), &mut []),
        Err(Status::TIMED_OUT)
    );
    assert!(overruns("gone")[1].contains("from caller 5 ran past"));
}

fn nap_in_tick(ms: u32, after: u32) -> [u8; 8] {
    let mut args = [0; 8];
    args[..4].copy_from_slice(&ms.to_le_bytes());
    args[4..].copy_from_slice(&after.to_le_bytes());
    args
}

#[test]
fn a_tick_that_runs_over_is_named_as_the_tick() {
    let (host, clock, a) = host("ticking");
    // The call's own tick after it is skipped; the host's next tick naps.
    host.call(a, NAP_IN_TICK, &nap_in_tick(1500, 1), &mut [])
        .unwrap();
    clock.advance(Duration::from_millis(5));
    host.tick();
    assert_eq!(
        overruns("ticking"),
        [
            "device ticking (driver sleeper): its tick ran past its 500 ms deadline; its driver is abandoned until the call returns; job(s) none end interrupted"
        ]
    );
    comes_back(&host, &clock, "ticking");
}

/// A call runs the driver's tick after its method when the tick is due; a tick that runs over
/// there is the call's deadline, and the line says it was in the tick.
#[test]
fn a_tick_that_runs_over_after_a_call_is_named_as_the_calls_tick() {
    let (host, clock, a) = host("after");
    host.name_caller(CallerId(6), "setter");
    assert_eq!(
        host.call_as(CallerId(6), a, NAP_IN_TICK, &nap_in_tick(1500, 0), &mut []),
        Err(Status::TIMED_OUT)
    );
    assert_eq!(
        overruns("after"),
        [
            "device after (driver sleeper): the call sleeper.nap_in_tick from caller 6 (setter), in the tick after it, ran past its 500 ms deadline; its driver is abandoned until the call returns; job(s) none end interrupted"
        ]
    );
    comes_back(&host, &clock, "after");
    // The method itself running over is not said to be in the tick.
    assert_eq!(
        host.call_as(CallerId(6), a, NAP, &1500u32.to_le_bytes(), &mut []),
        Err(Status::TIMED_OUT)
    );
    assert!(overruns("after")[1].contains("from caller 6 (setter) ran past"));
}

/// The run that left two lines 3.4 s apart on one abandoned instance: a call is stuck, and a
/// caller that goes meanwhile sends `caller_gone` on a new lane, which waited on the instance the
/// stuck call holds and ran out a deadline of its own. Now it is not sent to a hung device (the
/// rebind starts an instance with nothing to tell), the endpoints' release and the description are
/// `NO_DEVICE`, and none of them waits.
#[test]
fn work_for_a_hung_device_is_refused_at_once_and_leaves_no_second_line() {
    let (host, clock, a) = host("stuck");
    host.name_caller(CallerId(3), "holder");
    assert_eq!(
        host.call_as(CallerId(3), a, NAP, &1500u32.to_le_bytes(), &mut []),
        Err(Status::TIMED_OUT)
    );
    let started = Instant::now();
    host.caller_gone(CallerId(4));
    host.caller_gone(CallerId(3));
    assert_eq!(
        host.usb_endpoints_release(CallerId(3), a),
        Err(Status::NO_DEVICE)
    );
    assert_eq!(host.describe(a).err(), Some(Status::NO_DEVICE));
    assert_eq!(
        host.call_as(CallerId(4), a, NAP, &0u32.to_le_bytes(), &mut []),
        Err(Status::NO_DEVICE)
    );
    let waited = started.elapsed();
    assert!(
        waited < Duration::from_millis(400),
        "the work waited {waited:?} on the abandoned call"
    );
    comes_back(&host, &clock, "stuck");
    assert_eq!(
        overruns("stuck"),
        [
            "device stuck (driver sleeper): the call sleeper.nap from caller 3 (holder) ran past its 500 ms deadline; its driver is abandoned until the call returns; job(s) none end interrupted"
        ]
    );
    // The new instance takes calls, and a caller that goes reaches it.
    host.call_as(CallerId(4), a, NAP, &0u32.to_le_bytes(), &mut [])
        .unwrap();
    host.caller_gone(CallerId(4));
    assert_eq!(overruns("stuck").len(), 1);
}
