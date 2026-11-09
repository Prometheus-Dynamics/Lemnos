//! The `temperature/1` contract.

#![no_std]

use lemnos_core::{Result, contract};

#[contract("temperature", version = 1)]
pub trait Temperature {
    /// Degrees Celsius.
    #[method(1)]
    fn read(&mut self) -> Result<f32>;
}
