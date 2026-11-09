//! IMU orientation fusion, for `no_std` targets.
//!
//! - [`Orientation`]: a Mahony or Madgwick filter over the gyroscope,
//!   accelerometer and (optionally) magnetometer, with variable `dt`, magnetic
//!   disturbance rejection and a mount rotation. Output: quaternion, roll,
//!   pitch, yaw, gravity and linear acceleration in the body frame.
//!
//! Nothing here allocates. All arithmetic is `f32`, and the square root,
//! trigonometry and linear solves it needs are its own (see `math`), so it
//! builds for bare-metal targets without `libm`. Per-sample work is fixed: no
//! loops over large arrays, no per-sample allocation.
//!
//! Frames: the sensor's own axes are the sensor frame; the robot's frame is
//! the body (the sensor frame rotated by the mount); the world is z up and x
//! toward magnetic north. Yaw is referenced to magnetic north only in
//! nine-axis mode with a calibrated magnetometer. See the `Orientation` type
//! for the exact conventions.
//!
//! Timestamps are `u64` microseconds on any monotonic clock. The driver in
//! the parent module has no clock, so it supplies its own (see `lib.rs`).

mod math;
mod orientation;
mod quat;

#[cfg(test)]
mod testutil;

pub use orientation::{Algorithm, Mode, Orientation, OrientationConfig, Output};
pub use quat::Quat;

/// A 3-vector `[x, y, z]`.
pub type Vec3 = [f32; 3];

/// Standard gravity, m/s².
pub const STANDARD_GRAVITY: f32 = 9.80665;
