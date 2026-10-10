#![allow(clippy::needless_range_loop)]

use super::*;
use crate::math::{abs, norm, sub};
use crate::testutil::{Rng, random_unit};

const G: f32 = STANDARD_GRAVITY;
const ACCEL_OFFSET: Vec3 = [0.3, -0.2, 0.1];
const ACCEL_SCALE: Vec3 = [1.02, 0.98, 1.01];

/// The raw accelerometer reading for a true specific force `a`.
fn raw_accel(a: Vec3) -> Vec3 {
    [
        ACCEL_SCALE[0] * a[0] + ACCEL_OFFSET[0],
        ACCEL_SCALE[1] * a[1] + ACCEL_OFFSET[1],
        ACCEL_SCALE[2] * a[2] + ACCEL_OFFSET[2],
    ]
}

/// Raw magnetometer reading (µT) for a true field `b`: soft iron then hard iron.
fn raw_mag(b: Vec3) -> Vec3 {
    let s = [[1.1, 0.05, 0.05], [0.05, 0.95, 0.05], [0.05, 0.05, 1.05]];
    let mut out = [0.0_f32; 3];
    for (o, row) in out.iter_mut().zip(s.iter()) {
        *o = row[0] * b[0] + row[1] * b[1] + row[2] * b[2];
    }
    [out[0] + 10.0, out[1] - 5.0, out[2] + 20.0]
}

/// Holds still on `a` for `secs` at 100 Hz from `t0`. Returns the end time.
fn hold(c: &mut ImuCalibrator, t0: u64, secs: f32, a: Vec3) -> u64 {
    let n = (secs * 100.0) as u64;
    for k in 0..n {
        c.push(t0 + k * 10_000, raw_accel(a), [0.0; 3]);
    }
    t0 + n * 10_000
}

/// Moves for `n` samples (gyro far above the quasi-static limit).
fn moving(c: &mut ImuCalibrator, t0: u64, n: u64) -> u64 {
    for k in 0..n {
        c.push(t0 + k * 10_000, [3.0, -2.0, 5.0], [1.0, 0.5, -0.7]);
    }
    t0 + n * 10_000
}

#[test]
fn accel_six_routine_produces_a_candidate_only_after_apply() {
    let mut c = ImuCalibrator::new();
    c.start(Routine::AccelSix);
    let faces: [Vec3; 6] = [
        [G, 0.0, 0.0],
        [-G, 0.0, 0.0],
        [0.0, G, 0.0],
        [0.0, -G, 0.0],
        [0.0, 0.0, G],
        [0.0, 0.0, -G],
    ];
    let mut t = 0u64;
    for face in faces.iter() {
        t = hold(&mut c, t, 2.5, *face);
        t = moving(&mut c, t, 20);
    }
    assert!(c.candidate(), "routine should finish after six faces");
    assert!(!c.failed());
    assert_eq!(c.running(), None);
    // Not applied yet: the correction is still the factory identity.
    assert!(norm(sub(c.accel().center, [0.0; 3])) < 1e-6);
    assert!(c.apply());
    assert!(!c.candidate());
    assert!(!c.apply(), "nothing left to apply");

    let e = c.accel();
    assert!(
        norm(sub(e.center, ACCEL_OFFSET)) < 0.01,
        "centre {:?}",
        e.center
    );
    for g in faces.iter() {
        let m = norm(e.apply(raw_accel(*g)));
        assert!(abs(m - G) < 0.01 * G, "|a| {m}");
    }
}

#[test]
fn accel_six_routine_times_out_with_insufficient_data() {
    let mut c = ImuCalibrator::new();
    c.start(Routine::AccelSix);
    // Only the +X face, held for 200 s: five faces never arrive.
    let mut t = 0u64;
    let mut k = 0u64;
    while t < 170_000_000 {
        c.push(t, raw_accel([G, 0.0, 0.0]), [0.0; 3]);
        t = k * 100_000;
        k += 1;
    }
    assert_eq!(c.running(), Some(Routine::AccelSix));
    assert!(!c.failed(), "not failed before the 180 s timeout");
    while t < 200_000_000 {
        c.push(t, raw_accel([G, 0.0, 0.0]), [0.0; 3]);
        t = k * 100_000;
        k += 1;
    }
    assert!(c.failed(), "failed after the timeout");
    assert_eq!(c.running(), None);
    assert!(!c.candidate());
}

#[test]
fn gyro_hold_routine_then_apply_sets_the_bias() {
    let mut c = ImuCalibrator::new();
    c.start(Routine::GyroHold);
    let bias: Vec3 = [0.02, -0.01, 0.005];
    let mut t = 0u64;
    for _ in 0..700 {
        c.push(t, [0.0, 0.0, G], bias);
        t += 10_000;
    }
    assert!(c.candidate(), "five seconds of still windows");
    assert_eq!(c.running(), None);
    assert!(c.apply());
    let b = c.gyro_bias();
    for i in 0..3 {
        assert!((b[i] - bias[i]).abs() < 0.001, "bias {i}: {}", b[i]);
    }
}

#[test]
fn gyro_bias_converges_while_still() {
    let mut c = ImuCalibrator::new();
    let bias: Vec3 = [0.02, -0.01, 0.005];
    let mut t = 0u64;
    for _ in 0..4000 {
        c.push(t, [0.0, 0.0, G], bias);
        t += 10_000;
    }
    let b = c.gyro_bias();
    for i in 0..3 {
        assert!((b[i] - bias[i]).abs() < 0.001, "bias {i}: {}", b[i]);
    }
    let (acc, gyr) = c.status();
    assert!(gyr.active, "still for 40 s is active");
    assert!(gyr.samples > 0 && acc.samples > 0);
}

#[test]
fn accel_sphere_is_applied_automatically_with_rate_limit() {
    let mut c = ImuCalibrator::new();
    let mut rng = Rng::new(21);
    let mut t = 0u64;
    let mut changed_any = false;
    // Tumbling: hold a random direction for 1.5 s, then move.
    for _ in 0..60 {
        let d = random_unit(&mut rng);
        let a = [d[0] * G, d[1] * G, d[2] * G];
        for _ in 0..150 {
            changed_any |= c.push(t, raw_accel(a), [0.0; 3]);
            t += 10_000;
        }
        t = moving(&mut c, t, 10);
    }
    assert!(changed_any, "some fit was applied");
    let e = c.accel();
    assert!(
        norm(sub(e.center, ACCEL_OFFSET)) < 0.05,
        "centre {:?}",
        e.center
    );
    let (acc, _) = c.status();
    assert!(acc.active, "confidence {}", acc.confidence);
    assert!(c.revision() > 0);
}

#[test]
fn magnetometer_partial_cone_is_not_applied_and_full_sphere_is() {
    let mut m = MagCalibrator::new();
    let mut rng = Rng::new(31);
    let mut t = 0u64;
    // Cone within 60 degrees of +Z.
    for _ in 0..1500 {
        let d = loop {
            let d = random_unit(&mut rng);
            if d[2] >= 0.5 {
                break d;
            }
        };
        m.push(t, raw_mag(crate::math::scale(d, 50.0)));
        t += 10_000;
    }
    assert!(
        m.status().coverage < 600,
        "cone coverage {}",
        m.status().coverage
    );
    assert!(!m.status().active);

    for _ in 0..3000 {
        let d = random_unit(&mut rng);
        m.push(t, raw_mag(crate::math::scale(d, 50.0)));
        t += 10_000;
    }
    assert!(m.status().active, "confidence {}", m.status().confidence);
    let e = m.field();
    assert!(
        norm(sub(e.center, [10.0, -5.0, 20.0])) < 1.0,
        "centre {:?}",
        e.center
    );
}

#[test]
fn magnetometer_rotate_routine_produces_candidate_until_applied() {
    let mut m = MagCalibrator::new();
    m.start(Routine::MagRotate);
    let mut rng = Rng::new(41);
    let mut t = 0u64;
    let mut n = 0;
    while !m.candidate() && n < 6000 {
        let d = random_unit(&mut rng);
        m.push(t, raw_mag(crate::math::scale(d, 50.0)));
        t += 10_000;
        n += 1;
    }
    assert!(m.candidate(), "routine should finish");
    assert_eq!(m.running(), None);
    let rev = m.revision();
    assert!(m.apply());
    assert_eq!(m.revision(), rev + 1);
    assert!(norm(sub(m.field().center, [10.0, -5.0, 20.0])) < 1.0);
    assert!(!m.apply());
}

#[test]
fn magnetometer_ignores_fields_outside_the_earth_range() {
    let mut m = MagCalibrator::new();
    let mut t = 0u64;
    for _ in 0..1000 {
        m.push(t, [100.0, 0.0, 0.0]);
        t += 10_000;
    }
    assert_eq!(m.status().samples, 0);
}

#[test]
fn start_ends_a_running_routine_and_discards_its_candidate() {
    let mut c = ImuCalibrator::new();
    c.start(Routine::GyroHold);
    c.start(Routine::AccelSix);
    assert_eq!(c.running(), Some(Routine::AccelSix));
    assert!(!c.candidate());
    c.stop();
    assert_eq!(c.running(), None);
    assert!(!c.apply());
}
