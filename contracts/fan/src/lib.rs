//! The `fan/1` contract: one fan. Duty is per-mille, 0 to 1000.

#![no_std]

use lemnos_core::{Result, contract};

#[contract("fan", version = 1)]
pub trait Fan {
    /// Takes the fan and sets its duty.
    #[method(1)]
    fn set_duty(&mut self, per_mille: u16) -> Result<()>;
    /// The duty last set, or read back from the hardware.
    #[method(2)]
    fn duty(&mut self) -> Result<u16>;
    /// Revolutions per minute, for a fan with a tachometer.
    #[method(3)]
    fn rpm(&mut self) -> Result<Option<u32>>;
    /// Hands the fan back to whoever controlled it before.
    #[method(4)]
    fn release(&mut self) -> Result<()>;
}
