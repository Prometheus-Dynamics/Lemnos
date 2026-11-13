//! The control law, on explicit times. Dev's `fan_control` tests carried over, in nanoseconds.

use crate::law::{Control, Curve, DEGRADED, FULL, WRITE_FAILED, check_settings, parse_curve};
use lemnos_contract_fan_control::{Point, Settings};
use lemnos_core::{Config, ConfigValue, Driver, Health, Status};
use std::vec::Vec;

/// One second, in nanoseconds.
const S: u64 = 1_000_000_000;
/// One millisecond, in nanoseconds.
const MS: u64 = 1_000_000;

const CPU: [Point; 3] = [
    Point {
        value: 50_000,
        duty: 700,
    },
    Point {
        value: 65_000,
        duty: 850,
    },
    Point {
        value: 75_000,
        duty: 1000,
    },
];

fn settings(floor: i32, hysteresis: i32, slew_per_s: i32, kick_ms: u32) -> Settings {
    Settings {
        floor,
        hysteresis,
        slew_per_s,
        kick_ms,
    }
}

/// The usual tuning: a 700 floor, a 3 degree band, 50 per-mille per second.
fn tuned() -> Settings {
    settings(700, 3_000, 50, 0)
}

/// A controller with the CPU curve on input 0 and nothing else.
fn one(settings: Settings) -> Control {
    let cpu = Curve::new(&CPU).unwrap();
    Control::new(settings, 3_000, [Some(cpu), None, None, None]).unwrap()
}

/// Reads `value` (millidegrees) at `now`, steps, and returns the duty written.
fn step_with(control: &mut Control, now: u64, value: i32) -> Option<i32> {
    control.observe(0, Some(value), now);
    control.step(now)
}

fn cpu() -> Curve {
    Curve::new(&CPU).unwrap()
}

#[test]
fn curves_are_checked() {
    assert!(Curve::new(&CPU).is_ok());
    assert_eq!(Curve::new(&[]), Err(Status::INVALID_ARGUMENT));
    let unordered = [Point { value: 2, duty: 1 }, Point { value: 1, duty: 2 }];
    assert_eq!(Curve::new(&unordered), Err(Status::INVALID_ARGUMENT));
    let bad = [Point {
        value: 1,
        duty: 1001,
    }];
    assert_eq!(Curve::new(&bad), Err(Status::INVALID_ARGUMENT));
    let nine = [Point { value: 0, duty: 0 }; 9];
    assert_eq!(Curve::new(&nine), Err(Status::INVALID_ARGUMENT));
}

#[test]
fn the_curve_interpolates_and_clamps() {
    let c = cpu();
    assert_eq!(c.eval(0), 700);
    assert_eq!(c.eval(57_500), 775);
    assert_eq!(c.eval(90_000), 1000);
}

#[test]
fn a_curve_text_parses_and_a_broken_one_is_refused() {
    let c = parse_curve("30000:300, 60000:600,75000:1000").unwrap();
    assert_eq!(c.points().len(), 3);
    assert_eq!(c.eval(60_000), 600);
    assert_eq!(parse_curve(""), Err(Status::INVALID_ARGUMENT));
    assert_eq!(parse_curve("30000"), Err(Status::INVALID_ARGUMENT));
    assert_eq!(parse_curve("x:1"), Err(Status::INVALID_ARGUMENT));
    assert_eq!(parse_curve("1:2001"), Err(Status::INVALID_ARGUMENT));
    let nine = "1:1,2:2,3:3,4:4,5:5,6:6,7:7,8:8,9:9";
    assert_eq!(parse_curve(nine), Err(Status::INVALID_ARGUMENT));
}

#[test]
fn the_settings_are_checked() {
    assert!(check_settings(&tuned()).is_ok());
    assert!(check_settings(&settings(1001, 0, 0, 0)).is_err());
    assert!(check_settings(&settings(0, -1, 0, 0)).is_err());
    assert!(check_settings(&settings(0, 0, -1, 0)).is_err());
}

#[test]
fn the_floor_holds_at_low_demand() {
    let mut c = one(tuned());
    assert_eq!(step_with(&mut c, 0, 30_000), Some(700));
}

#[test]
fn rising_is_immediate_and_falling_is_slew_limited() {
    let mut c = one(tuned());
    assert_eq!(step_with(&mut c, 0, 80_000), Some(1000));
    // 50 per-mille per second: one second later the duty has fallen by at most 50.
    assert_eq!(step_with(&mut c, S, 30_000), Some(950));
    assert_eq!(step_with(&mut c, 2 * S, 30_000), Some(900));
}

#[test]
fn hysteresis_keeps_the_peak_until_it_has_fallen_the_band() {
    let mut c = one(tuned());
    step_with(&mut c, 0, 70_000);
    // 2 degrees below the peak is inside the 3 degree band: still evaluated at 70.
    assert_eq!(step_with(&mut c, 10 * S, 68_000), Some(cpu().eval(70_000)));
    // 4 degrees below: the peak resets to the new value.
    assert_eq!(step_with(&mut c, 20 * S, 66_000), Some(cpu().eval(66_000)));
}

#[test]
fn a_missing_required_input_forces_full_duty_and_degrades() {
    let mut c = one(tuned());
    assert_eq!(step_with(&mut c, 0, 40_000), Some(700));
    // No reading: the last good one still counts for max_age_ms.
    c.observe(0, None, S);
    assert_eq!(c.step(S), Some(700));
    assert!(!c.state().degraded);
    // Older than 3 s: stale.
    c.observe(0, None, 4 * S);
    assert_eq!(c.step(4 * S), Some(FULL));
    let state = c.state();
    assert!(state.degraded);
    assert_eq!(c.health(), Health::Degraded(DEGRADED));
}

#[test]
fn a_sensor_that_never_read_forces_full_duty_from_the_start() {
    let mut c = one(tuned());
    assert_eq!(c.step(0), Some(FULL));
    assert_eq!(c.health(), Health::Degraded(DEGRADED));
}

#[test]
fn a_fresh_reading_after_a_failure_brings_the_curve_back() {
    let mut c = one(tuned());
    c.step(0);
    c.observe(0, None, 4 * S);
    assert_eq!(c.step(4 * S), Some(FULL));
    // Falls at the slew rate, not at once: the curve is 700 and the fan has 1000.
    assert_eq!(step_with(&mut c, 5 * S, 40_000), Some(950));
    assert_eq!(c.health(), Health::Ok);
}

#[test]
fn an_input_that_is_not_wired_is_not_counted() {
    let mut c = one(tuned());
    assert!(c.present(0));
    assert!(!c.present(1));
    c.observe(1, Some(90_000), 0);
    assert_eq!(step_with(&mut c, 0, 30_000), Some(700));
    assert_eq!(c.state().source, Some(0));
    assert_eq!(c.health(), Health::Ok);
    assert_eq!(c.curve(1), Err(Status::NOT_FOUND));
}

#[test]
fn the_hottest_input_wins_and_is_reported() {
    let board = Curve::new(&[
        Point { value: 0, duty: 0 },
        Point {
            value: 10_000,
            duty: 1000,
        },
    ])
    .unwrap();
    let mut c = Control::new(tuned(), 3_000, [Some(cpu()), Some(board), None, None]).unwrap();
    c.observe(0, Some(50_000), 0);
    c.observe(1, Some(9_000), 0);
    assert_eq!(c.step(0), Some(900));
    let state = c.state();
    assert_eq!(state.source, Some(1));
    assert_eq!(state.demand, 900);
}

#[test]
fn spin_up_kicks_a_stopped_fan_when_the_floor_is_zero() {
    let mut c = one(settings(0, 3_000, 50, 500));
    // Demand is 700 at 50 degrees; the kick runs at full for 500 ms first.
    assert_eq!(step_with(&mut c, 0, 50_000), Some(1000));
    assert_eq!(step_with(&mut c, 400 * MS, 50_000), Some(1000));
    // After the kick the duty comes down at the slew rate: 200 ms at 50 per second.
    assert_eq!(step_with(&mut c, 600 * MS, 50_000), Some(990));
    assert_eq!(step_with(&mut c, 1_600 * MS, 50_000), Some(940));
    assert_eq!(step_with(&mut c, 6_600 * MS, 50_000), Some(700));
}

#[test]
fn a_hold_overrides_the_curve_for_its_time_then_the_curve_takes_over() {
    let mut c = one(tuned());
    c.hold(900, 3_000).unwrap();
    c.begin(0);
    assert_eq!(step_with(&mut c, 0, 40_000), Some(900));
    assert!(c.state().held);
    assert_eq!(step_with(&mut c, S, 40_000), Some(900));
    assert_eq!(step_with(&mut c, 2 * S, 40_000), Some(900));
    // At 3 s the hold has run out: the duty comes down from 900 at the slew rate.
    c.begin(3 * S);
    assert_eq!(step_with(&mut c, 3 * S, 40_000), Some(850));
    assert!(!c.state().held);
}

#[test]
fn a_hold_is_bounded_by_its_own_end_for_the_next_tick() {
    let mut c = one(tuned());
    c.step(0);
    c.hold(300, 500).unwrap();
    c.begin(0);
    assert_eq!(
        c.next_due(),
        Some(500 * MS),
        "the hold ends before the next step"
    );
}

#[test]
fn a_hold_does_not_outrank_the_failsafe_or_the_floor() {
    let mut c = one(tuned());
    c.hold(0, 5_000).unwrap();
    c.begin(0);
    // No reading at all: stale, so full duty whatever the hold asks.
    assert_eq!(c.step(0), Some(FULL));
    assert!(c.state().held);

    let mut c = one(tuned());
    c.hold(0, 5_000).unwrap();
    c.begin(0);
    // A reading: the hold is below the floor, so the floor holds.
    assert_eq!(step_with(&mut c, 0, 40_000), Some(700));
}

#[test]
fn a_hold_is_refused_when_it_cannot_run() {
    let mut c = one(tuned());
    assert_eq!(c.hold(1001, 1), Err(Status::INVALID_ARGUMENT));
    assert_eq!(c.hold(500, 0), Err(Status::INVALID_ARGUMENT));
    c.set_enabled(false);
    assert_eq!(c.hold(500, 10), Err(Status::NO_DEVICE));
}

#[test]
fn a_failed_write_retries_with_a_backoff_capped_at_8_seconds() {
    let mut c = one(tuned());
    let mut attempts = Vec::new();
    let mut fails = 0;
    // Steps that are not in a retry wait return a write. The first five fail.
    for t in 0..=23 {
        let now = t * S;
        c.observe(0, Some(40_000), now);
        if let Some(_duty) = c.step(now) {
            attempts.push(t);
            if fails < 5 {
                fails += 1;
                c.write_failed(now);
                assert_eq!(c.health(), Health::Degraded(WRITE_FAILED));
            } else {
                c.wrote();
            }
        }
    }
    // First try at 0. Waits of 1, 2, 4, 8 and then 8 again.
    assert_eq!(attempts, [0, 1, 3, 7, 15, 23]);
    assert_eq!(c.health(), Health::Ok);
}

#[test]
fn a_failed_write_reports_zero_duty_while_the_fan_is_released() {
    let mut c = one(tuned());
    c.step(0);
    c.write_failed(0);
    assert_eq!(c.state().duty, 0);
}

#[test]
fn stepping_counts_steps_within_three_periods() {
    let mut c = one(tuned());
    assert!(!c.stepping(0), "never stepped");
    c.step(0);
    assert!(c.stepping(2 * S + 999 * MS));
    assert!(!c.stepping(3 * S), "three periods without a step");
}

#[test]
fn a_disabled_controller_is_not_gated_on_stepping() {
    let mut c = one(tuned());
    c.set_enabled(false);
    assert!(c.stepping(100 * S));
}

#[test]
fn a_degraded_or_retrying_controller_still_counts_as_stepping() {
    let mut c = one(tuned());
    c.step(0);
    assert!(c.state().degraded);
    assert!(c.stepping(2 * S), "degraded still steps");

    let mut c = one(tuned());
    c.step(0);
    c.write_failed(0);
    c.step(S);
    c.write_failed(S);
    // Steps at 2 s fall in the retry wait: they are not writes, but they are steps.
    assert_eq!(c.step(2 * S), None);
    assert!(c.stepping(4 * S), "a step in the wait counts");
    assert!(!c.stepping(5 * S));
}

#[test]
fn disabling_releases_the_fan_once_and_enabling_starts_from_a_stop() {
    let mut c = one(tuned());
    c.step(0);
    c.set_enabled(false);
    assert_eq!(c.state().duty, 0);
    assert!(c.release_due(0));
    assert_eq!(c.next_due(), Some(0), "the release is due now");
    c.released();
    assert_eq!(c.next_due(), None, "nothing to do while disabled");
    assert!(!c.step_due(10 * S));

    c.set_enabled(true);
    assert_eq!(c.next_due(), Some(0));
    assert!(c.step_due(0));
}

#[test]
fn a_failed_release_is_tried_again_in_a_second() {
    let mut c = one(tuned());
    c.set_enabled(false);
    c.release_failed(5 * S);
    assert!(!c.release_due(5 * S));
    assert!(c.release_due(6 * S));
}

#[test]
fn next_due_is_the_next_step() {
    let mut c = one(tuned());
    assert_eq!(c.next_due(), Some(0));
    c.step(0);
    assert_eq!(c.next_due(), Some(S));
    // A tick that comes early is not a step.
    assert!(!c.step_due(S - 1));
    assert!(c.step_due(S));
}

#[test]
fn set_curve_replaces_one_input_in_memory_and_refuses_an_absent_one() {
    let mut c = one(tuned());
    let steeper = [
        Point {
            value: 30_000,
            duty: 300,
        },
        Point {
            value: 40_000,
            duty: 900,
        },
    ];
    c.set_curve(0, &steeper).unwrap();
    assert_eq!(c.curve(0).unwrap().eval(40_000), 900);
    assert_eq!(c.set_curve(1, &steeper), Err(Status::NOT_FOUND));
    assert_eq!(c.set_curve(0, &[]), Err(Status::INVALID_ARGUMENT));
}

#[test]
fn set_settings_refuses_a_bad_floor() {
    let mut c = one(tuned());
    assert_eq!(
        c.set_settings(settings(1001, 0, 0, 0)),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(c.settings(), tuned());
    c.set_settings(settings(400, 0, 50, 0)).unwrap();
    assert_eq!(c.settings().floor, 400);
}

#[test]
fn a_zero_max_age_is_refused_at_start() {
    let cpu = Curve::new(&CPU).unwrap();
    assert!(Control::new(tuned(), 0, [Some(cpu), None, None, None]).is_err());
}

#[test]
fn a_full_valid_config_passes_the_check() {
    let entries = [
        ("floor", ConfigValue::Int(300)),
        ("hysteresis", ConfigValue::Int(2000)),
        ("slew_per_s", ConfigValue::Int(50)),
        ("kick_ms", ConfigValue::Int(0)),
        ("max_age_ms", ConfigValue::Int(3000)),
        (
            "curve_t0",
            ConfigValue::Str("30000:300,60000:600,75000:1000"),
        ),
        ("curve_t3", ConfigValue::Str("40000:200")),
    ];
    assert_eq!(
        <crate::FanController as Driver>::check_config(&Config::new(&entries)),
        Ok(())
    );
}

#[test]
fn an_unknown_key_is_refused_and_a_near_miss_is_suggested() {
    let bogus = [("bogus", ConfigValue::Int(1))];
    let error = <crate::FanController as Driver>::check_config(&Config::new(&bogus)).unwrap_err();
    assert_eq!(error.key, "bogus");
    let near = [("flor", ConfigValue::Int(300))];
    let error = <crate::FanController as Driver>::check_config(&Config::new(&near)).unwrap_err();
    assert_eq!(error.suggestion, Some("floor"));
}

#[test]
fn a_wrong_kind_is_refused_by_name() {
    let entries = [("floor", ConfigValue::Str("300"))];
    let error = <crate::FanController as Driver>::check_config(&Config::new(&entries)).unwrap_err();
    assert_eq!(error.key, "floor");
}

#[test]
fn a_setting_out_of_range_is_refused_and_its_edges_pass() {
    for (key, value) in [
        ("floor", 1001),
        ("slew_per_s", -1),
        ("hysteresis", -1),
        ("max_age_ms", 0),
    ] {
        let entries = [(key, ConfigValue::Int(value))];
        let error =
            <crate::FanController as Driver>::check_config(&Config::new(&entries)).unwrap_err();
        assert_eq!(error.key, key);
    }
    let edges = [
        ("floor", ConfigValue::Int(1000)),
        ("max_age_ms", ConfigValue::Int(1)),
        ("hysteresis", ConfigValue::Int(0)),
    ];
    assert_eq!(
        <crate::FanController as Driver>::check_config(&Config::new(&edges)),
        Ok(())
    );
}

#[test]
fn a_curve_that_does_not_parse_or_is_not_ascending_is_refused() {
    for text in ["30000:300,30000:600", "30000"] {
        let entries = [("curve_t1", ConfigValue::Str(text))];
        let error =
            <crate::FanController as Driver>::check_config(&Config::new(&entries)).unwrap_err();
        assert_eq!(error.key, "curve_t1", "{text}");
    }
}
