//! Orientation from an accelerometer, a gyroscope and, when the board wants
//! it, a magnetometer. The fusion filter and the calibrators are in [`fusion`].
//!
//! # Time
//!
//! A driver has no clock here, so the filter counts time itself: each `read`
//! is one sample, taken `1 / sample_rate_hz` after the one before it. The
//! caller must read at that rate. A caller that reads faster or slower than
//! the configured rate gets the wrong integration (gyro angles scale with the
//! ratio of the two rates), so `sample_rate_hz` is part of the board's
//! configuration, not a hint. A read that fails does not advance the count, so
//! a missing input leaves no gap in the filter's time.
//!
//! # Calibration
//!
//! The accelerometer, gyroscope and magnetometer are calibrated here, on every
//! sample this driver reads: the gyroscope's zero-rate offset, and the
//! accelerometer's and magnetometer's ellipsoid fits. The estimates are kept
//! in memory only. The driver model has no persistence, so a restart begins
//! from the factory calibration, unless the calibration words are set in the
//! configuration (`imu_calibration`, `mag_calibration`).
//!
//! # Configuration
//!
//! | Key | Default | Meaning |
//! |---|---|---|
//! | `sample_rate_hz` | 100 | Reads per second the caller makes (10 to 1000). |
//! | `mode` | by `uses` | `"9axis"` when a magnetometer is wired in, else `"6axis"`. `"9axis"` refuses to bind without one, and `"6axis"` ignores one. |
//! | `algorithm` | `"mahony"` | or `"madgwick"`. |
//! | `kp`, `ki` | 1.0, 0.05 | Mahony gains. |
//! | `beta` | 0.1 | Madgwick gain. |
//! | `mount_roll_deg`, `mount_pitch_deg`, `mount_yaw_deg` | 0 | Sensor to body rotation. |
//! | `declination_deg` | 0 | Magnetic to true north (9-axis). |
//! | `dip_deg` | learned | Dip reference (9-axis). |
//! | `imu_calibration`, `mag_calibration` | factory | Comma-separated words from an earlier applied calibration. |
//!
//! # Units
//!
//! The accelerometer is m/s², the gyroscope rad/s, and the magnetometer
//! contract is tesla (the filter takes microtesla, so the field is scaled).
//! The output is roll, pitch and yaw in radians. The `orientation/1` contract
//! carries only those three, so the quaternion, gravity, linear acceleration,
//! and confidences of the fusion filter are not readable through it.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod fusion;

#[cfg(test)]
mod tests;

use fusion::words::MAX_CALIBRATION_WORDS;
use fusion::{
    Algorithm, ImuCalibrator, MagCalibrator, Mode, Orientation as Filter, OrientationConfig, Quat,
};
use lemnos_contract_accelerometer::Accelerometer;
use lemnos_contract_gyroscope::Gyroscope;
use lemnos_contract_magnetometer::Magnetometer;
use lemnos_contract_orientation::Orientation as OrientationContract;
use lemnos_core::{
    Config, Description, Dev, Health, Item, Lifecycle, MethodId, Result, Status, Unit, Vector3,
    driver,
};

const READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// Reads per second when `sample_rate_hz` is not set.
const DEFAULT_SAMPLE_HZ: f64 = 100.0;
/// The slowest rate: the filter clamps each step to 0.1 s, so a slower period
/// would integrate too little.
const SLOWEST_SAMPLE_HZ: f64 = 10.0;
/// The fastest rate, one sample per millisecond.
const FASTEST_SAMPLE_HZ: f64 = 1000.0;
/// Degrees to radians.
const DEG: f32 = core::f32::consts::PI / 180.0;
/// Tesla to microtesla.
const UT_PER_T: f32 = 1_000_000.0;
/// Why a calibration the configuration named is not in use.
const REFUSED: &str = "calibration words refused: factory calibration";

#[driver("orientation", provides = [OrientationContract])]
pub struct Orientation {
    #[needs]
    accel: Dev<dyn Accelerometer>,
    #[needs]
    gyro: Dev<dyn Gyroscope>,
    #[needs(optional)]
    mag: Option<Dev<dyn Magnetometer>>,
    filter: Filter,
    imu: ImuCalibrator,
    mag_cal: MagCalibrator,
    /// The time between two reads, microseconds.
    period_us: u64,
    /// The filter's time of the last successful read, microseconds.
    now_us: u64,
    /// Whether the magnetometer is read and used.
    nine_axis: bool,
    /// Set when a calibration the configuration named was refused.
    refused: Option<&'static str>,
}

impl Lifecycle for Orientation {
    type Needs = OrientationNeeds;

    fn start(needs: OrientationNeeds, cfg: &Config<'_>) -> Result<Self> {
        let hz = sample_rate(cfg)?;
        let nine_axis = match setting_text(cfg, "mode")? {
            None => needs.mag.is_some(),
            Some("6axis") => false,
            Some("9axis") => true,
            Some(_) => return Err(Status::INVALID_ARGUMENT),
        };
        if nine_axis && needs.mag.is_none() {
            // 9-axis needs the magnetometer the board names in `uses`.
            return Err(Status::INVALID_ARGUMENT);
        }
        let filter = Filter::new(filter_config(cfg, nine_axis)?);

        let mut refused = None;
        let mut imu = ImuCalibrator::new();
        let mut mag_cal = MagCalibrator::new();
        let mut buf = [0i32; MAX_CALIBRATION_WORDS];
        if let Some(text) = setting_text(cfg, "imu_calibration")? {
            let n = words(text, &mut buf)?;
            if n > 0 && imu.load(&buf[..n]).is_err() {
                refused = Some(REFUSED);
            }
        }
        if let Some(text) = setting_text(cfg, "mag_calibration")? {
            let n = words(text, &mut buf)?;
            if n > 0 && mag_cal.load(&buf[..n]).is_err() {
                refused = Some(REFUSED);
            }
        }

        Ok(Self {
            accel: needs.accel,
            gyro: needs.gyro,
            mag: needs.mag,
            filter,
            imu,
            mag_cal,
            period_us: (1_000_000.0 / hz + 0.5) as u64,
            now_us: 0,
            nine_axis,
            refused,
        })
    }

    fn describe(&self, out: &mut Description) {
        for (name, field) in [("roll", "x"), ("pitch", "y"), ("yaw", "z")] {
            out.add(Item::value(name, READ).unit(Unit::Radian, 0).field(field));
        }
    }

    fn health(&self) -> Health {
        if let Some(why) = self.refused {
            return Health::Degraded(why);
        }
        if !self.nine_axis {
            return Health::Degraded("six-axis: yaw is relative");
        }
        let out = self.filter.output();
        // `now_us` moves on each successful read, so a device nobody has read yet isn't degraded.
        if self.now_us > 0 && !out.valid {
            return Health::Degraded("waiting for a usable accelerometer sample");
        }
        if out.magnetic_disturbance {
            return Health::Degraded("magnetometer ignored: disturbed or uncalibrated");
        }
        Health::Ok
    }
}

impl OrientationContract for Orientation {
    fn read(&mut self) -> Result<Vector3> {
        // Every input is read before any state changes, so a failed read
        // leaves the filter and the calibrators where they were.
        let accel = self.accel.read()?;
        let gyro = self.gyro.read()?;
        let field = match (self.nine_axis, self.mag.as_mut()) {
            (true, Some(mag)) => Some(mag.read()?),
            _ => None,
        };

        let t = self.now_us.saturating_add(self.period_us);
        let accel = [accel.x, accel.y, accel.z];
        let gyro = [gyro.x, gyro.y, gyro.z];
        self.imu.push(t, accel, gyro);
        let accel = self.imu.accel().apply(accel);
        let bias = self.imu.gyro_bias();
        let gyro = [gyro[0] - bias[0], gyro[1] - bias[1], gyro[2] - bias[2]];

        if let Some(field) = field {
            let raw = [field.x * UT_PER_T, field.y * UT_PER_T, field.z * UT_PER_T];
            self.mag_cal.push(t, raw);
            let trusted = self.mag_cal.status().active;
            let field = self.mag_cal.field().apply(raw);
            // The magnetometer goes first, so a heading set on this sample is
            // used by it.
            self.filter.update_mag(t, field, trusted);
        }
        self.filter.update_imu(t, gyro, accel);
        self.now_us = t;

        let out = self.filter.output();
        if !out.valid {
            // The first usable accelerometer sample starts the filter.
            return Err(Status::AGAIN);
        }
        Ok(Vector3 {
            x: out.roll,
            y: out.pitch,
            z: out.yaw,
        })
    }
}

/// The sample rate from `sample_rate_hz`, checked against the filter's limits.
fn sample_rate(cfg: &Config<'_>) -> Result<f64> {
    let hz = match cfg.get("sample_rate_hz") {
        None => DEFAULT_SAMPLE_HZ,
        Some(_) => cfg
            .float("sample_rate_hz")
            .ok_or(Status::INVALID_ARGUMENT)?,
    };
    if (SLOWEST_SAMPLE_HZ..=FASTEST_SAMPLE_HZ).contains(&hz) {
        Ok(hz)
    } else {
        Err(Status::INVALID_ARGUMENT)
    }
}

/// The text of a string setting, or `None` when it is not set.
fn setting_text<'a>(cfg: &Config<'a>, key: &str) -> Result<Option<&'a str>> {
    match cfg.get(key) {
        None => Ok(None),
        Some(_) => cfg.str(key).map(Some).ok_or(Status::INVALID_ARGUMENT),
    }
}

/// A number setting, or `default` when it is not set.
fn number(cfg: &Config<'_>, key: &str, default: f32) -> Result<f32> {
    match cfg.get(key) {
        None => Ok(default),
        Some(_) => cfg
            .float(key)
            .map(|v| v as f32)
            .ok_or(Status::INVALID_ARGUMENT),
    }
}

/// The filter's settings from the configuration.
fn filter_config(cfg: &Config<'_>, nine_axis: bool) -> Result<OrientationConfig> {
    let algorithm = match setting_text(cfg, "algorithm")?.unwrap_or("mahony") {
        "mahony" => Algorithm::Mahony,
        "madgwick" => Algorithm::Madgwick,
        _ => return Err(Status::INVALID_ARGUMENT),
    };
    let dip_rad = match cfg.get("dip_deg") {
        None => None,
        Some(_) => Some(number(cfg, "dip_deg", 0.0)? * DEG),
    };
    Ok(OrientationConfig {
        algorithm,
        mode: if nine_axis {
            Mode::NineAxis
        } else {
            Mode::SixAxis
        },
        kp: number(cfg, "kp", 1.0)?,
        ki: number(cfg, "ki", 0.05)?,
        beta: number(cfg, "beta", 0.1)?,
        mount: Quat::from_euler_deg(
            number(cfg, "mount_roll_deg", 0.0)?,
            number(cfg, "mount_pitch_deg", 0.0)?,
            number(cfg, "mount_yaw_deg", 0.0)?,
        ),
        declination_rad: number(cfg, "declination_deg", 0.0)? * DEG,
        dip_rad,
    })
}

/// Parses a comma-separated list of calibration words into `out`, returning
/// how many there are. An empty text is no words.
fn words(text: &str, out: &mut [i32; MAX_CALIBRATION_WORDS]) -> Result<usize> {
    let mut n = 0;
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if n == out.len() {
            return Err(Status::INVALID_ARGUMENT);
        }
        out[n] = part.parse().map_err(|_| Status::INVALID_ARGUMENT)?;
        n += 1;
    }
    Ok(n)
}
