//! IMU orientation fusion and sensor calibration, for `no_std` targets.
//!
//! - [`Orientation`]: a Mahony or Madgwick filter over the gyroscope,
//!   accelerometer and (optionally) magnetometer, with variable `dt`, magnetic
//!   disturbance rejection and a mount rotation. Output: quaternion, roll,
//!   pitch, yaw, gravity and linear acceleration in the body frame.
//! - [`EllipsoidFit`]: an online ellipsoid fit (offset, scale and soft iron)
//!   used for accelerometer and magnetometer calibration.
//! - [`ImuCalibrator`] and [`MagCalibrator`]: automatic calibration on every
//!   sample (gyro zero rate, accelerometer and magnetometer ellipsoids), plus
//!   forced [`Routine`]s that produce a candidate applied with `apply()`.
//! - [`words`]: the persisted layout of an applied calibration, as `i32` words.
//!
//! The crate is `#![no_std]` and does not allocate. All arithmetic is `f32`,
//! and the square root, trigonometry and linear solves it needs are its own
//! (see `math`), so it builds for bare-metal and wasm targets without `libm`.
//! Per-sample work is fixed: no loops over large arrays, no per-sample
//! allocation.
//!
//! Frames: the sensor's own axes are the sensor frame; the robot's frame is
//! the body (the sensor frame rotated by the mount); the world is z up and x
//! toward magnetic north. Yaw is referenced to magnetic north only in
//! nine-axis mode with a calibrated magnetometer. See the `Orientation` type for the
//! exact conventions.
//!
//! Timestamps are `u64` microseconds on any monotonic clock.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

mod calibrate;
mod ellipsoid;
mod math;
mod orientation;
mod quat;
pub mod words;

#[cfg(test)]
mod testutil;

pub use calibrate::{Changed, ImuCalibrator, MagCalibrator, PartStatus, Routine};
pub use ellipsoid::{Ellipsoid, EllipsoidFit, Fit};
pub use orientation::{Algorithm, Mode, Orientation, OrientationConfig, Output};
pub use quat::Quat;
pub use words::WordsError;

/// A 3-vector `[x, y, z]`.
pub type Vec3 = [f32; 3];

/// Standard gravity, m/s².
pub const STANDARD_GRAVITY: f32 = 9.80665;
