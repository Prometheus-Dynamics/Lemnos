//! The controller on a host, with simulated devices: a temperature the test sets (and can make
//! fail), and a fan that records every write and every release (and can make writes fail).
//! Time is the simulated clock; each second the test advances it and ticks the host.

use std::sync::Arc;
use std::time::Duration;

use lemnos_contract_fan::Fan;
use lemnos_contract_fan_control::{POINT_BYTES, Point, State};
use lemnos_contract_temperature::Temperature;
use lemnos_core::{
    Config, ConfigError, Decode, Encode, Health, Key, Kind, Lifecycle, MethodId, Reader, Result,
    Status, Writer, contract, driver,
};
use lemnos_driver_fan_control::FanController;
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

/// The test's knob on a simulated temperature. Its `set` takes millidegrees.
#[contract("sim-temperature-knob", version = 1)]
pub trait TemperatureKnob {
    #[method(1)]
    fn set(&mut self, milli: i32) -> Result<()>;
    /// Nonzero: reads fail.
    #[method(2)]
    fn fail(&mut self, on: u32) -> Result<()>;
}

#[driver("sim-temperature", provides = [Temperature, TemperatureKnob])]
pub struct SimTemperature {
    milli: i32,
    failing: bool,
}

impl Lifecycle for SimTemperature {
    type Needs = SimTemperatureNeeds;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[Key::optional("milli", Kind::Int)])
    }

    fn start(_: SimTemperatureNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            milli: cfg.int("milli").unwrap_or(25_000) as i32,
            failing: false,
        })
    }
}

impl Temperature for SimTemperature {
    fn read(&mut self) -> Result<f32> {
        if self.failing {
            Err(Status::IO)
        } else {
            Ok(self.milli as f32 / 1000.0)
        }
    }
}

impl TemperatureKnob for SimTemperature {
    fn set(&mut self, milli: i32) -> Result<()> {
        self.milli = milli;
        Ok(())
    }

    fn fail(&mut self, on: u32) -> Result<()> {
        self.failing = on != 0;
        Ok(())
    }
}

/// The test's knob on a simulated fan: what it was asked for, and how it can fail. It fails the way the board's
/// hwmon fan does: a refused write refuses the release and the hand-back too, its drop is refused only when the
/// test says so, and the duty reads fail only when the test says so. A host that has the fan bound out answers
/// every call to it, reads included, with `NO_DEVICE`, and the drop runs when it binds the fan out.
#[contract("sim-fan-knob", version = 1)]
pub trait FanKnob {
    /// Writes asked for so far, failed ones included.
    #[method(1)]
    fn calls(&mut self) -> Result<u32>;
    /// The duty the fan is at.
    #[method(2)]
    fn last(&mut self) -> Result<u32>;
    /// Releases (the safe state, full duty) asked for so far, failed ones included.
    #[method(3)]
    fn releases(&mut self) -> Result<u32>;
    /// The next `n` writes fail with `IO`.
    #[method(4)]
    fn fail_next(&mut self, n: u32) -> Result<()>;
    /// Hand-backs asked for so far.
    #[method(5)]
    fn hand_backs(&mut self) -> Result<u32>;
    /// While nonzero, writes, releases and hand-backs are refused with `IO`, as the kernel refuses them.
    #[method(6)]
    fn refuse(&mut self, on: u32) -> Result<()>;
    /// While nonzero, the drop's full-duty write is refused too.
    #[method(7)]
    fn drop_refused(&mut self, on: u32) -> Result<()>;
    /// While nonzero, duty reads fail with `IO`.
    #[method(8)]
    fn unreadable(&mut self, on: u32) -> Result<()>;
    /// 1: the host binds the fan out. Every call answers `NO_DEVICE`, and the drop runs (full duty, unless the drop
    /// is refused). 0: the host binds it again.
    #[method(9)]
    fn bound_out(&mut self, on: u32) -> Result<()>;
    /// Duty reads asked for so far, failed ones included.
    #[method(10)]
    fn reads(&mut self) -> Result<u32>;
}

#[driver("sim-fan", provides = [Fan, FanKnob])]
pub struct SimFan {
    calls: u32,
    last: u16,
    releases: u32,
    hand_backs: u32,
    reads: u32,
    failing: u32,
    refusing: bool,
    drop_refused: bool,
    unreadable: bool,
    bound_out: bool,
}

impl Lifecycle for SimFan {
    type Needs = SimFanNeeds;

    fn start(_: SimFanNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {
            calls: 0,
            last: 0,
            releases: 0,
            hand_backs: 0,
            reads: 0,
            failing: 0,
            refusing: false,
            drop_refused: false,
            unreadable: false,
            bound_out: false,
        })
    }
}

impl Fan for SimFan {
    fn set_duty(&mut self, per_mille: u16) -> Result<()> {
        self.calls += 1;
        if self.bound_out {
            return Err(Status::NO_DEVICE);
        }
        if self.refusing {
            return Err(Status::IO);
        }
        if self.failing > 0 {
            self.failing -= 1;
            return Err(Status::IO);
        }
        self.last = per_mille;
        Ok(())
    }

    fn duty(&mut self) -> Result<u16> {
        self.reads += 1;
        if self.bound_out {
            return Err(Status::NO_DEVICE);
        }
        if self.unreadable {
            return Err(Status::IO);
        }
        Ok(self.last)
    }

    fn rpm(&mut self) -> Result<Option<u32>> {
        Ok(None)
    }

    fn release(&mut self) -> Result<()> {
        self.releases += 1;
        if self.bound_out {
            return Err(Status::NO_DEVICE);
        }
        if self.refusing {
            return Err(Status::IO);
        }
        self.last = 1000;
        Ok(())
    }

    fn hand_back(&mut self) -> Result<()> {
        self.hand_backs += 1;
        if self.bound_out {
            return Err(Status::NO_DEVICE);
        }
        if self.refusing {
            return Err(Status::IO);
        }
        Ok(())
    }
}

impl FanKnob for SimFan {
    fn calls(&mut self) -> Result<u32> {
        Ok(self.calls)
    }

    fn last(&mut self) -> Result<u32> {
        Ok(u32::from(self.last))
    }

    fn releases(&mut self) -> Result<u32> {
        Ok(self.releases)
    }

    fn hand_backs(&mut self) -> Result<u32> {
        Ok(self.hand_backs)
    }

    fn fail_next(&mut self, n: u32) -> Result<()> {
        self.failing = n;
        Ok(())
    }

    fn refuse(&mut self, on: u32) -> Result<()> {
        self.refusing = on != 0;
        Ok(())
    }

    fn drop_refused(&mut self, on: u32) -> Result<()> {
        self.drop_refused = on != 0;
        Ok(())
    }

    fn unreadable(&mut self, on: u32) -> Result<()> {
        self.unreadable = on != 0;
        Ok(())
    }

    fn bound_out(&mut self, on: u32) -> Result<()> {
        self.bound_out = on != 0;
        // The host's drop: the fan goes to full duty, unless the drop is refused as the writes are.
        if self.bound_out && !self.drop_refused {
            self.last = 1000;
        }
        Ok(())
    }

    fn reads(&mut self) -> Result<u32> {
        Ok(self.reads)
    }
}

/// A board: a simulated fan, a simulated temperature `cpu` at `cpu_milli`, and the controller
/// wired to them. `two` adds a second temperature `board` at 60 degrees, wired as `t1`.
fn board(cpu_milli: i32, two: bool) -> String {
    let mut text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"fan-test\"\n\
         [[devices]]\nid = \"fan\"\ndriver = \"sim-fan\"\n\
         [[devices]]\nid = \"cpu\"\ndriver = \"sim-temperature\"\nconfig = {{ milli = {cpu_milli} }}\n"
    );
    let mut uses = String::from("fan = \"fan\", t0 = \"cpu\"");
    if two {
        text.push_str(
            "[[devices]]\nid = \"board\"\ndriver = \"sim-temperature\"\nconfig = { milli = 60000 }\n",
        );
        uses.push_str(", t1 = \"board\"");
    }
    text.push_str(&format!(
        "[[devices]]\nid = \"ctl\"\ndriver = \"fan-control\"\nuses = {{ {uses} }}\n\
         config = {{ curve_t0 = \"30000:300,60000:600,75000:1000\" }}\n"
    ));
    text
}

fn enc<T: Encode + ?Sized>(value: &T) -> Vec<u8> {
    let mut buffer = [0u8; 256];
    let mut writer = Writer::new(&mut buffer);
    value.encode(&mut writer).unwrap();
    let used = writer.len();
    buffer[..used].to_vec()
}

struct Rig {
    host: Host,
    clock: SimClock,
}

fn start_rig(text: &str) -> Rig {
    let sim = SimBoard::new();
    let clock = sim.clock();
    // The host's own bind-out (five failures in a row) is off here: the fan's bind-out is the test's knob, which
    // runs the drop as the host does. A host that binds out on its own would replace the sim's fan with a new one.
    let host = Host::start(
        &parse(text).unwrap(),
        &[
            entry::<FanController>(),
            entry::<SimFan>(),
            entry::<SimTemperature>(),
        ],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options {
            error_limit: u32::MAX,
            ..Options::default()
        },
    )
    .unwrap();
    Rig { host, clock }
}

impl Rig {
    fn call(&self, id: &str, contract: u8, method: u16, args: &[u8]) -> Vec<u8> {
        let index = self.host.device(id).unwrap().index;
        let mut reply = [0u8; 256];
        let n = self
            .host
            .call(index, MethodId { contract, method }, args, &mut reply)
            .unwrap();
        reply[..n].to_vec()
    }

    /// A `u32` from a simulated fan's or temperature's knob (contract 1).
    fn knob(&self, id: &str, method: u16) -> u32 {
        u32::from_le_bytes(self.call(id, 1, method, &[]).try_into().unwrap())
    }

    fn duty(&self) -> u32 {
        self.knob("fan", 2)
    }

    fn calls(&self) -> u32 {
        self.knob("fan", 1)
    }

    fn releases(&self) -> u32 {
        self.knob("fan", 3)
    }

    fn hand_backs(&self) -> u32 {
        self.knob("fan", 5)
    }

    fn reads(&self) -> u32 {
        self.knob("fan", 10)
    }

    fn fail_fan(&self, n: u32) {
        self.call("fan", 1, 4, &enc(&n));
    }

    fn set_knob(&self, method: u16, on: u32) {
        self.call("fan", 1, method, &enc(&on));
    }

    fn bind_out(&self, on: u32) {
        self.set_knob(9, on);
    }

    /// The controller's state, or `None` when its duty is unknown (the state read answers `NOT_FOUND`).
    fn state_if_known(&self) -> Option<State> {
        let index = self.host.device("ctl").unwrap().index;
        let mut reply = [0u8; 256];
        let n = self
            .host
            .call(
                index,
                MethodId {
                    contract: 0,
                    method: 1,
                },
                &[],
                &mut reply,
            )
            .ok()?;
        Some(State::decode(&mut Reader::new(&reply[..n])).unwrap())
    }

    /// Whether the controller's duty item is in its description.
    fn has_duty_item(&self) -> bool {
        let index = self.host.device("ctl").unwrap().index;
        let description = self.host.describe(index).unwrap();
        description.items().any(|item| item.name == "duty")
    }

    fn set_temp(&self, milli: i32) {
        self.call("cpu", 1, 1, &enc(&milli));
    }

    fn fail_temp(&self, on: u32) {
        self.call("cpu", 1, 2, &enc(&on));
    }

    /// A method of the controller (contract 0).
    fn ctl(&self, method: u16, args: &[u8]) -> Vec<u8> {
        self.call("ctl", 0, method, args)
    }

    fn state(&self) -> State {
        let bytes = self.ctl(1, &[]);
        State::decode(&mut Reader::new(&bytes)).unwrap()
    }

    fn health(&self) -> Health {
        self.host.device("ctl").unwrap().health
    }

    /// One second on the simulated clock, then the host's ticks.
    fn second(&mut self) {
        self.clock.advance(Duration::from_secs(1));
        self.host.tick();
    }
}

#[test]
fn the_fan_follows_its_curve_and_an_early_tick_does_nothing() {
    let mut rig = start_rig(&board(40_000, false));
    rig.host.tick();
    assert_eq!(rig.duty(), 400, "30 to 60 degrees is 300 to 600 per-mille");
    assert_eq!(rig.calls(), 1);
    rig.host.tick();
    assert_eq!(rig.calls(), 1, "a tick that is not due writes nothing");

    rig.set_temp(70_000);
    rig.second();
    assert_eq!(rig.duty(), 866, "rising is immediate");
    rig.set_temp(30_000);
    rig.second();
    assert_eq!(rig.duty(), 816, "falling is 50 per-mille a second");
    rig.second();
    assert_eq!(rig.duty(), 766);
    assert_eq!(rig.calls(), 4, "one write a second");
    assert_eq!(rig.state().source, Some(0));
}

#[test]
fn a_failing_sensor_keeps_its_last_reading_for_max_age_then_runs_at_full_duty() {
    let mut rig = start_rig(&board(40_000, false));
    rig.host.tick();
    rig.fail_temp(1);
    rig.second();
    assert_eq!(rig.duty(), 400, "1 s old: still good");
    rig.second();
    assert_eq!(rig.duty(), 400, "2 s old");
    rig.second();
    assert_eq!(rig.duty(), 400, "3 s old is the limit");
    rig.second();
    assert_eq!(rig.duty(), 1000, "4 s old: stale, full duty");
    assert!(rig.state().degraded);
    assert!(matches!(rig.health(), Health::Degraded(_)));
    assert_eq!(
        rig.calls(),
        5,
        "the controller kept stepping while degraded"
    );

    rig.fail_temp(0);
    rig.second();
    assert_eq!(
        rig.duty(),
        950,
        "a reading is back: the curve takes over, falling"
    );
    assert!(!rig.state().degraded);
    assert_eq!(rig.health(), Health::Ok);
}

#[test]
fn a_sensor_broken_from_the_start_runs_at_full_duty_until_it_reads() {
    let mut rig = start_rig(&board(40_000, false));
    rig.fail_temp(1);
    rig.host.tick();
    assert_eq!(rig.duty(), 1000);
    assert!(rig.state().degraded);
    rig.fail_temp(0);
    rig.second();
    assert_eq!(rig.duty(), 950);
}

#[test]
fn a_failed_write_releases_the_fan_and_retries_on_a_backoff_capped_at_8_seconds() {
    let mut rig = start_rig(&board(40_000, false));
    rig.fail_fan(5);
    rig.host.tick();
    let mut attempts = Vec::new();
    let mut seen = 0;
    for t in 0..=23u32 {
        if t > 0 {
            rig.second();
        }
        if rig.calls() != seen {
            seen = rig.calls();
            attempts.push(t);
        }
    }
    // Five writes fail, each followed by a release. The waits are 1, 2, 4, 8, 8 seconds.
    assert_eq!(attempts, [0, 1, 3, 7, 15, 23]);
    assert_eq!(rig.releases(), 5);
    // The fan was at full duty while its writes were refused, so the first write that works falls from
    // full at the slew limit (50 per mille a second, over the 8 s since the last step): 1000 - 400.
    assert_eq!(rig.duty(), 600);
    for _ in 0..12 {
        if rig.duty() == 400 {
            break;
        }
        rig.second();
    }
    assert_eq!(rig.duty(), 400, "the curve is reached by falling");
    assert_eq!(rig.health(), Health::Ok);
}

/// The seconds (from now, one a second) at which the controller made an attempt at the fan: a write while the fan is
/// in, or a read while the host has it bound out (the attempt that finds out whether it is bound again).
fn attempt_seconds(rig: &mut Rig, seconds: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let (mut calls, mut reads) = (rig.calls(), rig.reads());
    for t in 1..=seconds {
        rig.second();
        if rig.calls() != calls || rig.reads() != reads {
            calls = rig.calls();
            reads = rig.reads();
            out.push(t);
        }
    }
    out
}

/// The retry waits are 1, 2, 4 and then 8 s, and stay at 8 s: the last gaps of a run of attempts are 8 s.
fn retried_past_the_cap(times: &[u32]) {
    assert!(times.len() >= 6, "retried past the cap: {times:?}");
    for pair in times[times.len() - 3..].windows(2) {
        assert_eq!(
            pair[1] - pair[0],
            8,
            "the wait stays at the 8 s cap: {times:?}"
        );
    }
}

#[test]
fn case_1_writes_refused_and_the_drop_lands_read_back_is_full_duty_once_the_fan_is_bound_again() {
    // The fan is at 400 (40 C). Its writes and its release are refused, so the safe state is not applied, and the
    // duty item reads back 400, where the fan is. The host binds the fan out, and its drop lands: the fan is at full
    // duty, but nothing can read that while it is out, so the duty item is absent. Retries go on past the cap. The fan
    // is bound again and reads full duty: the safe state is applied, and control resumes on the curve.
    let mut rig = start_rig(&board(40_000, false));
    rig.host.tick();
    assert_eq!(rig.duty(), 400);
    rig.set_knob(6, 1);
    rig.second();
    assert_eq!(
        rig.state_if_known().map(|s| s.duty),
        Some(400),
        "the read-back is where the fan is"
    );
    assert_eq!(
        rig.health(),
        Health::Degraded(lemnos_driver_fan_control::law::REFUSED),
        "not applied"
    );
    assert!(rig.has_duty_item());

    rig.bind_out(1);
    assert_eq!(rig.duty(), 1000, "the host's drop landed on the fan");
    rig.second();
    assert_eq!(rig.state_if_known(), None, "bound out: the duty is unknown");
    assert!(!rig.has_duty_item(), "the duty item is absent");
    assert_eq!(
        rig.health(),
        Health::Degraded(lemnos_driver_fan_control::law::BOUND_OUT)
    );

    let out = attempt_seconds(&mut rig, 60);
    retried_past_the_cap(&out);
    assert_eq!(rig.state_if_known(), None, "still out");

    // Bound again, and writes work again. The first attempt after that reads the drop's full duty back.
    rig.set_knob(6, 0);
    rig.bind_out(0);
    let mut seen = None;
    for _ in 0..20 {
        rig.second();
        if let Some(state) = rig.state_if_known() {
            seen = Some(state.duty);
            break;
        }
    }
    assert_eq!(
        seen,
        Some(1000),
        "read back on rebind: the drop's full duty"
    );
    assert_eq!(
        rig.health(),
        Health::Degraded(lemnos_driver_fan_control::law::REBOUND_SAFE)
    );
    for _ in 0..15 {
        rig.second();
        if rig.duty() == 400 {
            break;
        }
    }
    assert_eq!(rig.duty(), 400, "control resumes on the curve");
    assert_eq!(rig.health(), Health::Ok);
}

#[test]
fn case_2_writes_refused_and_the_drop_refused_too_read_back_is_the_last_duty_and_control_resumes_on_rebind()
 {
    // The fan is at 600 (60 C). Its writes, release and drop are all refused: the fan stays at 600, and reads show it.
    // The host binds it out, which is the same: the duty is unknown while it is out. Retries go on past the cap. The
    // fan is bound again, reads 600, and control resumes.
    let mut rig = start_rig(&board(60_000, false));
    rig.host.tick();
    assert_eq!(rig.duty(), 600);
    rig.set_knob(6, 1);
    rig.set_knob(7, 1);
    rig.second();
    assert_eq!(
        rig.state_if_known().map(|s| s.duty),
        Some(600),
        "the fan is at 600"
    );
    assert_eq!(
        rig.health(),
        Health::Degraded(lemnos_driver_fan_control::law::REFUSED)
    );

    rig.bind_out(1);
    assert_eq!(
        rig.duty(),
        600,
        "the drop was refused: the fan stays at 600"
    );
    rig.second();
    assert_eq!(rig.state_if_known(), None);
    assert!(!rig.has_duty_item());
    let out = attempt_seconds(&mut rig, 60);
    retried_past_the_cap(&out);

    rig.set_knob(6, 0);
    rig.set_knob(7, 0);
    rig.bind_out(0);
    let mut seen = None;
    for _ in 0..20 {
        rig.second();
        if let Some(state) = rig.state_if_known() {
            seen = Some(state.duty);
            break;
        }
    }
    assert_eq!(seen, Some(600), "read back on rebind: still 600");
    assert_eq!(
        rig.health(),
        Health::Degraded(lemnos_driver_fan_control::law::REBOUND)
    );
    for _ in 0..5 {
        rig.second();
    }
    assert_eq!(rig.health(), Health::Ok, "control resumes");
    assert_eq!(rig.duty(), 600, "the curve at 60 C");
}

#[test]
fn case_3_writes_and_reads_both_fail_duty_is_absent_and_control_resumes_when_bound_again() {
    // Writes and reads both fail, and the drop is refused too. The duty is unknown from the first failure: the duty
    // item is absent, and the health says so. Retries go on past the cap. The fan is bound again, but its reads
    // still fail: the controller's write takes, and control resumes.
    let mut rig = start_rig(&board(40_000, false));
    rig.host.tick();
    rig.set_knob(6, 1);
    rig.set_knob(7, 1);
    rig.set_knob(8, 1);
    rig.second();
    assert_eq!(
        rig.state_if_known(),
        None,
        "the read after the failed write failed too"
    );
    assert!(!rig.has_duty_item());
    assert_eq!(
        rig.health(),
        Health::Degraded(lemnos_driver_fan_control::law::REFUSED_UNKNOWN)
    );

    let refused = attempt_seconds(&mut rig, 40);
    retried_past_the_cap(&refused);
    assert_eq!(rig.state_if_known(), None);

    // The controller learns the fan is out at its next attempt, which is a write that answers NO_DEVICE.
    rig.bind_out(1);
    let out = attempt_seconds(&mut rig, 60);
    retried_past_the_cap(&out);
    assert_eq!(
        rig.health(),
        Health::Degraded(lemnos_driver_fan_control::law::BOUND_OUT)
    );
    assert_eq!(rig.state_if_known(), None);

    rig.set_knob(6, 0);
    rig.bind_out(0);
    for _ in 0..20 {
        rig.second();
        if rig.health() == Health::Ok {
            break;
        }
    }
    assert_eq!(
        rig.health(),
        Health::Ok,
        "the write took once the fan was bound again"
    );
    assert_eq!(
        rig.state_if_known().map(|s| s.duty),
        Some(400),
        "reads still fail: the duty is the written one"
    );
    assert!(rig.has_duty_item());
}

#[test]
fn disabling_hands_the_fan_back_and_enabling_takes_it_again() {
    let mut rig = start_rig(&board(40_000, false));
    rig.host.tick();
    assert_eq!(rig.calls(), 1);

    rig.ctl(8, &enc(&false));
    assert_eq!(
        rig.hand_backs(),
        1,
        "the fan is handed back to its baseline"
    );
    assert_eq!(
        rig.releases(),
        0,
        "a disable is not a failure: no full-duty release"
    );
    assert_eq!(rig.state().duty, 0);
    rig.second();
    rig.second();
    assert_eq!(rig.calls(), 1, "no writes while disabled");

    rig.ctl(8, &enc(&true));
    assert_eq!(rig.calls(), 2, "enabled: the first step is at once");
    assert_eq!(rig.duty(), 400);
}

#[test]
fn a_hold_overrides_the_curve_for_its_time_then_the_curve_takes_over() {
    let mut rig = start_rig(&board(40_000, false));
    rig.host.tick();
    rig.ctl(6, &[enc(&900i32), enc(&3_000u32)].concat());
    assert_eq!(rig.duty(), 900, "the hold is written at once");
    assert!(rig.state().held);
    rig.second();
    assert_eq!(rig.duty(), 900);
    rig.second();
    assert_eq!(rig.duty(), 900);
    rig.second();
    assert_eq!(
        rig.duty(),
        850,
        "the hold has run out: the duty comes down from 900"
    );
    assert!(!rig.state().held);
}

#[test]
fn a_curve_set_through_the_contract_takes_effect_and_a_restart_forgets_it() {
    let mut rig = start_rig(&board(40_000, false));
    rig.host.tick();
    assert_eq!(rig.duty(), 400);
    rig.set_temp(35_000);
    rig.second();
    assert_eq!(rig.duty(), 350, "the board's curve at 35 degrees");

    let mut points = [0u8; 2 * POINT_BYTES];
    let n = Point::write_all(
        &[
            Point {
                value: 30_000,
                duty: 300,
            },
            Point {
                value: 40_000,
                duty: 900,
            },
        ],
        &mut points,
    )
    .unwrap();
    rig.ctl(5, &[enc(&0u8), enc(&points[..n])].concat());
    assert_eq!(rig.duty(), 600, "the steeper curve at 35 degrees, at once");

    // Tuning is in memory only: a new controller starts from the board again.
    let restarted = start_rig(&board(35_000, false));
    restarted.host.tick();
    assert_eq!(restarted.duty(), 350);
}

#[test]
fn the_hotter_of_two_temperatures_drives_the_fan() {
    let rig = start_rig(&board(40_000, true));
    rig.host.tick();
    assert_eq!(
        rig.duty(),
        600,
        "the board at 60 degrees asks for more than the cpu at 40"
    );
    assert_eq!(rig.state().source, Some(1));
}

#[test]
fn stopping_the_controller_hands_the_fan_back() {
    let rig = start_rig(&board(40_000, false));
    rig.host.tick();
    rig.host.off("ctl").unwrap();
    assert_eq!(rig.hand_backs(), 1);
    assert_eq!(rig.releases(), 0);
}
