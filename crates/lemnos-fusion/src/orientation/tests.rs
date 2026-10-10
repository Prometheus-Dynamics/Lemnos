#![allow(clippy::needless_range_loop)]

use super::*;
use crate::math::{self, abs, sin_cos};
use crate::testutil::{Rng, body_rates, sensor_accel, sensor_field};

/// Field in µT, world frame: north is +x, dipping down (dip about 63 degrees).
const FIELD: Vec3 = [20.0, 0.0, -40.0];
const G: f32 = STANDARD_GRAVITY;

fn deg(r: f32) -> f32 {
    r * 180.0 / core::f32::consts::PI
}

fn nine_axis(kp: f32, ki: f32) -> OrientationConfig {
    OrientationConfig {
        mode: Mode::NineAxis,
        kp,
        ki,
        ..OrientationConfig::default()
    }
}

/// Roll and pitch truth: sinusoids. Returns `(roll, pitch, roll_rate, pitch_rate)`.
fn sinusoid_truth(t: f32) -> (f32, f32, f32, f32) {
    let (sr, cr) = sin_cos(0.3 * t);
    let (sp, cp) = sin_cos(0.4 * t + 1.0);
    (0.3 * sr, 0.2 * sp, 0.3 * 0.3 * cr, 0.2 * 0.4 * cp)
}

#[test]
fn static_tilt_converges_within_half_a_degree() {
    // The first sample sets the attitude (from gravity), so a board that
    // starts tilted holds it. A step from level is slow with the integral
    // gain (ki 0.05 settles over tens of seconds); see the doc.
    let mut o = Orientation::new(OrientationConfig::default());
    let a = sensor_accel(Quat::from_euler_deg(20.0, -10.0, 0.0), G);
    o.update_imu(0, [0.0; 3], a);
    for k in 1..=500u64 {
        o.update_imu(k * 10_000, [0.0; 3], a);
    }
    let out = o.output();
    assert!(out.valid);
    assert!((deg(out.roll) - 20.0).abs() < 0.5, "roll {}", deg(out.roll));
    assert!(
        (deg(out.pitch) + 10.0).abs() < 0.5,
        "pitch {}",
        deg(out.pitch)
    );
    for i in 0..3 {
        assert!((out.gravity[i] - a[i]).abs() < 0.05, "gravity {i}");
        assert!(abs(out.linear_acceleration[i]) < 0.05);
    }
}

#[test]
fn madgwick_converges_from_a_wrong_start() {
    let cfg = OrientationConfig {
        algorithm: Algorithm::Madgwick,
        beta: 0.3,
        ..OrientationConfig::default()
    };
    let mut o = Orientation::new(cfg);
    o.update_imu(0, [0.0; 3], [0.0, 0.0, G]);
    let a = sensor_accel(Quat::from_euler_deg(20.0, -10.0, 0.0), G);
    for k in 1..=2000u64 {
        o.update_imu(k * 10_000, [0.0; 3], a);
    }
    let out = o.output();
    assert!((deg(out.roll) - 20.0).abs() < 0.5, "roll {}", deg(out.roll));
    assert!(
        (deg(out.pitch) + 10.0).abs() < 0.5,
        "pitch {}",
        deg(out.pitch)
    );
    assert_eq!(out.gyro_bias, [0.0; 3]);
}

#[test]
fn gyro_bias_is_estimated_and_tilt_holds() {
    let bias = [0.02_f32, 0.02, 0.02];
    // Mahony gains kp 2, ki 0.2: the integral term's slow pole is about 9 s.
    // The default ki of 0.01 would take about 100 s (see the report).
    for cfg in [
        nine_axis(2.0, 0.2),
        OrientationConfig {
            kp: 2.0,
            ki: 0.2,
            ..OrientationConfig::default()
        },
    ] {
        let nine = cfg.mode == Mode::NineAxis;
        let mut o = Orientation::new(cfg);
        let mut worst = 0.0_f32;
        for k in 0..=6000u64 {
            let t_us = k * 10_000;
            let t = t_us as f32 * 1e-6;
            let (roll, pitch, rr, pr) = sinusoid_truth(t);
            let q = Quat::from_euler_rad(roll, pitch, 0.0);
            let gyro = math::add(body_rates(roll, rr, pr), bias);
            if nine {
                o.update_mag(t_us, sensor_field(q, FIELD), true);
            }
            o.update_imu(t_us, gyro, sensor_accel(q, G));
            if t > 0.5 {
                let out = o.output();
                let e = math::norm([deg(out.roll - roll), deg(out.pitch - pitch), 0.0]);
                worst = math::max(worst, e);
            }
        }
        let out = o.output();
        assert!(
            (out.gyro_bias[0] - bias[0]).abs() < 0.005,
            "bx {}",
            out.gyro_bias[0]
        );
        assert!(
            (out.gyro_bias[1] - bias[1]).abs() < 0.005,
            "by {}",
            out.gyro_bias[1]
        );
        assert!(
            worst < 1.0,
            "roll/pitch error {worst} deg (nine-axis: {nine})"
        );
    }
}

#[test]
fn six_axis_yaw_is_relative_and_drifts_with_z_bias() {
    let mut o = Orientation::new(OrientationConfig::default());
    for k in 0..=6000u64 {
        o.update_imu(k * 10_000, [0.0, 0.0, 0.01], [0.0, 0.0, G]);
    }
    // Gravity cannot observe z: yaw grows as bias * t = 0.6 rad.
    let yaw = o.output().yaw;
    assert!((yaw - 0.6).abs() < 0.05, "yaw {yaw}");
}

#[test]
fn nine_axis_yaw_holds_against_z_bias() {
    let mut o = Orientation::new(nine_axis(2.0, 0.2));
    for k in 0..=6000u64 {
        let t_us = k * 10_000;
        if k % 10 == 0 {
            o.update_mag(t_us, FIELD, true);
        }
        o.update_imu(t_us, [0.0, 0.0, 0.01], [0.0, 0.0, G]);
    }
    let out = o.output();
    assert!(deg(out.yaw).abs() < 2.0, "yaw {} deg", deg(out.yaw));
    assert!(!out.magnetic_disturbance);
}

#[test]
fn magnetic_disturbance_is_rejected_and_yaw_does_not_jump() {
    let mut o = Orientation::new(nine_axis(1.0, 0.01));
    let a = [0.0, 0.0, G];
    for k in 0..=1000u64 {
        let t_us = k * 10_000;
        o.update_mag(t_us, FIELD, true);
        o.update_imu(t_us, [0.0; 3], a);
    }
    assert!(!o.output().magnetic_disturbance);
    let yaw_before = o.output().yaw;

    // Field magnitude 30 % off: disturbed for the next 5 s.
    let disturbed = math::scale(FIELD, 1.3);
    let mut yaw_min = f32::MAX;
    let mut yaw_max = f32::MIN;
    for k in 1001..=1500u64 {
        let t_us = k * 10_000;
        o.update_mag(t_us, disturbed, true);
        o.update_imu(t_us, [0.0; 3], a);
        let out = o.output();
        assert!(out.magnetic_disturbance, "flag at sample {k}");
        yaw_min = math::min(yaw_min, out.yaw);
        yaw_max = math::max(yaw_max, out.yaw);
    }
    assert!(
        deg(yaw_max - yaw_min) < 1.0,
        "yaw moved {} deg",
        deg(yaw_max - yaw_min)
    );
    assert!(abs(o.output().yaw - yaw_before) < 0.0175);

    // Back to normal: the magnetometer is used again.
    for k in 1501..=1600u64 {
        let t_us = k * 10_000;
        o.update_mag(t_us, FIELD, true);
        o.update_imu(t_us, [0.0; 3], a);
    }
    assert!(!o.output().magnetic_disturbance);
}

#[test]
fn untrusted_magnetometer_is_flagged_and_unused() {
    let mut o = Orientation::new(nine_axis(1.0, 0.01));
    for k in 0..=200u64 {
        let t_us = k * 10_000;
        o.update_mag(t_us, FIELD, false);
        o.update_imu(t_us, [0.0; 3], [0.0, 0.0, G]);
    }
    assert!(o.output().magnetic_disturbance);
}

#[test]
fn stale_magnetometer_counts_as_absent() {
    let mut o = Orientation::new(nine_axis(1.0, 0.01));
    o.update_mag(0, FIELD, true);
    o.update_imu(0, [0.0; 3], [0.0, 0.0, G]);
    o.update_imu(400_000, [0.0; 3], [0.0, 0.0, G]);
    assert!(!o.output().magnetic_disturbance, "fresh at 0.4 s");
    o.update_imu(600_000, [0.0; 3], [0.0, 0.0, G]);
    assert!(o.output().magnetic_disturbance, "absent at 0.6 s");
}

#[test]
fn variable_dt_gives_the_same_attitude() {
    let run = |jitter: bool| -> (f32, f32) {
        let mut o = Orientation::new(OrientationConfig::default());
        let mut rng = Rng::new(99);
        let mut t_us = 0u64;
        let end_us = 5_000_000u64;
        loop {
            let t = t_us as f32 * 1e-6;
            let (roll, pitch, rr, pr) = sinusoid_truth(t);
            let q = Quat::from_euler_rad(roll, pitch, 0.0);
            o.update_imu(t_us, body_rates(roll, rr, pr), sensor_accel(q, G));
            if t_us >= end_us {
                break;
            }
            let step = if jitter {
                // 0.5 to 1.5 times the nominal 10 ms.
                (10_000.0 * (0.5 + rng.unit())) as u64
            } else {
                10_000
            };
            t_us = (t_us + step).min(end_us);
        }
        let out = o.output();
        (out.roll, out.pitch)
    };
    let (r0, p0) = run(false);
    let (r1, p1) = run(true);
    assert!(deg(abs(r0 - r1)) < 0.5, "roll {} vs {}", deg(r0), deg(r1));
    assert!(deg(abs(p0 - p1)) < 0.5, "pitch {} vs {}", deg(p0), deg(p1));
}

#[test]
fn gap_is_reanchored_not_integrated() {
    let mut o = Orientation::new(OrientationConfig::default());
    let a = [0.0, 0.0, G];
    o.update_imu(0, [0.0; 3], a);
    // A two second gap with a large rate: the attitude must not move.
    o.update_imu(2_000_000, [0.0, 0.0, 1.0], a);
    assert!(
        abs(o.output().yaw) < 1e-6,
        "gap integrated: {}",
        o.output().yaw
    );
    // The next close sample integrates normally.
    o.update_imu(2_010_000, [0.0, 0.0, 1.0], a);
    assert!(
        (o.output().yaw - 0.01).abs() < 1e-3,
        "yaw {}",
        o.output().yaw
    );
}

#[test]
fn mount_rotation_maps_sensor_to_body() {
    // Sensor mounted yawed 90 degrees: body = mount * sensor.
    let mount = Quat::from_euler_deg(0.0, 0.0, 90.0);
    let cfg = OrientationConfig {
        mount,
        ..OrientationConfig::default()
    };
    let mut o = Orientation::new(cfg);
    let q_ws = Quat::from_euler_deg(15.0, 0.0, 0.0);
    let a = sensor_accel(q_ws, G);
    o.update_imu(0, [0.0; 3], a);
    let out = o.output();
    // quaternion = q_world_sensor * mount^-1.
    let expected = q_ws.mul(mount.conj());
    let dot = out.quaternion.w * expected.w
        + out.quaternion.x * expected.x
        + out.quaternion.y * expected.y
        + out.quaternion.z * expected.z;
    assert!(abs(dot) > 0.9999, "quaternion mismatch {dot}");
    // Gravity is a body-frame vector.
    let body_a = mount.rotate(a);
    for i in 0..3 {
        assert!((out.gravity[i] - body_a[i]).abs() < 1e-3);
    }
}

#[test]
fn outputs_stay_finite_over_random_input() {
    let mut o = Orientation::new(nine_axis(1.0, 0.01));
    let mut rng = Rng::new(1234);
    let mut t_us = 0u64;
    for _ in 0..20_000 {
        t_us += 5_000 + (rng.unit() * 10_000.0) as u64;
        let g = [rng.signed(), rng.signed(), rng.signed()];
        let a = [
            rng.signed() * 20.0,
            rng.signed() * 20.0,
            G + rng.signed() * 5.0,
        ];
        let field = math::scale(FIELD, 1.0 + 0.5 * rng.signed());
        o.update_mag(t_us, field, rng.unit() < 0.8);
        o.update_imu(t_us, g, a);
        let out = o.output();
        assert!(out.roll.is_finite() && out.pitch.is_finite() && out.yaw.is_finite());
        assert!(out.quaternion.w.is_finite());
        assert!(abs(out.yaw) <= core::f32::consts::PI + 1e-3);
    }
}

#[test]
fn board_measured_static_values_hold_the_attitude() {
    // The Raze, at rest on the bench: accelerometer (9.793, -0.099, -0.440)
    // m/s², gyroscope about (0, -0.005, 0.002) rad/s, 100 Hz, 6-axis.
    let mut o = Orientation::new(OrientationConfig::default());
    let a = [9.793, -0.099, -0.440];
    let g = [0.0, -0.005, 0.002];
    for k in 0..500u64 {
        o.update_imu(k * 10_000, g, a);
    }
    let out = o.output();
    // The board is on its side (gravity along the sensor's X): the attitude
    // must follow the accelerometer, not a level start.
    assert!(
        (deg(out.pitch) + 87.0).abs() < 2.0,
        "pitch {} deg",
        deg(out.pitch)
    );
    for i in 0..3 {
        assert!(
            (out.gravity[i] - a[i]).abs() < 0.1,
            "gravity {i}: {:?}",
            out.gravity
        );
    }
}
