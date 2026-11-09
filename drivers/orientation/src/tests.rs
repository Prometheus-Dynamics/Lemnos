//! The driver on fake devices. Each input is a slot in a [`Bench`] that holds a
//! reading, or fails while it holds none, and counts the reads it serves.

use super::*;
use crate::fusion::words::MAX_CALIBRATION_WORDS;
use lemnos_core::{ConfigValue, Encode, LocalCaller, Router, Writer};
use std::boxed::Box;
use std::format;
use std::string::{String, ToString};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
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

/// Three fake devices behind the host's router.
struct Bench {
    slots: Mutex<[Option<Vector3>; 3]>,
    reads: [AtomicU32; 3],
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

/// A bench that lives for the test (the driver borrows it for `'static`).
fn bench() -> &'static Bench {
    Box::leak(Box::new(Bench {
        slots: Mutex::new([None; 3]),
        reads: [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)],
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
}

fn vec3(x: f32, y: f32, z: f32) -> Vector3 {
    Vector3 { x, y, z }
}

/// The accelerometer's reading for a board at rest, rolled by `roll` rad.
fn tilted(roll: f32) -> Vector3 {
    vec3(0.0, G * roll.sin(), G * roll.cos())
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

/// The words of a magnetometer trained on a sphere of [`FIELD_UT`] with no
/// distortion, which the driver then uses as its applied calibration.
fn trained_mag_words() -> Vec<i32> {
    let mut m = MagCalibrator::new();
    // A uniform sphere: `z` uniform on [-1, 1] and the azimuth uniform.
    let mut seed = 12_345u32;
    let mut uniform = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1u32 << 24) as f32
    };
    let mut t = 0u64;
    for _ in 0..4500 {
        let z = 2.0 * uniform() - 1.0;
        let phi = 2.0 * core::f32::consts::PI * uniform();
        let r = (1.0 - z * z).max(0.0).sqrt();
        let b = [
            FIELD_UT * r * phi.cos(),
            FIELD_UT * r * phi.sin(),
            FIELD_UT * z,
        ];
        m.push(t, b);
        t += 10_000;
    }
    assert!(m.status().active, "training left the calibration inactive");
    let mut buf = [0i32; MAX_CALIBRATION_WORDS];
    let n = m.words(&mut buf);
    buf[..n].to_vec()
}

/// Calibration words as the configuration's comma-separated text.
fn as_text(words: &[i32]) -> String {
    words
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",")
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
fn a_rate_gyro_turns_the_yaw_by_its_rate_times_the_read_period() {
    // 101 reads at 100 Hz: 100 steps of 10 ms at 0.1 rad/s is 0.1 rad.
    let b = level_bench();
    b.set(GYRO, Some(vec3(0.0, 0.0, 0.1)));
    let mut o = driver(b, false, &[]);
    let mut last = 0.0;
    for _ in 0..101 {
        last = o.read().expect("reads").z;
    }
    assert!(
        (last - 0.1).abs() < 2e-3,
        "yaw {last} after 1 s at 0.1 rad/s"
    );
}

#[test]
fn the_configured_rate_is_the_time_step_of_each_read() {
    // The same 0.1 rad/s, read 51 times at 50 Hz: 50 steps of 20 ms, the same 0.1 rad.
    let b = level_bench();
    b.set(GYRO, Some(vec3(0.0, 0.0, 0.1)));
    let mut o = driver(b, false, &[("sample_rate_hz", ConfigValue::Int(50))]);
    let mut last = 0.0;
    for _ in 0..51 {
        last = o.read().expect("reads").z;
    }
    assert!((last - 0.1).abs() < 2e-3, "yaw {last} at 50 Hz");
}

#[test]
fn a_failed_read_reports_and_the_filter_resumes_where_it_stopped() {
    // Reference: a bench that never fails, read at each successful step k.
    let inputs = |k: u32| {
        let k = k as f32;
        (
            vec3(0.2 * (0.07 * k).sin(), 0.1, G),
            vec3(0.0, 0.05 * (0.05 * k).sin(), 0.15),
        )
    };
    let steps = 60;
    let reference = bench();
    let mut expected = Vec::new();
    let mut r = driver(reference, false, &[]);
    for k in 0..steps {
        let (a, g) = inputs(k);
        reference.set(ACCEL, Some(a));
        reference.set(GYRO, Some(g));
        expected.push(r.read().expect("reads"));
    }

    // The same inputs, but the gyroscope fails on five attempts in the middle.
    let b = bench();
    let mut o = driver(b, false, &[]);
    let (mut k, mut attempt, mut failures) = (0u32, 0u32, 0u32);
    while k < steps {
        let (a, g) = inputs(k);
        let fail = (20..25).contains(&attempt);
        b.set(ACCEL, Some(a));
        b.set(GYRO, if fail { None } else { Some(g) });
        match o.read() {
            Ok(got) => {
                assert!(!fail, "a read with a failed gyroscope succeeded");
                let want = expected[k as usize];
                assert!(
                    (got.x - want.x).abs() < 1e-6
                        && (got.y - want.y).abs() < 1e-6
                        && (got.z - want.z).abs() < 1e-6,
                    "step {k}: {got:?} != {want:?}"
                );
                k += 1;
            }
            Err(status) => {
                assert!(fail, "step {k} failed without a fault: {status:?}");
                failures += 1;
            }
        }
        attempt += 1;
    }
    assert_eq!(failures, 5);
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
fn an_untrusted_magnetometer_gives_no_heading_and_says_so() {
    // Factory calibration: the magnetometer is not trusted, so it does not set
    // the heading, and the health reports that it is ignored.
    let b = level_bench();
    let (h, v) = (FIELD_UT * DIP.cos(), FIELD_UT * DIP.sin());
    let psi = 0.5f32;
    b.set(
        MAG,
        Some(vec3(h * psi.cos() * 1e-6, -h * psi.sin() * 1e-6, -v * 1e-6)),
    );
    let mut o = driver(b, true, &[("mode", ConfigValue::Str("9axis"))]);
    let r = o.read().expect("reads");
    assert!(r.z.abs() < 1e-3, "yaw {} without a trusted field", r.z);
    assert_eq!(
        o.health(),
        Health::Degraded("magnetometer ignored: disturbed or uncalibrated")
    );
}

#[test]
fn a_calibrated_magnetometer_sets_the_heading_from_the_field() {
    // A board yawed by `psi` sees the field as (h cos psi, -h sin psi, -v) in
    // its own axes, with h and v the horizontal and vertical parts.
    let words = trained_mag_words();
    let text = as_text(&words);
    let b = level_bench();
    let psi = 0.5f32;
    let (h, v) = (FIELD_UT * DIP.cos(), FIELD_UT * DIP.sin());
    b.set(
        MAG,
        Some(vec3(h * psi.cos() * 1e-6, -h * psi.sin() * 1e-6, -v * 1e-6)),
    );
    let mut o = driver(
        b,
        true,
        &[
            ("mode", ConfigValue::Str("9axis")),
            ("mag_calibration", ConfigValue::Str(&text)),
        ],
    );
    let r = o.read().expect("reads");
    assert!((r.z - psi).abs() < 0.02, "heading {} for {psi}", r.z);
    assert_eq!(o.health(), Health::Ok);
}

#[test]
fn a_constant_gyro_offset_on_a_still_board_is_learned_away() {
    // 30 s at 100 Hz with a 0.02 rad/s offset on a still, level board: left
    // alone the yaw would reach 0.6 rad.
    let b = level_bench();
    b.set(GYRO, Some(vec3(0.0, 0.0, 0.02)));
    let mut o = driver(b, false, &[]);
    let mut last = 0.0;
    for _ in 0..3000 {
        last = o.read().expect("reads").z;
    }
    assert!(
        last.abs() < 0.15,
        "yaw {last} after 30 s of a 0.02 rad/s offset"
    );
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
fn bad_settings_are_refused_at_bind() {
    let refuse =
        |entries: &[(&str, ConfigValue<'_>)]| start(level_bench(), false, entries).is_err();
    assert!(refuse(&[("sample_rate_hz", ConfigValue::Float(1.0))]));
    assert!(refuse(&[("sample_rate_hz", ConfigValue::Int(5000))]));
    assert!(refuse(&[("sample_rate_hz", ConfigValue::Str("fast"))]));
    assert!(refuse(&[("mode", ConfigValue::Str("3axis"))]));
    assert!(refuse(&[("algorithm", ConfigValue::Str("ekf"))]));
    assert!(refuse(&[("kp", ConfigValue::Str("high"))]));
    assert!(refuse(&[("imu_calibration", ConfigValue::Str("1,x"))]));
    let too_many = as_text(&[1i32; MAX_CALIBRATION_WORDS + 1]);
    assert!(refuse(&[("imu_calibration", ConfigValue::Str(&too_many))]));
    assert!(refuse(&[("sample_rate_hz", ConfigValue::Float(9.0))]));
    assert!(!refuse(&[("sample_rate_hz", ConfigValue::Float(10.0))]));
    assert!(!refuse(&[("sample_rate_hz", ConfigValue::Int(1000))]));
}

#[test]
fn calibration_words_the_layout_refuses_leave_the_factory_state_and_say_so() {
    let b = level_bench();
    let refused = driver(b, false, &[("imu_calibration", ConfigValue::Str("9,1,0"))]);
    assert_eq!(
        refused.health(),
        Health::Degraded("calibration words refused: factory calibration")
    );

    // The factory state's own words load.
    let mut words = [0i32; MAX_CALIBRATION_WORDS];
    let n = ImuCalibrator::new().words(&mut words);
    let text = as_text(&words[..n]);
    let accepted = driver(b, false, &[("imu_calibration", ConfigValue::Str(&text))]);
    assert_eq!(
        accepted.health(),
        Health::Degraded("six-axis: yaw is relative")
    );
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
