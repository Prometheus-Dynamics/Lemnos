//! The controller on a host, with simulated devices: a temperature the test sets (and can make
//! fail), and a fan that records every write and every release (and can make writes fail).
//! Time is the simulated clock; each second the test advances it and ticks the host.

use std::sync::Arc;
use std::time::Duration;

use lemnos_contract_fan::Fan;
use lemnos_contract_fan_control::{POINT_BYTES, Point, State};
use lemnos_contract_temperature::Temperature;
use lemnos_core::{
    Config, Decode, Encode, Health, Lifecycle, MethodId, Reader, Result, Status, Writer, contract,
    driver,
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

/// The test's knob on a simulated fan: what it was asked for, and how it can fail.
#[contract("sim-fan-knob", version = 1)]
pub trait FanKnob {
    /// Writes asked for so far, failed ones included.
    #[method(1)]
    fn calls(&mut self) -> Result<u32>;
    /// The duty last written.
    #[method(2)]
    fn last(&mut self) -> Result<u32>;
    /// Hand-backs so far.
    #[method(3)]
    fn releases(&mut self) -> Result<u32>;
    /// The next `n` writes fail.
    #[method(4)]
    fn fail_next(&mut self, n: u32) -> Result<()>;
}

#[driver("sim-fan", provides = [Fan, FanKnob])]
pub struct SimFan {
    calls: u32,
    last: u16,
    releases: u32,
    failing: u32,
}

impl Lifecycle for SimFan {
    type Needs = SimFanNeeds;

    fn start(_: SimFanNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {
            calls: 0,
            last: 0,
            releases: 0,
            failing: 0,
        })
    }
}

impl Fan for SimFan {
    fn set_duty(&mut self, per_mille: u16) -> Result<()> {
        self.calls += 1;
        if self.failing > 0 {
            self.failing -= 1;
            return Err(Status::IO);
        }
        self.last = per_mille;
        Ok(())
    }

    fn duty(&mut self) -> Result<u16> {
        Ok(self.last)
    }

    fn rpm(&mut self) -> Result<Option<u32>> {
        Ok(None)
    }

    fn release(&mut self) -> Result<()> {
        self.releases += 1;
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

    fn fail_next(&mut self, n: u32) -> Result<()> {
        self.failing = n;
        Ok(())
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
    let host = Host::start(
        &parse(text).unwrap(),
        &[
            entry::<FanController>(),
            entry::<SimFan>(),
            entry::<SimTemperature>(),
        ],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
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

    fn fail_fan(&self, n: u32) {
        self.call("fan", 1, 4, &enc(&n));
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
    assert_eq!(rig.duty(), 400);
    assert_eq!(rig.health(), Health::Ok);
}

#[test]
fn disabling_releases_the_fan_and_enabling_takes_it_again() {
    let mut rig = start_rig(&board(40_000, false));
    rig.host.tick();
    assert_eq!(rig.calls(), 1);

    rig.ctl(8, &enc(&false));
    assert_eq!(rig.releases(), 1, "the fan is released");
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
fn stopping_the_controller_releases_the_fan() {
    let rig = start_rig(&board(40_000, false));
    rig.host.tick();
    rig.host.off("ctl").unwrap();
    assert_eq!(rig.releases(), 1);
}
