//! The simulation script (`lemnosd --sim-script FILE`): the motion the board
//! goes through, its noise, the loads on its supply, the faults to inject and
//! the scripted GPIO inputs. TOML, versioned like a board definition.
//!
//! ```toml
//! format = "lemnos.sim"
//! schema_version = 1
//! speed = 1.0          # simulated seconds per real second
//! repeat = true        # loop the motion; otherwise the last pose is held
//!
//! [[motion]]
//! kind = "still"
//! seconds = 2.0
//!
//! [[motion]]
//! kind = "attitude"    # slerp to a pose, then hold it
//! seconds = 1.0
//! roll_deg = 20.0
//! pitch_deg = -10.0
//! yaw_deg = 0.0
//!
//! [[motion]]
//! kind = "rotate"      # constant body rate
//! seconds = 4.0
//! rate_dps = [0.0, 0.0, 90.0]
//!
//! [[motion]]
//! kind = "shake"       # sinusoidal linear acceleration along the world x axis
//! seconds = 2.0
//! amplitude = 3.0      # m/s², peak
//! hz = 6.0
//!
//! [[fault]]
//! device = "magnetometer"   # a board device id: imu, magnetometer, power
//! kind = "unresponsive"     # unresponsive | bus-error | stuck
//! at = 6.0
//! seconds = 3.0            # omitted: until the end of the run
//!
//! [[gpio]]                 # a scripted input level (a switch)
//! chip = "pinctrl-rp1"
//! line = 5
//! level = true
//! at = 3.0
//! ```

use serde::Deserialize;
use std::fmt;
use std::path::Path;

/// The format name of a simulation script.
pub const FORMAT: &str = "lemnos.sim";
/// The schema version this build reads.
pub const SCHEMA_VERSION: u32 = 1;

/// A parsed simulation script. Every field has a default, so an empty
/// script (the format and version only) is a still board with a
/// powered-off supply load.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    pub format: String,
    pub schema_version: u32,
    /// Simulated seconds per real second.
    #[serde(default = "one")]
    pub speed: f64,
    /// Seed of the noise generator: the same script and seed give the same run.
    #[serde(default = "seed")]
    pub seed: u64,
    /// Loop the motion once it ends; otherwise the last pose is held.
    #[serde(default)]
    pub repeat: bool,
    #[serde(default)]
    pub imu: Imu,
    #[serde(default)]
    pub mag: Mag,
    #[serde(default)]
    pub fan: Fan,
    #[serde(default)]
    pub power: Power,
    #[serde(default)]
    pub thermal: Thermal,
    #[serde(default, rename = "motion")]
    pub motion: Vec<Segment>,
    #[serde(default, rename = "load")]
    pub loads: Vec<Load>,
    #[serde(default, rename = "fault")]
    pub faults: Vec<Fault>,
    #[serde(default, rename = "gpio")]
    pub gpio: Vec<GpioLevel>,
}

fn one() -> f64 {
    1.0
}

fn seed() -> u64 {
    1
}

/// The inertial sensors' noise and bias.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Imu {
    /// Accelerometer noise, m/s² (one sigma).
    pub accel_noise: f64,
    /// Gyroscope noise, rad/s (one sigma).
    pub gyro_noise: f64,
    /// Gyroscope bias, rad/s, per body axis.
    pub gyro_bias: [f64; 3],
}

impl Default for Imu {
    fn default() -> Self {
        Self {
            accel_noise: 0.02,
            gyro_noise: 0.002,
            gyro_bias: [0.0; 3],
        }
    }
}

/// The magnetometer's field and noise.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Mag {
    /// The Earth field in the world frame (z up, x toward magnetic north), µT.
    pub field_ut: [f64; 3],
    /// Noise, µT (one sigma).
    pub noise_ut: f64,
}

impl Default for Mag {
    fn default() -> Self {
        Self {
            field_ut: [20.0, 0.0, -40.0],
            noise_ut: 0.3,
        }
    }
}

/// The fan's response: a first-order tachometer lagging the PWM duty.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Fan {
    /// Speed at 100 % duty, rpm.
    pub max_rpm: f64,
    /// Time constant of the rotor, seconds.
    pub time_constant_s: f64,
}

impl Default for Fan {
    fn default() -> Self {
        Self {
            max_rpm: 4200.0,
            time_constant_s: 0.8,
        }
    }
}

/// The supply: its voltage, a base load, the load of each switched port that
/// is on, and the drop across the supply's output resistance.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Power {
    pub bus_v: f64,
    pub base_a: f64,
    pub port_a: f64,
    pub sag_ohm: f64,
    /// Current noise, A (one sigma).
    pub noise_a: f64,
}

impl Default for Power {
    fn default() -> Self {
        Self {
            bus_v: 5.1,
            base_a: 0.35,
            port_a: 0.45,
            sag_ohm: 0.04,
            noise_a: 0.01,
        }
    }
}

/// The board's temperature: it warms with the load and the fan's duty.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Thermal {
    pub ambient_c: f64,
    /// °C per ampere of supply current, at steady state.
    pub c_per_amp: f64,
    /// °C per unit of fan duty (0 to 1), at steady state.
    pub c_per_duty: f64,
    pub time_constant_s: f64,
}

impl Default for Thermal {
    fn default() -> Self {
        Self {
            ambient_c: 34.0,
            c_per_amp: 3.0,
            c_per_duty: 6.0,
            time_constant_s: 8.0,
        }
    }
}

/// One part of the motion, from the time the previous part ended.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Segment {
    /// No rotation and no linear acceleration: gravity only.
    Still { seconds: f64 },
    /// A constant rate about the body axes, degrees per second.
    Rotate { seconds: f64, rate_dps: [f64; 3] },
    /// A sinusoidal linear acceleration along the world x axis, peak
    /// `amplitude` m/s² at `hz`, with the attitude held.
    Shake {
        seconds: f64,
        amplitude: f64,
        hz: f64,
    },
    /// Slerps from the attitude the segment starts at to the given one, then
    /// holds it for the rest of the segment.
    Attitude {
        seconds: f64,
        roll_deg: f64,
        pitch_deg: f64,
        yaw_deg: f64,
    },
}

impl Segment {
    pub fn seconds(&self) -> f64 {
        match *self {
            Self::Still { seconds }
            | Self::Rotate { seconds, .. }
            | Self::Shake { seconds, .. }
            | Self::Attitude { seconds, .. } => seconds,
        }
    }
}

/// A supply load: `amps` extra while `at` ≤ t < `at + seconds`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Load {
    pub at: f64,
    pub seconds: f64,
    pub amps: f64,
}

/// What a fault does to a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FaultKind {
    /// The device does not acknowledge its address.
    Unresponsive,
    /// Its transactions fail at random: each step of the simulation, the
    /// device NACKs with probability `rate`.
    BusError,
    /// The device keeps its last samples.
    Stuck,
}

/// A fault on a device, from `at` for `seconds` (forever when omitted).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fault {
    pub device: String,
    pub kind: FaultKind,
    pub at: f64,
    pub seconds: Option<f64>,
    /// `bus-error`: the probability per step of a failed transaction (0 to 1).
    pub rate: Option<f64>,
}

/// A scripted input level: the line is driven to `level` from `at` on.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GpioLevel {
    pub chip: String,
    pub line: u32,
    pub level: bool,
    #[serde(default)]
    pub at: f64,
}

/// A script that cannot be used.
#[derive(Debug)]
pub struct ScriptError(pub String);

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ScriptError {}

impl Script {
    /// Parses and checks a script.
    pub fn from_toml_str(text: &str) -> Result<Self, ScriptError> {
        let script: Self = toml::from_str(text).map_err(|e| ScriptError(e.to_string()))?;
        script.check()?;
        Ok(script)
    }

    /// Reads a script file.
    pub fn from_path(path: &Path) -> Result<Self, ScriptError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ScriptError(format!("{}: {e}", path.display())))?;
        Self::from_toml_str(&text).map_err(|e| ScriptError(format!("{}: {e}", path.display())))
    }

    fn check(&self) -> Result<(), ScriptError> {
        let bad = |what: String| Err(ScriptError(what));
        if self.format != FORMAT {
            return bad(format!("format {:?}, expected {FORMAT:?}", self.format));
        }
        if self.schema_version != SCHEMA_VERSION {
            return bad(format!(
                "schema_version {}, this build reads {SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        if !(self.speed > 0.0 && self.speed.is_finite()) {
            return bad("speed must be a positive number".into());
        }
        for (i, segment) in self.motion.iter().enumerate() {
            let seconds = segment.seconds();
            if !(seconds > 0.0 && seconds.is_finite()) {
                return bad(format!("motion {}: seconds must be positive", i + 1));
            }
            if let Segment::Shake { hz, .. } = segment
                && !(*hz > 0.0 && hz.is_finite())
            {
                return bad(format!("motion {}: hz must be positive", i + 1));
            }
        }
        for load in &self.loads {
            if load.seconds < 0.0 || load.amps < 0.0 {
                return bad("load: seconds and amps must not be negative".into());
            }
        }
        for fault in &self.faults {
            if fault.at < 0.0 || fault.seconds.is_some_and(|s| s < 0.0) {
                return bad(format!(
                    "fault on {:?}: times must not be negative",
                    fault.device
                ));
            }
            match fault.rate {
                Some(rate) if !(0.0..=1.0).contains(&rate) => {
                    return bad(format!("fault on {:?}: rate must be 0 to 1", fault.device));
                }
                Some(_) if fault.kind != FaultKind::BusError => {
                    return bad(format!(
                        "fault on {:?}: rate is for bus-error only",
                        fault.device
                    ));
                }
                _ => {}
            }
        }
        if self.fan.time_constant_s <= 0.0 || self.thermal.time_constant_s <= 0.0 {
            return bad("time constants must be positive".into());
        }
        Ok(())
    }
}
