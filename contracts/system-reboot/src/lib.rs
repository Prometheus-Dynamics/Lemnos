//! The `system.reboot/1` contract: restarts the board, and into its way back where the board file
//! declares one (`reboot_to`, an addition to version 1 that moves no hash).

#![no_std]

use lemnos_core::{Result, contract};

#[contract("system.reboot", version = 1, kind = "system")]
pub trait Reboot {
    /// Asks the system to restart. Returns once the restart is under way.
    #[method(1)]
    fn reboot(&mut self) -> Result<()>;
    /// Restarts the board into its way back, `target`, the board file's `[boot] loop_return` (today
    /// only `"rpiboot"`: the firmware's flashing mode, for the next boot only). The board's firmware is
    /// asked first; then the restart goes as `reboot`'s does. Returns once the restart is under way.
    ///
    /// Refused, with nothing asked and nothing restarted: `NOT_SUPPORTED` when the board file declares
    /// no way back, or the board has no firmware to ask; `INVALID_ARGUMENT` for a `target` that is not
    /// the declared one; `NOT_PERMITTED` while the board's security switch is on. `IO` when the
    /// firmware does not take the request (nothing is restarted) or the restart fails.
    #[method(2)]
    #[added]
    fn reboot_to(&mut self, target: &str) -> Result<()>;
}
