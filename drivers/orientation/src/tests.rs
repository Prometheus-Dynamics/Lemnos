//! The driver on fake devices. Each input is a slot in a [`Bench`] that holds a
//! reading, or fails while it holds none, and counts the reads it serves. The
//! bench's host clock is the sim clock: a test moves time by hand, so the same
//! reads always give the same answers.

use super::*;
use lemnos_core::{ConfigValue, Encode, HostClock, HostClockHandle, LocalCaller, Router, Writer};
use lemnos_sim::SimClock;
use std::boxed::Box;
use std::format;
use std::time::Duration;

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::vec;
use std::vec::Vec;

/// The router's device numbers.
const ACCEL: u32 = 0;
const GYRO: u32 = 1;
const MAG: u32 = 2;
/// Standard gravity, m/s².
const G: f32 = 9.80665;
/// The Earth's field for the heading tests: 50 µT at a 60° dip.
const FIELD_UT: f32 = 50.0;
const DIP: f32 = 60.0 * core::f32::consts::PI / 180.0;
/// One millisecond, in nanoseconds.
const MS: u64 = 1_000_000;
/// The tolerance on an angle the time tests check, rad (0.006 degrees). A constant turn's error
/// is the first-order step's own: (ω·dt)³/12 per step, about 1e-5 rad over two seconds at 50 Hz.
const TURN_TOL: f32 = 1e-4;

/// Three fake devices behind the host's router, and the host clock.
struct Bench {
    slots: Mutex<[Option<Vector3>; 3]>,
    reads: [AtomicU32; 3],
    clock: SimClock,
}

impl Router for Bench {
    fn call(
        &self,
        device: u32,
        _method: MethodId,
        _args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize> {
        let slot = device as usize;
        let value = self.slots.lock().unwrap()[slot].ok_or(Status::TIMED_OUT)?;
        self.reads[slot].fetch_add(1, Ordering::SeqCst);
        let mut out = Writer::new(reply);
        value.encode(&mut out)?;
        Ok(out.len())
    }

    fn health(&self, _device: u32) -> Health {
        Health::Ok
    }
}

impl HostClock for Bench {
    fn now_ns(&self) -> u64 {
        self.clock.now_ns()
    }
}

/// A bench that lives for the test (the driver borrows it for `'static`).
fn bench() -> &'static Bench {
    Box::leak(Box::new(Bench {
        slots: Mutex::new([None; 3]),
        reads: [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)],
        clock: SimClock::new(),
    }))
}

impl Bench {
    /// Sets a device's reading, or `None` to make its reads fail.
    fn set(&self, device: u32, value: Option<Vector3>) {
        self.slots.lock().unwrap()[device as usize] = value;
    }

    fn reads(&self, device: u32) -> u32 {
        self.reads[device as usize].load(Ordering::SeqCst)
    }

    /// Moves the clock to `ns`. The sim clock only moves forward.
    fn move_to(&self, ns: u64) {
        let now = self.clock.now_ns();
        assert!(ns >= now, "the clock went back: {now} to {ns}");
        self.clock.advance(Duration::from_nanos(ns - now));
    }
}

fn vec3(x: f32, y: f32, z: f32) -> Vector3 {
    Vector3 { x, y, z }
}

/// The accelerometer's reading for a board at rest, rolled by `roll` rad.
fn tilted(roll: f32) -> Vector3 {
    vec3(0.0, G * roll.sin(), G * roll.cos())
}

/// The accelerometer's reading for a board at rest with roll and pitch, rad.
fn posed(roll: f32, pitch: f32) -> Vector3 {
    vec3(
        -G * pitch.sin(),
        G * roll.sin() * pitch.cos(),
        G * roll.cos() * pitch.cos(),
    )
}

/// A driver bound to `bench`, with a magnetometer when `mag` is set.
fn start(
    bench: &'static Bench,
    mag: bool,
    entries: &[(&str, ConfigValue<'_>)],
) -> Result<Orientation> {
    let needs = OrientationNeeds {
        accel: Dev::new(LocalCaller::new(bench, ACCEL, 0)),
        gyro: Dev::new(LocalCaller::new(bench, GYRO, 0)),
        mag: mag.then(|| Dev::new(LocalCaller::new(bench, MAG, 0))),
        clock: HostClockHandle::shared(bench),
    };
    Orientation::start(needs, &Config::new(entries))
}

/// [`start`] for a configuration that must be accepted.
fn driver(bench: &'static Bench, mag: bool, entries: &[(&str, ConfigValue<'_>)]) -> Orientation {
    match start(bench, mag, entries) {
        Ok(o) => o,
        Err(status) => panic!("the driver was refused: {status:?}"),
    }
}

/// A board at rest, level, with no magnetometer reading.
fn level_bench() -> &'static Bench {
    let b = bench();
    b.set(ACCEL, Some(vec3(0.0, 0.0, G)));
    b.set(GYRO, Some(vec3(0.0, 0.0, 0.0)));
    b
}

/// One read at `t` ns, with the given inputs.
fn read_at(b: &Bench, o: &mut Orientation, t: u64, accel: Vector3, gyro: Vector3) -> Vector3 {
    b.move_to(t);
    b.set(ACCEL, Some(accel));
    b.set(GYRO, Some(gyro));
    o.read().expect("the read reads")
}

/// Reads at each instant in `times` (ns, ascending) while the board turns about its x axis at
/// `rate` rad/s from level at time zero. Returns the last reading.
fn turn_x(b: &Bench, o: &mut Orientation, rate: f32, times: &[u64]) -> Vector3 {
    let mut last = vec3(0.0, 0.0, 0.0);
    for &t in times {
        let roll = rate * (t as f32 * 1e-9);
        last = read_at(b, o, t, tilted(roll), vec3(rate, 0.0, 0.0));
    }
    last
}

/// Read instants every `step_ns` from zero to `end_ns`, both included.
fn regular(step_ns: u64, end_ns: u64) -> Vec<u64> {
    (0..=end_ns / step_ns).map(|i| i * step_ns).collect()
}

/// Read instants from zero to `end_ns` with intervals of 2 to 18 ms (10 ms on average), from a
/// fixed sequence. The last read lands on `end_ns`.
fn jittered(end_ns: u64) -> Vec<u64> {
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut t = 0;
    let mut out = vec![0];
    while t < end_ns {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let step = (2 + (seed >> 33) % 17) * MS;
        t = (t + step).min(end_ns);
        out.push(t);
    }
    out
}

#[test]
fn a_level_board_at_rest_reads_zero_angles() {
    let b = level_bench();
    let mut o = driver(b, false, &[]);
    for _ in 0..10 {
        let r = o.read().expect("a level board reads");
        assert!(r.x.abs() < 1e-3, "roll {}", r.x);
        assert!(r.y.abs() < 1e-3, "pitch {}", r.y);
        assert!(r.z.abs() < 1e-3, "yaw {}", r.z);
    }
    assert_eq!(b.reads(ACCEL), 10);
    assert_eq!(b.reads(GYRO), 10);
    assert_eq!(o.health(), Health::Degraded("six-axis: yaw is relative"));
}

#[test]
fn the_first_read_starts_from_the_tilt_the_accelerometer_shows() {
    let b = bench();
    b.set(ACCEL, Some(tilted(0.5)));
    b.set(GYRO, Some(vec3(0.0, 0.0, 0.0)));
    let mut o = driver(b, false, &[]);
    let r = o.read().expect("the first read reads");
    assert!((r.x - 0.5).abs() < 1e-3, "roll {}", r.x);
    assert!(r.y.abs() < 1e-3, "pitch {}", r.y);
}

#[test]
fn the_first_read_takes_the_tilt_and_integrates_no_time() {
    // The clock is at 7 s when the first read comes, and the board is turning about z at 1 rad/s.
    // The first read starts the filter, so it shows no yaw. The next read, 50 ms later, integrates
    // those 50 ms. (A 50 ms step is 0.05 rad at this rate; the integrator's first-order error on
    // it is (ω·dt)³/12, about 1e-5 rad, inside TURN_TOL.)
    let b = level_bench();
    b.set(GYRO, Some(vec3(0.0, 0.0, 1.0)));
    b.move_to(7_000 * MS);
    let mut o = driver(b, false, &[]);
    let first = o.read().expect("the first read reads");
    assert!(
        first.z.abs() < 1e-6,
        "the first read integrated: yaw {}",
        first.z
    );
    let second = read_at(
        b,
        &mut o,
        7_050 * MS,
        vec3(0.0, 0.0, G),
        vec3(0.0, 0.0, 1.0),
    );
    assert!(
        (second.z - 0.05).abs() < TURN_TOL,
        "yaw {} after 50 ms at 1 rad/s",
        second.z
    );
}

#[test]
fn a_rate_gyro_turns_the_yaw_by_its_rate_times_the_time_between_reads() {
    // 101 reads at 100 Hz over 1 s: 0.1 rad/s for 1 s is 0.1 rad.
    let b = level_bench();
    b.set(GYRO, Some(vec3(0.0, 0.0, 0.1)));
    let mut o = driver(b, false, &[]);
    let mut last = 0.0;
    for t in regular(10 * MS, 1_000 * MS) {
        b.move_to(t);
        last = o.read().expect("reads").z;
    }
    assert!(
        (last - 0.1).abs() < TURN_TOL,
        "yaw {last} after 1 s at 0.1 rad/s"
    );
}

#[test]
fn a_constant_turn_reads_its_analytic_angle_at_any_read_rate() {
    // 0.5 rad/s for 2 s is one radian of roll. Read every 20 ms (50 Hz) and every 5 ms (200 Hz).
    let end = 2_000 * MS;
    let mut rolls = Vec::new();
    for step in [20 * MS, 5 * MS] {
        let b = level_bench();
        let mut o = driver(b, false, &[]);
        let r = turn_x(b, &mut o, 0.5, &regular(step, end));
        assert!(
            (r.x - 1.0).abs() < TURN_TOL,
            "roll {} at {} ms reads, want 1.0",
            r.x,
            step / MS
        );
        rolls.push(r.x);
    }
    assert!(
        (rolls[0] - rolls[1]).abs() < TURN_TOL,
        "50 Hz {} and 200 Hz {}",
        rolls[0],
        rolls[1]
    );
}

#[test]
fn a_tilt_held_still_reads_the_same_tilt_at_any_read_rate() {
    // Held still, the attitude is the first read's tilt at every rate. Nothing turns, so a read
    // rate cannot change it: this passes on any clock, and it checks the first read's tilt.
    for step in [20 * MS, 5 * MS] {
        let b = bench();
        b.set(ACCEL, Some(posed(0.3, -0.2)));
        b.set(GYRO, Some(vec3(0.0, 0.0, 0.0)));
        let mut o = driver(b, false, &[]);
        let mut r = vec3(0.0, 0.0, 0.0);
        for t in regular(step, 2_000 * MS) {
            b.move_to(t);
            r = o.read().expect("reads");
        }
        assert!(
            (r.x - 0.3).abs() < TURN_TOL,
            "roll {} at {} ms",
            r.x,
            step / MS
        );
        assert!(
            (r.y + 0.2).abs() < TURN_TOL,
            "pitch {} at {} ms",
            r.y,
            step / MS
        );
    }
}

#[test]
fn irregular_read_intervals_give_the_answer_of_regular_ones() {
    let end = 2_000 * MS;
    let b1 = bench();
    let mut o1 = driver(b1, false, &[]);
    let regular_r = turn_x(b1, &mut o1, 0.5, &regular(10 * MS, end));

    let times = jittered(end);
    let gaps: Vec<u64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        gaps.iter().any(|&g| g != gaps[0]),
        "the intervals are not irregular"
    );
    let b2 = bench();
    let mut o2 = driver(b2, false, &[]);
    let jitter_r = turn_x(b2, &mut o2, 0.5, &times);

    assert!(
        (jitter_r.x - regular_r.x).abs() < TURN_TOL,
        "jittered roll {} against regular {}",
        jitter_r.x,
        regular_r.x
    );
    assert!((jitter_r.x - 1.0).abs() < TURN_TOL, "roll {}", jitter_r.x);
}

#[test]
fn a_gap_within_the_bound_integrates_its_whole_length() {
    // Yaw alone: the accelerometer is level, so only the gyroscope moves the heading. The clock
    // reads 0.4 s after the last read, under the 0.5 s bound: the step is integrated in full.
    let b = level_bench();
    b.set(GYRO, Some(vec3(0.0, 0.0, 0.2)));
    let mut o = driver(b, false, &[]);
    for t in regular(10 * MS, 1_000 * MS) {
        b.move_to(t);
        o.read().expect("reads");
    }
    let r = read_at(
        b,
        &mut o,
        1_400 * MS,
        vec3(0.0, 0.0, G),
        vec3(0.0, 0.0, 0.2),
    );
    assert!(
        (r.z - 0.28).abs() < TURN_TOL,
        "yaw {} after a 0.4 s gap",
        r.z
    );
}

#[test]
fn a_gap_beyond_the_bound_is_held_and_steps_resume_after_it() {
    // A 2 s gap at 0.2 rad/s: the gap is not integrated, so the heading does not move on the read
    // that ends it. The reads after it integrate again, from that read on.
    let b = level_bench();
    b.set(GYRO, Some(vec3(0.0, 0.0, 0.2)));
    let mut o = driver(b, false, &[]);
    let mut before = 0.0;
    for t in regular(10 * MS, 1_000 * MS) {
        b.move_to(t);
        before = o.read().expect("reads").z;
    }
    let at_gap = read_at(
        b,
        &mut o,
        3_000 * MS,
        vec3(0.0, 0.0, G),
        vec3(0.0, 0.0, 0.2),
    );
    assert!(
        (at_gap.z - before).abs() < 1e-6,
        "the gap was integrated: yaw {} after {}",
        at_gap.z,
        before
    );
    let next = read_at(
        b,
        &mut o,
        3_010 * MS,
        vec3(0.0, 0.0, G),
        vec3(0.0, 0.0, 0.2),
    );
    assert!(
        (next.z - before - 0.002).abs() < TURN_TOL,
        "yaw {} after one 10 ms step",
        next.z - before
    );
}

#[test]
fn after_a_gap_the_tilt_reconverges_from_the_accelerometer_without_a_jump() {
    // Level and still, then the board is rolled by 0.4 rad during a 2 s gap. The read that ends
    // the gap does not move the attitude. Reads at 50 Hz after it pull the tilt to the accelerometer
    // (0.4 rad): within 0.01 rad by 3 s, and within 0.02 rad by 10 s. The integral term (ki) makes
    // the approach overshoot by about 0.015 rad for a step this size, and that tail settles slowly;
    // the measured curve is 0.004 rad at 3 s, -0.0145 at 5 s, -0.014 at 10 s.
    let b = level_bench();
    let mut o = driver(b, false, &[]);
    for t in regular(10 * MS, 1_000 * MS) {
        b.move_to(t);
        o.read().expect("reads");
    }
    let at_gap = read_at(b, &mut o, 3_000 * MS, posed(0.4, 0.0), vec3(0.0, 0.0, 0.0));
    assert!(
        at_gap.x.abs() < 1e-6,
        "the gap moved the roll to {}",
        at_gap.x
    );
    let mut at_3s = None;
    let mut r = at_gap;
    for t in regular(20 * MS, 10_000 * MS)
        .into_iter()
        .filter(|&t| t > 3_000 * MS)
    {
        r = read_at(b, &mut o, t, posed(0.4, 0.0), vec3(0.0, 0.0, 0.0));
        if t == 6_000 * MS {
            at_3s = Some(r.x);
        }
    }
    let at_3s = at_3s.expect("a read at 6 s");
    assert!(
        (at_3s - 0.4).abs() < 0.01,
        "roll {at_3s} at 3 s after the gap, want 0.4"
    );
    assert!(
        (r.x - 0.4).abs() < 0.02,
        "roll {} at 10 s after the gap, want 0.4",
        r.x
    );
}

#[test]
fn a_failed_read_leaves_the_attitude_and_the_next_read_integrates_its_time() {
    // Reads every 10 ms of a steady turn. Five attempts in the middle fail; each still takes its
    // 10 ms of clock. The read that follows integrates the whole 60 ms, so the final roll is the
    // analytic angle.
    let rate = 0.5;
    let b = bench();
    let mut o = driver(b, false, &[]);
    let mut last = None;
    for attempt in 0..100u64 {
        let t = attempt * 10 * MS;
        let fail = (20..25).contains(&attempt);
        b.move_to(t);
        b.set(ACCEL, Some(tilted(rate * (t as f32 * 1e-9))));
        b.set(
            GYRO,
            if fail {
                None
            } else {
                Some(vec3(rate, 0.0, 0.0))
            },
        );
        match o.read() {
            Ok(r) => {
                assert!(!fail, "a read with a failed gyroscope succeeded");
                last = Some(r);
            }
            Err(status) => {
                assert!(fail, "attempt {attempt} failed without a fault: {status:?}");
                assert_eq!(status, Status::TIMED_OUT);
            }
        }
    }
    let r = last.expect("the reads succeeded");
    assert!(
        (r.x - rate * 0.99).abs() < TURN_TOL,
        "roll {} at 0.99 s",
        r.x
    );
}

#[test]
fn the_configured_sample_rate_does_not_change_the_attitude() {
    // The same 1 s turn, read every 5 ms: a driver configured at 50 Hz and one at 200 Hz must
    // agree, and both must be the analytic 0.5 rad.
    let end = 1_000 * MS;
    let mut rolls = Vec::new();
    for hz in [50, 200] {
        let b = bench();
        let mut o = driver(b, false, &[("sample_rate_hz", ConfigValue::Int(hz))]);
        rolls.push(turn_x(b, &mut o, 0.5, &regular(5 * MS, end)).x);
    }
    assert!(
        (rolls[0] - rolls[1]).abs() < TURN_TOL,
        "50 Hz config {} against 200 Hz config {}",
        rolls[0],
        rolls[1]
    );
    assert!((rolls[1] - 0.5).abs() < TURN_TOL, "roll {}", rolls[1]);
}

#[test]
fn nine_axis_needs_a_magnetometer_to_bind() {
    assert!(start(level_bench(), false, &[("mode", ConfigValue::Str("9axis"))]).is_err());
    assert!(start(level_bench(), true, &[("mode", ConfigValue::Str("9axis"))]).is_ok());
}

#[test]
fn a_six_axis_device_does_not_read_its_magnetometer() {
    let b = level_bench();
    b.set(MAG, Some(vec3(2e-5, 0.0, -4e-5)));
    let mut o = driver(b, true, &[("mode", ConfigValue::Str("6axis"))]);
    for _ in 0..5 {
        o.read().expect("reads");
    }
    assert_eq!(b.reads(MAG), 0);
}

#[test]
fn a_wired_magnetometer_is_used_unless_the_mode_says_otherwise() {
    let b = level_bench();
    b.set(MAG, Some(vec3(2e-5, 0.0, -4e-5)));
    let mut o = driver(b, true, &[]);
    for _ in 0..5 {
        o.read().expect("a level board reads");
    }
    assert_eq!(b.reads(MAG), 5);
}

#[test]
fn a_magnetometer_sets_the_heading_from_the_field() {
    // A board yawed by `psi` sees the field as (h cos psi, -h sin psi, -v) in
    // its own axes, with h and v the horizontal and vertical parts.
    let b = level_bench();
    let psi = 0.5f32;
    let (h, v) = (FIELD_UT * DIP.cos(), FIELD_UT * DIP.sin());
    b.set(
        MAG,
        Some(vec3(h * psi.cos() * 1e-6, -h * psi.sin() * 1e-6, -v * 1e-6)),
    );
    let mut o = driver(b, true, &[("mode", ConfigValue::Str("9axis"))]);
    let r = o.read().expect("reads");
    assert!((r.z - psi).abs() < 0.02, "heading {} for {psi}", r.z);
    assert_eq!(o.health(), Health::Ok);
}

#[test]
fn a_failing_accelerometer_is_reported_on_each_call_until_it_returns() {
    let b = level_bench();
    let mut o = driver(b, false, &[]);
    o.read().expect("reads");
    b.set(ACCEL, None);
    for _ in 0..3 {
        assert_eq!(o.read(), Err(Status::TIMED_OUT));
    }
    b.set(ACCEL, Some(vec3(0.0, 0.0, G)));
    assert!(o.read().is_ok());
}

#[test]
fn bad_settings_are_refused_before_any_device_is_made() {
    let refuse = |entries: &[(&str, ConfigValue<'_>)]| {
        Orientation::check_config(&Config::new(entries)).is_err()
    };
    assert!(refuse(&[("sample_rate_hz", ConfigValue::Float(1.0))]));
    assert!(refuse(&[("sample_rate_hz", ConfigValue::Int(5000))]));
    assert!(refuse(&[("sample_rate_hz", ConfigValue::Str("fast"))]));
    assert!(refuse(&[("mode", ConfigValue::Str("3axis"))]));
    assert!(refuse(&[("algorithm", ConfigValue::Str("ekf"))]));
    assert!(refuse(&[("kp", ConfigValue::Str("high"))]));
    assert!(refuse(&[("sample_rate_hz", ConfigValue::Float(9.0))]));
    assert!(!refuse(&[("sample_rate_hz", ConfigValue::Float(10.0))]));
    assert!(!refuse(&[("sample_rate_hz", ConfigValue::Int(1000))]));
}

#[test]
fn check_config_accepts_a_full_valid_config() {
    let full = [
        ("sample_rate_hz", ConfigValue::Float(100.0)),
        ("mode", ConfigValue::Str("9axis")),
        ("algorithm", ConfigValue::Str("madgwick")),
        ("kp", ConfigValue::Float(1.0)),
        ("ki", ConfigValue::Float(0.05)),
        ("beta", ConfigValue::Float(0.1)),
        ("dip_deg", ConfigValue::Float(60.0)),
        ("mount_roll_deg", ConfigValue::Int(0)),
        ("mount_pitch_deg", ConfigValue::Float(0.0)),
        ("mount_yaw_deg", ConfigValue::Float(0.0)),
        ("declination_deg", ConfigValue::Float(-2.0)),
    ];
    assert!(Orientation::check_config(&Config::new(&full)).is_ok());
}

#[test]
fn check_config_names_an_unknown_key_and_suggests_the_near_one() {
    let bogus = [("bogus", ConfigValue::Int(1))];
    let err = Orientation::check_config(&Config::new(&bogus)).unwrap_err();
    assert_eq!(err.key, "bogus");
    let near = [("sample_rat_hz", ConfigValue::Float(50.0))];
    let err = Orientation::check_config(&Config::new(&near)).unwrap_err();
    assert_eq!(err.suggestion, Some("sample_rate_hz"));
}

#[test]
fn describes_roll_pitch_and_yaw_in_radians() {
    let b = level_bench();
    let o = driver(b, false, &[]);
    let mut d = Description::default();
    o.describe(&mut d);
    let names: Vec<&str> = d.items().map(|i| i.name).collect();
    assert_eq!(names, ["roll", "pitch", "yaw"]);
    let _ = format!("{names:?}");
}
