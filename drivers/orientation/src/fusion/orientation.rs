//! Orientation filter: Mahony or Madgwick, six-axis (accelerometer and
//! gyroscope) or nine-axis (adds the magnetometer).
//!
//! # Frames and conventions
//!
//! - **Sensor frame**: the IMU's own axes. `update_imu` and `update_mag` take
//!   calibrated values in this frame (rad/s, m/s², µT).
//! - **Body frame** (the robot): the sensor frame rotated by
//!   `OrientationConfig::mount`, a rotation that maps a sensor-frame vector to
//!   the body frame (`v_body = mount.rotate(v_sensor)`).
//! - **World frame**: right-handed, z up, and x pointing at magnetic north
//!   when the magnetometer is used. Because x is north, y points west, so yaw
//!   is a right-handed rotation about z: 0 puts the body x-axis at north and a
//!   positive yaw turns toward west. (The "east-north-up" wording in the design
//!   doc is not used: yaw 0 needs x = north.)
//! - `Output::quaternion` is `q_world_body = q_world_sensor ⊗ mount⁻¹`, so it
//!   rotates body vectors into the world. `gravity` is the specific-force
//!   direction in the body frame, `|g| = STANDARD_GRAVITY`, which is `+g` up
//!   when still. `linear_acceleration` is the calibrated body acceleration
//!   minus `gravity`.
//! - `yaw` is the body's heading from the ZYX Euler angles of
//!   `quaternion`. In nine-axis mode `declination_rad` is added and the result
//!   wrapped to `[-pi, pi]`. In six-axis mode yaw is relative: nothing observes
//!   the heading, so it drifts with the gyroscope's z bias.
//!
//! Internally the filter tracks `q_world_sensor`. It integrates the gyroscope,
//! corrects the tilt with the accelerometer (weighted by how close `|a|` is to
//! gravity) and the heading with the magnetometer (when it is usable).

use crate::fusion::STANDARD_GRAVITY;
use crate::fusion::Vec3;
use crate::fusion::math::{self, abs, asin, atan2, clamp, cross, dot, norm, scale, sqrt, wrap_pi};
use crate::fusion::quat::Quat;

/// The correction algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Algorithm {
    /// Complementary filter with a PI correction; estimates the gyro bias.
    #[default]
    Mahony,
    /// Gradient descent on the sensor error (`beta`).
    Madgwick,
}

/// Which sensors feed the filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// Accelerometer and gyroscope. Roll and pitch only; yaw is relative.
    #[default]
    SixAxis,
    /// Adds the magnetometer, which references yaw to magnetic north.
    NineAxis,
}

/// Filter settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrientationConfig {
    pub algorithm: Algorithm,
    pub mode: Mode,
    /// Mahony proportional gain.
    pub kp: f32,
    /// Mahony integral gain (gyro bias estimation).
    pub ki: f32,
    /// Madgwick gain.
    pub beta: f32,
    /// Sensor frame to body frame.
    pub mount: Quat,
    /// Magnetic to true north, added to yaw in nine-axis mode.
    pub declination_rad: f32,
    /// Dip reference (rad). `None` learns it from the magnetometer.
    pub dip_rad: Option<f32>,
}

impl Default for OrientationConfig {
    fn default() -> Self {
        OrientationConfig {
            algorithm: Algorithm::Mahony,
            mode: Mode::SixAxis,
            kp: 1.0,
            ki: 0.05,
            beta: 0.1,
            mount: Quat::IDENTITY,
            declination_rad: 0.0,
            dip_rad: None,
        }
    }
}

/// The filter's output.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Output {
    /// At least one IMU update has run.
    pub valid: bool,
    /// World from body (mount applied).
    pub quaternion: Quat,
    /// Roll, pitch and yaw in radians (ZYX).
    pub roll: f32,
    pub pitch: f32,
    pub yaw: f32,
    /// Gravity direction in the body frame, m/s².
    pub gravity: Vec3,
    /// Calibrated body acceleration minus gravity, m/s².
    pub linear_acceleration: Vec3,
    /// The integral term's gyro bias estimate, rad/s, sensor frame (zero for Madgwick).
    pub gyro_bias: Vec3,
    /// The magnetometer is not used now: disturbed, untrusted, implausible (outside 15 to 80 µT),
    /// or absent.
    pub magnetic_disturbance: bool,
    /// The magnetometer's confidence, permille: 1000 while its field is used, 0 while it is ignored
    /// or the filter is six-axis.
    pub magnetometer_confidence: u16,
}

/// A magnetometer sample is fresh for this long (µs).
const MAG_FRESH_US: u64 = 500_000;
/// The longest step integrated as one step (µs). A longer gap between IMU samples integrates nothing
/// across it (its rate is unknown): the sample re-seeds the attitude from the sensors instead.
const GAP_US: u64 = 500_000;
/// Integral term limit (rad/s).
const INTEGRAL_MAX: f32 = 0.5;
/// Accelerometer trust: full within this relative error of gravity...
const ACCEL_FULL: f32 = 0.1;
/// ...zero beyond this, linear between.
const ACCEL_ZERO: f32 = 0.5;
/// A still sample: below this gyro magnitude (rad/s) and trusted accelerometer.
const STILL_GYRO: f32 = 0.05;
/// Magnetic disturbance: magnitude off the reference by more than this fraction.
const FIELD_TOL: f32 = 0.15;
/// Magnetic disturbance: dip angle off the reference by more than this (rad).
const DIP_TOL: f32 = 0.174_532_93;
/// EMA weight for the learned field magnitude and dip reference.
const LEARN_ALPHA: f32 = 0.02;
/// Magnetic plausibility: a field whose total (µT) is outside this range is not the Earth's, so the
/// magnetometer is ignored for that sample. The relative check above learns its reference from the
/// field it sees, so a field that is wrong from the start would be learned as right; this bound does
/// not learn. Bounds: `dev` `crates/lemnos-fusion/src/calibrate.rs:59` (lower) and
/// `docs/imu-calibration-fusion.md` (upper, 80 µT).
const FIELD_MIN_UT: f32 = 15.0;
const FIELD_MAX_UT: f32 = 80.0;

/// The heading (yaw) of a board at `roll` and `pitch` whose magnetometer reads `m` (sensor frame, µT).
fn heading(roll: f32, pitch: f32, m: Vec3) -> f32 {
    // The field in the body, turned by the tilt, is horizontal in the level frame: its direction is the heading.
    let level = Quat::from_euler_rad(roll, pitch, 0.0);
    let u = level.rotate(m);
    -atan2(u[1], u[0])
}

/// The orientation filter. Feed it with [`Orientation::update_imu`] and
/// [`Orientation::update_mag`]; read [`Orientation::output`].
#[derive(Clone, Debug)]
pub struct Orientation {
    config: OrientationConfig,
    /// World from sensor.
    q: Quat,
    valid: bool,
    last_t: u64,
    /// Mahony integral term (sensor frame); the gyro bias is its negation.
    integral: Vec3,
    accel: Vec3,
    accel_dir: Vec3,
    accel_w: f32,
    accel_dip_ok: bool,
    still: bool,
    mag_seen: bool,
    mag_t: u64,
    mag_ok: bool,
    mag_pending: Option<Vec3>,
    field_ref: Option<f32>,
    dip_ref: Option<f32>,
    /// A gap was seen and its sample had no usable accelerometer: the next usable sample re-seeds.
    waiting: bool,
    /// The last sample re-seeded the attitude after a gap (cleared by the next normal step).
    reseeded: bool,
    /// The last magnetometer sample's total was outside 15 to 80 µT (see `FIELD_MIN_UT`).
    implausible: bool,
    out: Output,
}

impl Orientation {
    /// A filter at rest with the identity attitude, until its first sample.
    pub fn new(config: OrientationConfig) -> Self {
        Orientation {
            config,
            q: Quat::IDENTITY,
            valid: false,
            last_t: 0,
            integral: [0.0; 3],
            accel: [0.0; 3],
            accel_dir: [0.0; 3],
            accel_w: 0.0,
            accel_dip_ok: false,
            still: false,
            mag_seen: false,
            mag_t: 0,
            mag_ok: false,
            mag_pending: None,
            field_ref: None,
            dip_ref: None,
            waiting: false,
            reseeded: false,
            implausible: false,
            out: Output::default(),
        }
    }

    /// Back to the start, keeping the configuration.
    pub fn reset(&mut self) {
        *self = Orientation::new(self.config);
    }

    /// The configuration.
    pub fn config(&self) -> &OrientationConfig {
        &self.config
    }

    /// One IMU sample at `t_us`: calibrated gyroscope (rad/s) and
    /// accelerometer (m/s²), sensor frame.
    pub fn update_imu(&mut self, t_us: u64, gyro: Vec3, accel: Vec3) {
        self.take_inputs(gyro, accel);

        if !self.valid {
            // Start from the first usable accelerometer sample: an unusable one
            // (no data, or a zero reading) would otherwise start level.
            if self.accel_w > 0.0 {
                self.init(t_us);
                self.valid = true;
                self.last_t = t_us;
                self.refresh_output();
            }
            return;
        }

        let dt_us = t_us.saturating_sub(self.last_t);
        if dt_us == 0 {
            // No time since the last sample: nothing to integrate.
            self.refresh_output();
            return;
        }
        if dt_us > GAP_US {
            // A gap. Nothing is integrated across it: the gyroscope's rate over it is unknown. The
            // sensors give the attitude directly, so this sample re-seeds it, as the first usable
            // sample does. An accelerometer with no usable tilt (shaken, or far from 1 g) cannot seed
            // it: as the first read does, this read says AGAIN, and last_t stays put so that the next
            // usable sample is still a gap and re-seeds.
            if self.accel_w > 0.0 {
                self.reseed(t_us);
                self.waiting = false;
                self.reseeded = true;
            } else {
                self.waiting = true;
            }
            self.refresh_output();
            return;
        }
        self.last_t = t_us;
        self.reseeded = false;
        let dt = dt_us as f32 * 1e-6;

        let mag_fresh = self.mag_fresh();
        let mag = if self.config.mode == Mode::NineAxis && mag_fresh && self.mag_ok {
            self.mag_pending
        } else {
            None
        };
        match self.config.algorithm {
            Algorithm::Mahony => self.update_mahony(dt, gyro, mag),
            Algorithm::Madgwick => self.update_madgwick(dt, gyro, mag),
        }
        self.refresh_output();
    }

    /// The inputs of this sample, for the checks a magnetometer sample makes against them (the dip).
    fn take_inputs(&mut self, gyro: Vec3, accel: Vec3) {
        let a_mag = norm(accel);
        let (dir, w) = if a_mag > 1e-6 {
            (scale(accel, 1.0 / a_mag), accel_trust(a_mag))
        } else {
            ([0.0; 3], 0.0)
        };
        self.accel = accel;
        self.accel_dir = dir;
        self.accel_w = w;
        self.accel_dip_ok = w >= 0.9;
        self.still = norm(gyro) < STILL_GYRO && w >= 0.99;
    }

    /// A magnetometer sample taken with the IMU sample `gyro`, `accel` of the same time. The dip check
    /// uses this sample's gravity, so the caller passes the IMU sample before the field (see `update_mag`).
    pub fn update_mag_with_imu(
        &mut self,
        t_us: u64,
        field_ut: Vec3,
        trusted: bool,
        gyro: Vec3,
        accel: Vec3,
    ) {
        self.take_inputs(gyro, accel);
        self.update_mag(t_us, field_ut, trusted);
    }

    /// One magnetometer sample at `t_us`: calibrated field (µT, sensor frame).
    /// `trusted` is false while the magnetometer's calibration is not applied. The dip check uses the
    /// gravity of the last IMU sample; use `update_mag_with_imu` when the field and the IMU sample are
    /// taken together.
    pub fn update_mag(&mut self, t_us: u64, field_ut: Vec3, trusted: bool) {
        self.mag_seen = true;
        self.mag_t = t_us;
        self.mag_pending = None;
        let mag = norm(field_ut);
        if mag < 1e-6 {
            self.mag_ok = false;
            self.implausible = true;
            self.set_mag_status();
            return;
        }
        let n = scale(field_ut, 1.0 / mag);

        self.implausible = !(FIELD_MIN_UT..=FIELD_MAX_UT).contains(&mag);
        let mut disturbed = self.implausible;
        if let Some(r) = self.field_ref
            && abs(mag - r) > FIELD_TOL * r
        {
            disturbed = true;
        }
        let dip = if self.accel_dip_ok {
            // Angle below horizontal: the field points down, opposite the
            // accelerometer's up direction.
            Some(asin(-dot(n, self.accel_dir)))
        } else {
            None
        };
        let dip_reference = self.config.dip_rad.or(self.dip_ref);
        if let (Some(d), Some(dm)) = (dip_reference, dip)
            && abs(dm - d) > DIP_TOL
        {
            disturbed = true;
        }

        let usable = trusted && !disturbed;
        if usable {
            self.field_ref = Some(match self.field_ref {
                None => mag,
                Some(r) => r + LEARN_ALPHA * (mag - r),
            });
            if self.config.dip_rad.is_none()
                && self.still
                && let Some(dm) = dip
            {
                self.dip_ref = Some(match self.dip_ref {
                    None => dm,
                    Some(d) => d + LEARN_ALPHA * (dm - d),
                });
            }
            self.mag_pending = Some(n);
        }
        self.mag_ok = usable;
        self.set_mag_status();
    }

    /// The latest output (cheap: computed on each update).
    pub fn output(&self) -> Output {
        self.out
    }

    /// Whether a gap's sample had no usable accelerometer, so the attitude is not re-seeded yet.
    pub fn waiting_for_accel(&self) -> bool {
        self.waiting
    }

    /// Whether the last sample re-seeded the attitude after a gap.
    pub fn reseeded(&self) -> bool {
        self.reseeded
    }

    /// Whether the magnetometer is ignored because its field is outside the Earth's range (and the
    /// sample is fresh). A disturbed or absent magnetometer is not this.
    pub fn magnetic_implausible(&self) -> bool {
        self.implausible && self.mag_fresh()
    }

    fn mag_fresh(&self) -> bool {
        self.mag_seen && self.last_t.saturating_sub(self.mag_t) <= MAG_FRESH_US
    }

    fn disturbance_flag(&self) -> bool {
        self.config.mode == Mode::NineAxis && !(self.mag_fresh() && self.mag_ok)
    }

    /// 1000 while the magnetometer's field is used, else 0.
    fn magnetometer_confidence(&self) -> u16 {
        if self.config.mode == Mode::NineAxis && self.mag_fresh() && self.mag_ok {
            1000
        } else {
            0
        }
    }

    /// Copies the magnetometer's status into the output (the flag and the confidence).
    fn set_mag_status(&mut self) {
        self.out.magnetic_disturbance = self.disturbance_flag();
        self.out.magnetometer_confidence = self.magnetometer_confidence();
    }

    /// Re-seeds the attitude at `t_us` from the sensors, as the first usable sample does: the tilt
    /// from gravity, and in nine-axis the heading from a fresh usable magnetometer sample. Otherwise
    /// the heading is kept as it was (nothing measures it). The integral term restarts.
    fn reseed(&mut self, t_us: u64) {
        let a = self.accel;
        let roll = atan2(a[1], a[2]);
        let pitch = atan2(-a[0], sqrt(a[1] * a[1] + a[2] * a[2]));
        let fresh_mag = self.config.mode == Mode::NineAxis
            && self.mag_seen
            && self.mag_ok
            && t_us.saturating_sub(self.mag_t) <= MAG_FRESH_US;
        let yaw = match self.mag_pending {
            Some(m) if fresh_mag => heading(roll, pitch, m),
            _ => self.q.euler().2,
        };
        self.q = Quat::from_euler_rad(roll, pitch, yaw);
        self.integral = [0.0; 3];
        self.last_t = t_us;
    }

    /// Starts from the accelerometer's tilt. In nine-axis mode, a fresh
    /// usable magnetometer sample also sets the heading.
    fn init(&mut self, t_us: u64) {
        let a = self.accel;
        let mut roll = 0.0;
        let mut pitch = 0.0;
        if self.accel_w > 0.0 {
            roll = atan2(a[1], a[2]);
            pitch = atan2(-a[0], sqrt(a[1] * a[1] + a[2] * a[2]));
        }
        let mut yaw = 0.0;
        if self.config.mode == Mode::NineAxis
            && self.mag_fresh()
            && self.mag_ok
            && let Some(m) = self.mag_pending
        {
            yaw = heading(roll, pitch, m);
        }
        self.q = Quat::from_euler_rad(roll, pitch, yaw);
        self.integral = [0.0; 3];
        self.last_t = t_us;
    }

    /// Reference field direction in the sensor frame for the current attitude:
    /// the horizontal magnitude along world x, and the world z component.
    fn mag_reference(q: Quat, m: Vec3) -> Vec3 {
        let h = q.rotate(m);
        let bx = sqrt(h[0] * h[0] + h[1] * h[1]);
        let bz = h[2];
        q.conj().rotate([bx, 0.0, bz])
    }

    fn update_mahony(&mut self, dt: f32, gyro: Vec3, mag: Option<Vec3>) {
        // The correction compares the sensors at this sample with the attitude at this sample, so the
        // gyroscope's step to it is taken first. Comparing with the attitude one step back leaves the
        // estimate one step ahead of a steady turn, by gyro * dt: a rate-dependent error.
        let q = integrate(self.q, gyro, dt);
        let v = q.conj().rotate([0.0, 0.0, 1.0]);
        let mut e = [0.0_f32; 3];
        if self.accel_w > 0.0 {
            e = math::add(e, scale(cross(self.accel_dir, v), self.accel_w));
        }
        if let Some(m) = mag {
            let w = Self::mag_reference(q, m);
            e = math::add(e, cross(m, w));
        }
        let cfg = self.config;
        if cfg.ki > 0.0 {
            for (i, ei) in self.integral.iter_mut().zip(e.iter()) {
                *i = clamp(*i + cfg.ki * ei * dt, -INTEGRAL_MAX, INTEGRAL_MAX);
            }
        }
        let omega = math::add(math::add(gyro, scale(e, cfg.kp)), self.integral);
        self.q = integrate(self.q, omega, dt);
    }

    fn update_madgwick(&mut self, dt: f32, gyro: Vec3, mag: Option<Vec3>) {
        let q = self.q;
        // The gradient is taken at the attitude after this sample's gyroscope step, for the same reason
        // as Mahony's: at the attitude one step back it would hold the estimate one step ahead.
        let p = integrate(q, gyro, dt);
        let (q0, q1, q2, q3) = (p.w, p.x, p.y, p.z);
        let mut f = [0.0_f32; 6];
        let mut j = [[0.0_f32; 4]; 6];
        let mut rows = 3;

        // Gravity rows, weighted by the accelerometer's trust.
        let a = self.accel_dir;
        let wa = self.accel_w;
        f[0] = wa * (2.0 * (q1 * q3 - q0 * q2) - a[0]);
        f[1] = wa * (2.0 * (q0 * q1 + q2 * q3) - a[1]);
        f[2] = wa * (2.0 * (0.5 - q1 * q1 - q2 * q2) - a[2]);
        j[0] = [-2.0 * q2, 2.0 * q3, -2.0 * q0, 2.0 * q1];
        j[1] = [2.0 * q1, 2.0 * q0, 2.0 * q3, 2.0 * q2];
        j[2] = [0.0, -4.0 * q1, -4.0 * q2, 0.0];

        if let Some(m) = mag {
            rows = 6;
            let h = p.rotate(m);
            let bx = sqrt(h[0] * h[0] + h[1] * h[1]);
            let bz = h[2];
            let w = Self::mag_reference(p, m);
            f[3] = w[0] - m[0];
            f[4] = w[1] - m[1];
            f[5] = w[2] - m[2];
            // The reference's sensor-frame derivative rows (x-aligned reference).
            j[3] = [
                -2.0 * bz * q2,
                2.0 * bz * q3,
                -4.0 * bx * q2 - 2.0 * bz * q0,
                -4.0 * bx * q3 + 2.0 * bz * q1,
            ];
            j[4] = [
                -2.0 * bx * q3 + 2.0 * bz * q1,
                2.0 * bx * q2 + 2.0 * bz * q0,
                2.0 * bx * q1 + 2.0 * bz * q3,
                -2.0 * bx * q0 + 2.0 * bz * q2,
            ];
            j[5] = [
                2.0 * bx * q2,
                2.0 * bx * q3 - 4.0 * bz * q1,
                2.0 * bx * q0 - 4.0 * bz * q2,
                2.0 * bx * q1,
            ];
        }

        let mut s = [0.0_f32; 4];
        for (fi, ji) in f.iter().zip(j.iter()).take(rows) {
            for k in 0..4 {
                s[k] += ji[k] * fi;
            }
        }
        let sn = sqrt(s.iter().map(|v| v * v).sum::<f32>());
        let beta = self.config.beta;
        // q_dot = 0.5 q ⊗ (0, ω) - beta * s / |s|
        let qw = q.mul(Quat {
            w: 0.0,
            x: gyro[0],
            y: gyro[1],
            z: gyro[2],
        });
        let mut qd = [0.5 * qw.w, 0.5 * qw.x, 0.5 * qw.y, 0.5 * qw.z];
        if sn > 1e-12 {
            for k in 0..4 {
                qd[k] -= beta * s[k] / sn;
            }
        }
        self.q = Quat {
            w: q.w + qd[0] * dt,
            x: q.x + qd[1] * dt,
            y: q.y + qd[2] * dt,
            z: q.z + qd[3] * dt,
        }
        .normalized();
    }

    fn refresh_output(&mut self) {
        let mount = self.config.mount;
        let qwb = self.q.mul(mount.conj()).normalized();
        let (roll, pitch, mut yaw) = qwb.euler();
        if self.config.mode == Mode::NineAxis {
            yaw = wrap_pi(yaw + self.config.declination_rad);
        }
        let gravity = qwb.conj().rotate([0.0, 0.0, STANDARD_GRAVITY]);
        let accel_body = mount.rotate(self.accel);
        let gyro_bias = match self.config.algorithm {
            Algorithm::Mahony => scale(self.integral, -1.0),
            Algorithm::Madgwick => [0.0; 3],
        };
        self.out = Output {
            valid: self.valid,
            quaternion: qwb,
            roll,
            pitch,
            yaw,
            gravity,
            linear_acceleration: math::sub(accel_body, gravity),
            gyro_bias,
            magnetic_disturbance: self.disturbance_flag(),
            magnetometer_confidence: self.magnetometer_confidence(),
        };
    }
}

/// Accelerometer trust: 1 within ±10 % of gravity, 0 beyond ±50 %, linear between.
fn accel_trust(a_mag: f32) -> f32 {
    let d = abs(a_mag / STANDARD_GRAVITY - 1.0);
    if d <= ACCEL_FULL {
        1.0
    } else if d >= ACCEL_ZERO {
        0.0
    } else {
        (ACCEL_ZERO - d) / (ACCEL_ZERO - ACCEL_FULL)
    }
}

/// `q + dt · ½ q ⊗ (0, ω)`, normalised.
fn integrate(q: Quat, omega: Vec3, dt: f32) -> Quat {
    let qw = q.mul(Quat {
        w: 0.0,
        x: omega[0],
        y: omega[1],
        z: omega[2],
    });
    Quat {
        w: q.w + 0.5 * qw.w * dt,
        x: q.x + 0.5 * qw.x * dt,
        y: q.y + 0.5 * qw.y * dt,
        z: q.z + 0.5 * qw.z * dt,
    }
    .normalized()
}

#[cfg(test)]
mod tests;
