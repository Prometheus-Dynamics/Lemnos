//! Orientation from an accelerometer, a gyroscope and, when the board wants
//! it, a magnetometer. The fusion filter is in [`fusion`].
//!
//! # Time
//!
//! The filter's time is the host's clock, not the caller's pace. A `read` takes
//! one sample of each input, reads the host clock once they are in hand, and
//! advances the filter by the time since its last successful sample. So the
//! output follows the motion, not the read pattern: the same turn read at 50 Hz,
//! at 200 Hz or at irregular intervals gives the same attitude. Each step is the
//! difference of two clock readings, so the steps add up to the elapsed time
//! whatever their lengths, and a read's own timing jitter does not bias an angle.
//!
//! The cases a clock can't settle are decided here:
//!
//! - **The first read.** The first usable accelerometer sample sets the attitude
//!   from gravity (and, in 9-axis, the heading from the magnetometer). No step
//!   exists yet, so nothing is integrated: the first read shows the tilt the
//!   sensors show, whatever the clock read before it.
//! - **A step of up to 0.5 s** is integrated as measured. The gyroscope's rate is
//!   known only at a read, so a step integrates that rate over its whole length.
//!   The integrator's error on a step is about (ω·dt)²/12 of the step: 2% at
//!   1 rad/s over 0.5 s, and far less over the few milliseconds of a normal
//!   interval. The bound is where a step is still short enough for that.
//! - **A gap over 0.5 s** holds the attitude: the read that ends the gap does not
//!   move it, and nothing is integrated across the gap. The rate over a span that
//!   long is unknown, and a stall can be seconds, so no rate is extrapolated. The
//!   reads after it integrate normally, and the sensors pull the attitude back:
//!   the tilt to the accelerometer, the heading to the magnetometer. The output
//!   does not jump at the gap, but a turn made during it is lost until then. At
//!   the default gains a 0.4 rad tilt error falls to 0.004 rad in 3 s, and then
//!   the integral term (`ki`) overshoots by about 0.015 rad, a tail that is still
//!   about 0.01 rad at 15 s. In six-axis mode yaw has no reference, so the part of
//!   a lost turn that is yaw stays lost. A client that reads slower than 2 Hz is
//!   in this case on every read.
//! - **A failed read** takes no sample. Its time is part of the next step, so an
//!   outage under 0.5 s is integrated as one step, and a longer one is a gap.
//! - **No clock.** The clock is a required need. A backend with no host clock
//!   refuses the bind, and the refusal names the `clock` need. A board file never
//!   names it.
//!
//! The sensors' own sample stamps are not used: `accel` and `gyro` are read by
//! single `read` calls, which carry no stamp, so the read's host time is the time
//! of each sample.
//!
//! # Calibration
//!
//! This driver doesn't calibrate anything. It uses what the sensor drivers report as it is,
//! so calibrating them is up to whoever is using them.
//!
//! # Configuration
//!
//! | Key | Default | Meaning |
//! |---|---|---|
//! | `sample_rate_hz` | not used | Checked (10 to 1000) and then ignored: the clock sets the time, not the rate. Kept so existing board files still bind. |
//! | `mode` | by `uses` | `"9axis"` when a magnetometer is wired in, else `"6axis"`. `"9axis"` refuses to bind without one, and `"6axis"` ignores one. |
//! | `algorithm` | `"mahony"` | or `"madgwick"`. |
//! | `kp`, `ki` | 1.0, 0.05 | Mahony gains. |
//! | `beta` | 0.1 | Madgwick gain. |
//! | `mount_roll_deg`, `mount_pitch_deg`, `mount_yaw_deg` | 0 | Sensor to body rotation. |
//! | `declination_deg` | 0 | Magnetic to true north (9-axis). |
//! | `dip_deg` | learned | Dip reference (9-axis). |
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

use fusion::{Algorithm, Mode, Orientation as Filter, OrientationConfig, Quat};
use lemnos_contract_accelerometer::Accelerometer;
use lemnos_contract_gyroscope::Gyroscope;
use lemnos_contract_magnetometer::Magnetometer;
use lemnos_contract_orientation::Orientation as OrientationContract;
use lemnos_core::{
    Config, ConfigError, Description, Dev, Health, HostClockHandle, Item, Key, Kind, Lifecycle,
    MethodId, Result, Status, Unit, Vector3, driver,
};

const READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// The range `sample_rate_hz` is checked against. The setting no longer sets the time step, so
/// the range only refuses nonsense values in existing board files.
const SLOWEST_SAMPLE_HZ: f64 = 10.0;
const FASTEST_SAMPLE_HZ: f64 = 1000.0;
/// Degrees to radians.
const DEG: f32 = core::f32::consts::PI / 180.0;
/// Tesla to microtesla.
const UT_PER_T: f32 = 1_000_000.0;
#[driver("orientation", provides = [OrientationContract])]
pub struct Orientation {
    #[needs]
    accel: Dev<dyn Accelerometer>,
    #[needs]
    gyro: Dev<dyn Gyroscope>,
    #[needs(optional)]
    mag: Option<Dev<dyn Magnetometer>>,
    /// The host clock the time between reads is measured on.
    #[needs]
    clock: HostClockHandle,
    filter: Filter,
    /// Whether a read has reached the sensors (so the device is not waiting for its first read).
    sampled: bool,
    /// Whether the magnetometer is read and used.
    nine_axis: bool,
}

/// The settings a board file gives the filter.
const KEYS: &[Key] = &[
    Key::optional("sample_rate_hz", Kind::Float),
    Key::optional("mode", Kind::Str),
    Key::optional("algorithm", Kind::Str),
    Key::optional("kp", Kind::Float),
    Key::optional("ki", Kind::Float),
    Key::optional("beta", Kind::Float),
    Key::optional("dip_deg", Kind::Float),
    Key::optional("mount_roll_deg", Kind::Float),
    Key::optional("mount_pitch_deg", Kind::Float),
    Key::optional("mount_yaw_deg", Kind::Float),
    Key::optional("declination_deg", Kind::Float),
];

impl Lifecycle for Orientation {
    type Needs = OrientationNeeds;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        if let Some(hz) = cfg.float("sample_rate_hz")
            && !(SLOWEST_SAMPLE_HZ..=FASTEST_SAMPLE_HZ).contains(&hz)
        {
            return Err(ConfigError::new("sample_rate_hz", "must be 10 to 1000"));
        }
        match cfg.str("mode") {
            None | Some("6axis" | "9axis") => {}
            Some(_) => return Err(ConfigError::new("mode", "must be 6axis or 9axis")),
        }
        match cfg.str("algorithm") {
            None | Some("mahony" | "madgwick") => {}
            Some(_) => return Err(ConfigError::new("algorithm", "must be mahony or madgwick")),
        }
        Ok(())
    }

    fn start(needs: OrientationNeeds, cfg: &Config<'_>) -> Result<Self> {
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

        Ok(Self {
            accel: needs.accel,
            gyro: needs.gyro,
            mag: needs.mag,
            clock: needs.clock,
            filter,
            sampled: false,
            nine_axis,
        })
    }

    fn describe(&self, out: &mut Description) {
        // The angles' principal ranges: roll and yaw are atan2 results in ±π; pitch is asin-like,
        // in ±π/2 (its root is never negative).
        for (name, field, half) in [
            ("roll", "x", core::f64::consts::PI),
            ("pitch", "y", core::f64::consts::FRAC_PI_2),
            ("yaw", "z", core::f64::consts::PI),
        ] {
            out.add(
                Item::value(name, READ)
                    .unit(Unit::Radian, 0)
                    .field(field)
                    .group("attitude")
                    .real_range(-half, half),
            );
        }
    }

    fn health(&self) -> Health {
        if !self.nine_axis {
            return Health::Degraded("six-axis: yaw is relative");
        }
        let out = self.filter.output();
        // A device nobody has read yet isn't degraded.
        if self.sampled && !out.valid {
            return Health::Degraded("waiting for a usable accelerometer sample");
        }
        if out.magnetic_disturbance {
            return Health::Degraded("magnetometer ignored: disturbed");
        }
        Health::Ok
    }
}

impl OrientationContract for Orientation {
    fn read(&mut self) -> Result<Vector3> {
        // Every input is read before any state changes, so a failed read
        // leaves the filter where it was.
        let accel = self.accel.read()?;
        let gyro = self.gyro.read()?;
        let field = match (self.nine_axis, self.mag.as_mut()) {
            (true, Some(mag)) => Some(mag.read()?),
            _ => None,
        };

        // The sample's time: the host clock, read once the inputs are in hand. Microseconds.
        let t = self.clock.now_ns() / 1_000;
        let accel = [accel.x, accel.y, accel.z];
        let gyro = [gyro.x, gyro.y, gyro.z];
        if let Some(field) = field {
            let field = [field.x * UT_PER_T, field.y * UT_PER_T, field.z * UT_PER_T];
            // The magnetometer goes first, so a heading set on this sample is
            // used by it.
            self.filter.update_mag(t, field, true);
        }
        self.filter.update_imu(t, gyro, accel);
        self.sampled = true;

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
    // The names are checked by `check_config`; this is the match over them.
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
