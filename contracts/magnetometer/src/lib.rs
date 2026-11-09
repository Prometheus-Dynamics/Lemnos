//! The `magnetometer/1` contract.

#![no_std]

use lemnos_core::{Result, Vector3, contract};

#[contract("magnetometer", version = 1)]
pub trait Magnetometer {
    /// Magnetic field in tesla.
    #[method(1)]
    fn read(&mut self) -> Result<Vector3>;
}
