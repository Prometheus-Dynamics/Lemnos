//! The `fan/1` contract: one fan. Duty is per-mille, 0 to 1000.

#![no_std]

use lemnos_core::{Result, contract};

#[contract("fan", version = 1, kind = "actuator")]
pub trait Fan {
    /// Takes the fan and sets its duty.
    #[method(1, reading = range)]
    fn set_duty(&mut self, per_mille: u16) -> Result<()>;
    /// The duty last set, or read back from the hardware.
    #[method(2, reading = number)]
    fn duty(&mut self) -> Result<u16>;
    /// Revolutions per minute, for a fan with a tachometer.
    #[method(3, reading = number)]
    fn rpm(&mut self) -> Result<Option<u32>>;
    /// Puts the fan in its safe state, full duty. Nothing hands the fan to another controller.
    #[method(4)]
    fn release(&mut self) -> Result<()>;
}
