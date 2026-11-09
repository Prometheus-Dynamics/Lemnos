//! The `accelerometer/1` contract.

#![no_std]

use lemnos_core::{Result, Vector3, contract};

#[contract("accelerometer", version = 1)]
pub trait Accelerometer {
    /// Acceleration in m/s².
    #[method(1)]
    fn read(&mut self) -> Result<Vector3>;
}
