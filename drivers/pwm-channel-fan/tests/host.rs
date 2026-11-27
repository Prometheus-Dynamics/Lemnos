//! The fan on a host with a simulated PWM channel and tachometer line. Time is the simulated
//! clock: a test advances it and runs the host's due ticks.

use std::sync::Arc;
use std::time::Duration;

use lemnos_core::{CallerId, Decode, ErrorKind, Health, Reader, Status};
use lemnos_driver_pwm_channel_fan::PwmChannelFan;
use lemnos_hal::raw::{EdgeDetect, Polarity};
use lemnos_host::board::parse;
use lemnos_host::{Host, entry};
use lemnos_sim::mock::{MockLine, MockPwm};
use lemnos_sim::{SimBoard, SimClock};

use lemnos_core::MethodId;

const PERIOD: u64 = 40_000; // 25 kHz
const MS: u64 = 1_000_000;

struct Rig {
    host: Host,
    clock: SimClock,
    pwm: MockPwm,
    tach: Option<MockLine>,
    sim: SimBoard,
}

/// A fan on `pwm0`, with the given `config` entries and, when `tach` is set, a tachometer on
/// `tach0`.
fn rig(config: &str, tach: bool) -> Rig {
    let sim = SimBoard::new();
    let clock = sim.clock();
    let pwm = sim.add_pwm("pwm0");
    let tach_line = tach.then(|| sim.add_line("tach0"));
    let uses = if tach {
        "pwm = \"pwm0\", tach = \"tach0\""
    } else {
        "pwm = \"pwm0\""
    };
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"fan-test\"\n\
         [[devices]]\nid = \"fan\"\ndriver = \"pwm-channel-fan\"\nuses = {{ {uses} }}\n\
         config = {{ {config} }}\n"
    );
    let host = Host::start(
        &parse(&text).unwrap(),
        &[entry::<PwmChannelFan>()],
        Box::new(sim.clone()),
        Arc::new(clock.clone()),
        lemnos_test_options::options(),
    )
    .unwrap();
    // The daemon's loop runs the due ticks; the first one writes the start-up duty.
    host.run_ticks();
    Rig {
        host,
        clock,
        pwm,
        tach: tach_line,
        sim,
    }
}

/// The fan's `fan/1` contract (0), method numbers as the contract has them.
const SET_DUTY: u16 = 1;
const DUTY: u16 = 2;
const RPM: u16 = 3;
const RELEASE: u16 = 4;
const HAND_BACK: u16 = 5;

impl Rig {
    fn call(&self, method: u16, args: &[u8]) -> Vec<u8> {
        let index = self.host.device("fan").unwrap().index;
        let mut reply = [0u8; 64];
        let n = self
            .host
            .call(
                index,
                MethodId {
                    contract: 0,
                    method,
                },
                args,
                &mut reply,
            )
            .unwrap();
        reply[..n].to_vec()
    }

    fn set(&self, per_mille: u16) -> Result<(), Status> {
        self.call_status(SET_DUTY, &per_mille.to_le_bytes())
            .map(|_| ())
    }

    fn call_status(&self, method: u16, args: &[u8]) -> Result<usize, Status> {
        let index = self.host.device("fan").unwrap().index;
        let mut reply = [0u8; 64];
        self.host.call(
            index,
            MethodId {
                contract: 0,
                method,
            },
            args,
            &mut reply,
        )
    }

    fn duty(&self) -> u16 {
        u16::from_le_bytes(self.call(DUTY, &[]).try_into().unwrap())
    }

    fn rpm(&self) -> Option<u32> {
        let bytes = self.call(RPM, &[]);
        <Option<u32>>::decode(&mut Reader::new(&bytes)).unwrap()
    }

    fn hand_back(&self) {
        self.call(HAND_BACK, &[]);
    }

    fn release(&self) {
        self.call(RELEASE, &[]);
    }

    fn health(&self) -> Health {
        self.host.device("fan").unwrap().health
    }

    /// Advances the simulated clock by `by` and runs the due ticks.
    fn advance(&self, by: Duration) {
        self.clock.advance(by);
        self.host.run_ticks();
    }

    fn now(&self) -> u64 {
        self.clock.now_ns()
    }
}

#[test]
fn the_fan_starts_at_full_duty_and_a_request_sets_the_high_time_of_the_period() {
    let rig = rig("frequency_hz = 25000", false);
    let current = rig.pwm.current();
    assert_eq!(current.period_ns, PERIOD);
    assert_eq!(current.duty_ns, PERIOD, "full duty at start");
    assert!(current.enabled);
    assert_eq!(rig.duty(), 1000);

    rig.set(500).unwrap();
    assert_eq!(rig.pwm.current().duty_ns, PERIOD / 2);
    assert_eq!(rig.duty(), 500);

    rig.set(0).unwrap();
    assert_eq!(rig.pwm.current().duty_ns, 0, "zero stops the fan");
    assert!(rig.pwm.current().enabled, "the output stays enabled");
}

#[test]
fn inverted_sets_the_polarity_and_leaves_the_duty_as_asked() {
    let rig = rig("inverted = true", false);
    assert_eq!(rig.pwm.current().polarity, Polarity::Inversed);
    rig.set(250).unwrap();
    let current = rig.pwm.current();
    assert_eq!(current.polarity, Polarity::Inversed);
    assert_eq!(
        current.duty_ns,
        PERIOD / 4,
        "the duty is the fan's duty, not the output's"
    );
}

#[test]
fn a_small_request_is_raised_to_the_minimum_and_zero_is_still_zero() {
    let rig = rig("min_duty = 200", false);
    rig.set(50).unwrap();
    assert_eq!(rig.pwm.current().duty_ns, PERIOD / 5);
    assert_eq!(rig.duty(), 50, "the duty read back is the one asked for");
    rig.set(0).unwrap();
    assert_eq!(rig.pwm.current().duty_ns, 0);
}

#[test]
fn a_kick_runs_full_duty_from_a_stopped_fan_and_then_settles() {
    let rig = rig("kick_ms = 500", false);
    rig.advance(Duration::from_secs(1));
    rig.set(0).unwrap();
    assert_eq!(rig.pwm.current().duty_ns, 0);

    rig.set(400).unwrap();
    assert_eq!(rig.pwm.current().duty_ns, PERIOD, "the kick starts at full");
    rig.advance(Duration::from_millis(400));
    assert_eq!(rig.pwm.current().duty_ns, PERIOD, "still kicking at 400 ms");
    rig.advance(Duration::from_millis(200));
    assert_eq!(
        rig.pwm.current().duty_ns,
        PERIOD * 400 / 1000,
        "settled after 500 ms"
    );
}

#[test]
fn a_kick_does_not_repeat_while_the_fan_keeps_running() {
    let rig = rig("kick_ms = 500", false);
    rig.advance(Duration::from_secs(1));
    rig.set(0).unwrap();
    rig.set(400).unwrap();
    rig.advance(Duration::from_secs(1));
    assert_eq!(rig.pwm.current().duty_ns, PERIOD * 400 / 1000);
    rig.set(600).unwrap();
    assert_eq!(
        rig.pwm.current().duty_ns,
        PERIOD * 600 / 1000,
        "a change of duty on a running fan is not kicked"
    );
}

#[test]
fn a_failed_write_is_retried_on_the_backoff_and_capped_at_eight_seconds() {
    let rig = rig("", false);
    rig.advance(Duration::from_secs(1));
    let start = rig.now();
    // Every write fails from now until 20 s later.
    rig.sim.faults("pwm0").unwrap().outage(
        Duration::from_nanos(start),
        Some(Duration::from_secs(20)),
        ErrorKind::Busy,
    );

    rig.set(500).unwrap();
    assert_eq!(
        rig.pwm.current().duty_ns,
        PERIOD,
        "the write fails, so the duty is unchanged"
    );
    assert!(
        matches!(rig.health(), Health::Degraded(_)),
        "degraded at once"
    );

    // Retries at +1 s, +3 s, +7 s, +15 s, then every 8 s: +23 s. The outage ends at +20 s, so the
    // first retry that can land is the one at +23 s.
    let at = |secs: u64| start + secs * 1_000_000_000;
    let mut landed = None;
    for _ in 1..=30u64 {
        rig.advance(Duration::from_secs(1));
        if rig.pwm.current().duty_ns == PERIOD / 2 && landed.is_none() {
            landed = Some(rig.now());
        }
    }
    assert_eq!(landed, Some(at(23)), "the write lands on the 23 s retry");
    assert!(
        matches!(rig.health(), Health::Ok),
        "healthy again after the good write"
    );
}

#[test]
fn hand_back_is_full_duty_too_as_the_channel_has_no_baseline_to_return_to() {
    let rig = rig("", false);
    rig.set(100).unwrap();
    rig.hand_back();
    assert_eq!(
        rig.pwm.current().duty_ns,
        PERIOD,
        "hand-back is the safe state here"
    );
    assert_eq!(rig.duty(), 1000);
}

#[test]
fn release_and_stopping_the_device_both_leave_the_fan_at_full() {
    let rig = rig("", false);
    rig.set(100).unwrap();
    rig.release();
    assert_eq!(
        rig.pwm.current().duty_ns,
        PERIOD,
        "release is full duty, not a hand-back"
    );
    assert_eq!(rig.duty(), 1000);

    rig.set(100).unwrap();
    rig.host.off("fan").unwrap();
    assert_eq!(
        rig.pwm.current().duty_ns,
        PERIOD,
        "a stopped driver leaves the fan at full"
    );
}

#[test]
fn a_request_above_full_is_refused() {
    let rig = rig("", false);
    assert_eq!(rig.set(1001), Err(Status::INVALID_ARGUMENT));
    assert_eq!(rig.duty(), 1000);
}

#[test]
fn a_tachometer_reports_the_speed_from_the_edges_timestamps() {
    let rig = rig("pulses_per_rev = 2", true);
    let line = rig.tach.clone().unwrap();
    assert_eq!(
        line.config().edge,
        EdgeDetect::Rising,
        "the tach line reports rising edges"
    );
    assert_eq!(rig.rpm(), Some(0), "no pulses yet");

    // A pulse every 5 ms, two to a revolution: 6000 rpm. Ten pulses in 50 ms.
    let base = rig.now();
    for k in 0..10u64 {
        line.edge(true, base + k * 5 * MS);
        line.edge(false, base + k * 5 * MS + 2 * MS);
    }
    rig.advance(Duration::from_millis(100));
    assert_eq!(rig.rpm(), Some(6000));
}

#[test]
fn a_tachometer_reads_zero_when_its_pulses_stop() {
    let rig = rig("pulses_per_rev = 2", true);
    let line = rig.tach.clone().unwrap();
    let base = rig.now();
    for k in 0..4u64 {
        line.edge(true, base + k * 5 * MS);
        line.edge(false, base + k * 5 * MS + 2 * MS);
    }
    rig.advance(Duration::from_millis(100));
    assert_eq!(rig.rpm(), Some(6000));
    rig.advance(Duration::from_secs(2));
    assert_eq!(rig.rpm(), Some(0), "no pulse for over a second: stopped");
}

#[test]
fn without_a_tachometer_the_speed_is_not_there() {
    let rig = rig("", false);
    assert_eq!(rig.rpm(), None);
}

/// A caller that goes away, as a client does: two of them, and the daemon's own writes (`LOCAL`).
const CALLER_A: CallerId = CallerId(7);
const CALLER_B: CallerId = CallerId(8);

impl Rig {
    /// A `set_duty` made for `caller`, as a client's call is.
    fn set_as(&self, caller: CallerId, per_mille: u16) {
        let index = self.host.device("fan").unwrap().index;
        let mut reply = [0u8; 64];
        self.host
            .call_as(
                caller,
                index,
                MethodId {
                    contract: 0,
                    method: SET_DUTY,
                },
                &per_mille.to_le_bytes(),
                &mut reply,
            )
            .unwrap();
    }
}

#[test]
fn the_owner_going_hands_the_channel_back_to_full_duty_at_once() {
    let rig = rig("frequency_hz = 25000", false);
    rig.set_as(CALLER_A, 300);
    assert_eq!(rig.pwm.current().duty_ns, 12_000);
    // The host ticks the device after the call that tells it, so no time has to pass for the write.
    rig.host.caller_gone(CALLER_A);
    assert_eq!(
        rig.pwm.current().duty_ns,
        PERIOD,
        "full duty, the channel's hand-back"
    );
    assert_eq!(rig.duty(), 1000);
}

#[test]
fn an_older_caller_going_leaves_the_newer_owners_duty() {
    let rig = rig("frequency_hz = 25000", false);
    rig.set_as(CALLER_A, 300);
    rig.set_as(CALLER_B, 600);
    rig.host.caller_gone(CALLER_A);
    assert_eq!(rig.pwm.current().duty_ns, 24_000, "B's duty stays");
    rig.host.caller_gone(CALLER_B);
    assert_eq!(rig.pwm.current().duty_ns, PERIOD);
}

#[test]
fn a_controller_write_after_a_client_keeps_the_channel_when_the_client_goes() {
    let rig = rig("frequency_hz = 25000", false);
    rig.set_as(CALLER_A, 300);
    // The daemon's own write (LOCAL) after the client's makes the daemon the owner.
    rig.set(500).unwrap();
    rig.host.caller_gone(CALLER_A);
    assert_eq!(
        rig.pwm.current().duty_ns,
        20_000,
        "the controller's duty stays"
    );
}

#[test]
fn a_hand_back_goes_to_the_declared_default_duty_not_to_full_duty() {
    // The board declares 300 per mille as the fan's default. Its owner going hands the channel back to it.
    let rig = rig("frequency_hz = 25000, default_duty = 300", false);
    rig.set_as(CALLER_A, 800);
    assert_eq!(rig.pwm.current().duty_ns, 32_000);
    rig.host.caller_gone(CALLER_A);
    assert_eq!(
        rig.pwm.current().duty_ns,
        12_000,
        "the declared default, 300 of 1000, not full duty"
    );
}

#[test]
fn a_client_that_replaced_the_controllers_write_leaves_the_channel_at_its_duty_and_logs_it() {
    // The controller's write (LOCAL) is replaced by a client, which goes. No full-duty write follows: the
    // controller's next step writes its own duty, and the note says the channel went back to it.
    let rig = rig("frequency_hz = 25000", false);
    rig.set(500).unwrap();
    rig.set_as(CALLER_A, 300);
    assert_eq!(rig.pwm.current().duty_ns, 12_000);
    rig.host.caller_gone(CALLER_A);
    assert_eq!(
        rig.pwm.current().duty_ns,
        12_000,
        "nothing is written when the controller is the owner"
    );
    let index = rig.host.device("fan").unwrap().index;
    let mut notes = Vec::new();
    rig.host.take_notes(index, &mut |note| notes.push(note));
    assert_eq!(
        notes,
        vec![lemnos_core::Note::new("handed back to the controller")]
    );
}

/// `override_for`, the timed override (method 6 of `fan/1`).
const OVERRIDE: u16 = 6;

impl Rig {
    /// `caller` asks for `per_mille` for `ms` milliseconds.
    fn override_as(&self, caller: CallerId, per_mille: u16, ms: u32) -> Result<(), Status> {
        let index = self.host.device("fan").unwrap().index;
        let mut args = [0u8; 6];
        args[..2].copy_from_slice(&per_mille.to_le_bytes());
        args[2..].copy_from_slice(&ms.to_le_bytes());
        let mut reply = [0u8; 64];
        self.host
            .call_as(
                caller,
                index,
                MethodId {
                    contract: 0,
                    method: OVERRIDE,
                },
                &args,
                &mut reply,
            )
            .map(|_| ())
    }
}

/// The duty a timed override holds is written at once, and the channel goes back to its hand-back
/// duty on the tick at the deadline: not a tick after it.
#[test]
fn a_timed_override_gives_the_channel_back_on_the_tick_at_its_deadline() {
    let rig = rig("frequency_hz = 25000", false);
    rig.override_as(CALLER_A, 300, 2_000).unwrap();
    let asked = rig.now();
    assert_eq!(rig.pwm.current().duty_ns, 12_000, "held from the call");
    rig.advance(Duration::from_millis(1_999));
    assert_eq!(
        rig.pwm.current().duty_ns,
        12_000,
        "still held a millisecond early"
    );
    rig.advance(Duration::from_millis(1));
    assert_eq!(rig.now(), asked + 2_000 * MS, "the tick is at the deadline");
    assert_eq!(
        rig.pwm.current().duty_ns,
        PERIOD,
        "handed back: no default duty, so full"
    );
    assert_eq!(rig.duty(), 1000);
}

/// The owner going before its deadline hands the channel back at once, as `caller_gone` does.
#[test]
fn the_owner_going_before_the_deadline_hands_the_channel_back_at_once() {
    let rig = rig("frequency_hz = 25000", false);
    rig.override_as(CALLER_A, 300, 2_000).unwrap();
    rig.advance(Duration::from_millis(500));
    rig.host.caller_gone(CALLER_A);
    assert_eq!(
        rig.pwm.current().duty_ns,
        PERIOD,
        "at once, not at the deadline"
    );
}

/// A new set replaces the timer: the deadline of the override it replaced does nothing.
#[test]
fn a_new_set_replaces_the_override_timer() {
    let rig = rig("frequency_hz = 25000", false);
    rig.override_as(CALLER_A, 300, 2_000).unwrap();
    let asked = rig.now();
    rig.advance(Duration::from_millis(1_000));
    rig.set_as(CALLER_A, 600);
    rig.advance(Duration::from_millis(4_000));
    assert_eq!(rig.now(), asked + 5_000 * MS);
    assert_eq!(
        rig.pwm.current().duty_ns,
        24_000,
        "the set's duty stays past the old deadline"
    );
    assert_eq!(rig.duty(), 600);
}

/// A new override replaces the old one's timer, and the new deadline is the one that counts.
#[test]
fn a_new_override_replaces_the_timer_with_its_own_deadline() {
    let rig = rig("frequency_hz = 25000", false);
    rig.override_as(CALLER_A, 300, 2_000).unwrap();
    let asked = rig.now();
    rig.advance(Duration::from_millis(1_000));
    rig.override_as(CALLER_B, 600, 3_000).unwrap();
    rig.advance(Duration::from_millis(1_500));
    assert_eq!(rig.now(), asked + 2_500 * MS, "past A's deadline");
    assert_eq!(rig.pwm.current().duty_ns, 24_000, "B's duty holds");
    rig.advance(Duration::from_millis(1_500));
    assert_eq!(rig.now(), asked + 4_000 * MS, "B's deadline");
    assert_eq!(
        rig.pwm.current().duty_ns,
        PERIOD,
        "handed back at B's deadline"
    );
}

/// Bounds: 1 s to 10 min, refused by name outside them, and a refusal sets nothing.
#[test]
fn an_override_outside_one_second_to_ten_minutes_is_refused_and_sets_nothing() {
    let rig = rig("frequency_hz = 25000", false);
    assert_eq!(
        rig.override_as(CALLER_A, 300, 999),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        rig.override_as(CALLER_A, 300, 600_001),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(rig.pwm.current().duty_ns, PERIOD, "nothing was set");
    assert_eq!(rig.duty(), 1000);
    rig.override_as(CALLER_A, 300, 1_000).unwrap();
    rig.override_as(CALLER_A, 300, 600_000).unwrap();
}
