//! The `orientation/1` contract.

#![no_std]

use lemnos_core::{Result, Vector3, contract};

#[contract("orientation", version = 1, kind = "sensor")]
pub trait Orientation {
    /// Roll, pitch and yaw in radians.
    #[method(1, reading = three_axes)]
    fn read(&mut self) -> Result<Vector3>;
}
