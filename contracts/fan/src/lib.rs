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
    /// Puts the fan in its safe state, full duty, for a failure: a write that will not go, or a
    /// controller that cannot run. Nothing hands the fan to another controller.
    #[method(4)]
    fn release(&mut self) -> Result<()>;
    /// Hands the fan back the way it was before this driver took it: the duty and the mode it had
    /// then. A controller that is stopped or disabled calls this. A driver that recorded nothing
    /// when it took the fan (`pwm-channel-fan`, whose safe state is its baseline) puts it at full duty.
    /// Marked `#[added]`: the contract's base hash does not move, and a device without the method is
    /// checked per method.
    #[method(5)]
    #[added]
    fn hand_back(&mut self) -> Result<()>;
}
