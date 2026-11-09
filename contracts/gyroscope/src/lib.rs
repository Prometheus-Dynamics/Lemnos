//! The `gyroscope/1` contract.

#![no_std]

use lemnos_core::{Result, Vector3, contract};

#[contract("gyroscope", version = 1)]
pub trait Gyroscope {
    /// Angular rate in rad/s.
    #[method(1)]
    fn read(&mut self) -> Result<Vector3>;
}
