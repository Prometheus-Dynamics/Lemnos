//! The arithmetic on its own: duty mapping, the PWM's high time, the retry wait and the
//! tachometer's maths, including the gap rule.

use super::math::{
    FULL, RETRY_CAP_NS, STALL_NS, Tach, backoff_ns, duty_ns, hardware_duty, period_ns,
    rpm_from_interval,
};
use lemnos_hal::raw::Edge;

fn rising(seq: u32, at_ns: u64) -> Edge {
    Edge {
        rising: true,
        timestamp_ns: at_ns,
        seq,
    }
}

fn falling(seq: u32, at_ns: u64) -> Edge {
    Edge {
        rising: false,
        timestamp_ns: at_ns,
        seq,
    }
}

const MS: u64 = 1_000_000;

#[test]
fn a_request_is_the_duty_unless_it_is_below_the_minimum() {
    assert_eq!(
        hardware_duty(0, 200),
        0,
        "zero stays zero, so the fan can stop"
    );
    assert_eq!(hardware_duty(50, 200), 200, "raised to the minimum");
    assert_eq!(hardware_duty(200, 200), 200);
    assert_eq!(hardware_duty(700, 200), 700);
    assert_eq!(hardware_duty(FULL, 200), FULL);
    assert_eq!(hardware_duty(900, 1000), FULL, "never above full");
}

#[test]
fn the_pwm_high_time_is_that_share_of_the_period() {
    let period = period_ns(25_000);
    assert_eq!(period, 40_000);
    assert_eq!(duty_ns(period, 0), 0);
    assert_eq!(duty_ns(period, 500), 20_000);
    assert_eq!(duty_ns(period, FULL), period);
    assert_eq!(duty_ns(period, 1), 40, "one per mille of 40 us");
    assert_eq!(duty_ns(period, 2000), period, "clamped to the period");
    // A period that does not divide evenly rounds down, never past the period.
    assert_eq!(duty_ns(1_000_003, 333), 333_000);
}

#[test]
fn the_retry_wait_doubles_from_one_second_and_stops_at_eight() {
    let waits: Vec<u64> = (1..=6).map(backoff_ns).collect();
    let s = 1_000_000_000;
    assert_eq!(waits, [s, 2 * s, 4 * s, 8 * s, 8 * s, 8 * s]);
    assert_eq!(RETRY_CAP_NS, 8 * s);
    assert_eq!(
        backoff_ns(u32::MAX),
        RETRY_CAP_NS,
        "a long run of failures stays at the cap"
    );
}

#[test]
fn revolutions_come_from_the_pulse_interval_and_the_pulses_per_revolution() {
    // A pulse every 5 ms, two pulses to a revolution: 100 revolutions a second.
    assert_eq!(rpm_from_interval(5 * MS, 2), 6000);
    // One pulse to a revolution, every 20 ms: 50 a second.
    assert_eq!(rpm_from_interval(20 * MS, 1), 3000);
    assert_eq!(rpm_from_interval(0, 2), 0, "no interval, no speed");
}

#[test]
fn a_tachometer_reads_from_the_last_two_rising_edges() {
    let mut tach = Tach::new(2);
    assert_eq!(tach.rpm(0), 0, "no pulse yet");
    tach.edge(rising(0, 0));
    assert_eq!(tach.rpm(MS), 0, "one pulse is not an interval");
    tach.edge(falling(1, 2 * MS));
    tach.edge(rising(2, 5 * MS));
    assert_eq!(tach.rpm(5 * MS), 6000);
}

#[test]
fn falling_edges_do_not_count_as_pulses() {
    let mut tach = Tach::new(2);
    tach.edge(falling(0, 0));
    tach.edge(falling(1, 5 * MS));
    tach.edge(falling(2, 10 * MS));
    assert_eq!(tach.rpm(10 * MS), 0);
}

#[test]
fn a_tachometer_reads_zero_once_its_pulses_stop() {
    let mut tach = Tach::new(2);
    tach.edge(rising(0, 0));
    tach.edge(falling(1, MS));
    tach.edge(rising(2, 5 * MS));
    // Within the stall window it still reads the speed.
    assert_eq!(tach.rpm(5 * MS + STALL_NS), 6000);
    assert_eq!(tach.rpm(5 * MS + STALL_NS + 1), 0);
}

#[test]
fn the_stall_window_is_twice_the_interval_when_that_is_longer() {
    // A slow fan: one pulse every 2 s. The window is 4 s, not 1 s.
    let mut tach = Tach::new(1);
    tach.edge(rising(0, 0));
    tach.edge(rising(1, 2_000 * MS));
    assert_eq!(tach.rpm(2_000 * MS + 3_900 * MS), 30);
    assert_eq!(tach.rpm(2_000 * MS + 4_100 * MS), 0);
}

#[test]
fn an_interval_across_lost_edges_is_not_used() {
    let mut tach = Tach::new(2);
    tach.edge(rising(0, 0));
    tach.edge(falling(1, MS));
    // Sequence 2 never arrives: the kernel's queue overflowed. The rising edge at 5 ms follows
    // a gap, so no interval is taken from it.
    tach.edge(rising(3, 5 * MS));
    assert_eq!(tach.rpm(5 * MS), 0);
    // The next pair, in sequence, gives a reading again.
    tach.edge(falling(4, 6 * MS));
    tach.edge(rising(5, 10 * MS));
    assert_eq!(tach.rpm(10 * MS), 6000);
}

#[test]
fn sequence_numbers_are_checked_across_the_wrap() {
    let mut tach = Tach::new(2);
    tach.edge(rising(u32::MAX, 0));
    tach.edge(rising(0, 5 * MS));
    assert_eq!(tach.rpm(5 * MS), 6000);
}

#[test]
fn the_board_gives_the_speed_range_and_the_stall_limit_to_the_shape() {
    use lemnos_core::{Config, ConfigValue};
    let entries = [
        ("max_rpm", ConfigValue::Int(3000)),
        ("warn_below_rpm", ConfigValue::Int(200)),
    ];
    let shape = super::shape(&Config::new(&entries)).unwrap();
    assert_eq!(
        (shape.max_rpm, shape.warn_below_rpm),
        (Some(3000), Some(200))
    );
    let bare = super::shape(&Config::new(&[])).unwrap();
    assert_eq!((bare.max_rpm, bare.warn_below_rpm), (None, None));
    let negative = [("max_rpm", ConfigValue::Int(-5))];
    assert!(super::shape(&Config::new(&negative)).is_err());
}
