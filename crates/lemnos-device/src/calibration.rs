//! Calibration as the device model sees it: what a sensor reports about its
//! own calibration, the commands a host (lemnosd, for Atlas) can send it, and
//! the words a host persists so the calibration survives a restart.
//!
//! The estimators and their maths live in the driver (they are shared through
//! `lemnos-fusion`). This module is only the plain-data contract between a
//! driver and its host: no allocation, fixed sizes, `i32` words.
//! `docs/imu-calibration-fusion.md` describes the behaviour.

/// The most words a sensor's persisted calibration takes (see
/// [`Sensor::calibration_words`](crate::Sensor::calibration_words)).
pub const MAX_CALIBRATION_WORDS: usize = 64;

/// The index of the accelerometer's part in [`CalibrationStatus::parts`] (an
/// IMU), or the magnetometer's part of a magnetometer.
pub const PART_ACCEL: usize = 0;
/// The gyroscope's part (an IMU only).
pub const PART_GYRO: usize = 1;
/// The magnetometer's part (a magnetometer only).
pub const PART_MAG: usize = 2;

/// A guided, forced calibration routine (`Start`). Automatic calibration runs
/// all the time and needs no routine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CalibrationRoutine {
    /// Hold the device still on each of its six faces (+X, -X, +Y, -Y, +Z,
    /// -Z) in turn: the accelerometer's offset and scale.
    AccelSix,
    /// Rotate the device through as many orientations as possible: the
    /// magnetometer's hard- and soft-iron correction.
    MagRotate,
    /// Hold the device still for a few seconds: the gyroscope's zero-rate
    /// offset.
    GyroHold,
}

impl CalibrationRoutine {
    /// The name a client writes (`accel-six`, `mag-rotate`, `gyro-hold`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::AccelSix => "accel-six",
            Self::MagRotate => "mag-rotate",
            Self::GyroHold => "gyro-hold",
        }
    }

    /// The routine a name (see [`name`](Self::name)) selects.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "accel-six" => Some(Self::AccelSix),
            "mag-rotate" => Some(Self::MagRotate),
            "gyro-hold" => Some(Self::GyroHold),
            _ => None,
        }
    }
}

/// A command a host sends a sensor's calibration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CalibrationCommand {
    /// Starts a guided routine (ends any other running one).
    Start(CalibrationRoutine),
    /// Ends the running routine, keeping any candidate it has produced.
    Stop,
    /// Makes the candidate of the last routine the applied calibration.
    Apply,
    /// Drops the candidate.
    Discard,
    /// Returns every part to its factory state (no correction, no
    /// confidence); the automatic estimators start again.
    Reset,
}

/// One part of a sensor's calibration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CalibrationPart {
    /// Samples the estimator has used (since the last reset, with forgetting
    /// where the estimator forgets).
    pub samples: u32,
    /// How much the applied correction can be trusted, 0 to 1000 (1000 is
    /// full confidence). 0 means not applied.
    pub confidence: u16,
    /// How well the samples cover the sensor's directions, 0 to 1000.
    pub coverage: u16,
    /// The fit's residual relative to the reference, in parts per thousand.
    pub residual: u16,
    /// Whether the applied correction is in use (confidence reached the
    /// threshold).
    pub active: bool,
}

/// A sensor's calibration status (see
/// [`Sensor::calibration_status`](crate::Sensor::calibration_status)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CalibrationStatus {
    /// Counts every change to the applied calibration. A host persists the
    /// words when it changes.
    pub revision: u32,
    /// The routine running now.
    pub running: Option<CalibrationRoutine>,
    /// How far the running routine is, 0 to 1000.
    pub progress: u16,
    /// A routine produced a candidate that `Apply` would make applied.
    pub candidate: bool,
    /// The last routine ended in failure (not enough data or a poor fit).
    pub failed: bool,
    /// Indexed by [`PART_ACCEL`], [`PART_GYRO`], [`PART_MAG`]. A part the
    /// device does not have is all zeros.
    pub parts: [CalibrationPart; 3],
}
