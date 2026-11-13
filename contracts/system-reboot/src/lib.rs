//! The `system.reboot/1` contract: restarts the board.

#![no_std]

use lemnos_core::{Result, contract};

#[contract("system.reboot", version = 1)]
pub trait Reboot {
    /// Asks the system to restart. Returns once the restart is under way.
    #[method(1)]
    fn reboot(&mut self) -> Result<()>;
}
