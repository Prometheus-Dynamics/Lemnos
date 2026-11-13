//! The `power-switch/1` contract: a switch that enables a load.

#![no_std]

use lemnos_core::{Result, contract};

#[contract("power-switch", version = 1, kind = "power")]
pub trait PowerSwitch {
    #[method(1, reading = switch)]
    fn set(&mut self, on: bool) -> Result<()>;
    #[method(2, reading = switch)]
    fn is_on(&mut self) -> Result<bool>;
    /// Whether the switch reports a fault. `None` when it has no fault input.
    #[method(3, reading = switch)]
    fn fault(&mut self) -> Result<Option<bool>>;
}
