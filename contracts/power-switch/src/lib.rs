//! The `power-switch/1` contract: a switch that enables a load.

#![no_std]

use lemnos_core::{Result, contract};

#[contract("power-switch", version = 1)]
pub trait PowerSwitch {
    #[method(1)]
    fn set(&mut self, on: bool) -> Result<()>;
    #[method(2)]
    fn is_on(&mut self) -> Result<bool>;
    /// Whether the switch reports a fault. `None` when it has no fault input.
    #[method(3)]
    fn fault(&mut self) -> Result<Option<bool>>;
}
