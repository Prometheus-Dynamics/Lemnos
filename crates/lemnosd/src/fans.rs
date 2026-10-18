//! Handing fans back to the kernel: the plans `lemnosd` records when it binds
//! a fan, the state file a stop helper reads after the process is gone, the
//! restore on panic, and the helper itself (`lemnos-ctl fan restore`). The
//! board-independent parts live in `lemnos_drivers_linux::fan_restore`, the
//! board's fans in `lemnos_board::fans`; this module keeps `lemnosd`'s paths.
//!
//! See [`lemnos_drivers_linux::FanRestore`] for the two kinds of fan
//! (`pwm-fan` and other cooling-device fans, and chips with an automatic
//! mode).

use lemnos_board::BoardDefinition;
use lemnos_drivers_linux::{FanRestore, SysRoot};
use std::path::Path;

pub use lemnos_board::fans::{FAN_DRIVER, automatic_mode, find_fan};
pub use lemnos_drivers_linux::{
    FAN_STATE_FILE, Restored, fan_state_path, read_fan_state, write_fan_state,
};

/// Applies each plan, ignoring errors (for a panic hook).
pub fn restore_fans(plans: &[FanRestore]) {
    for plan in plans {
        let _ = plan.apply();
    }
}

/// The stop helper for the board `board` (its fans, then `state`'s records,
/// then, with `all`, every hwmon fan): see
/// [`lemnos_drivers_linux::restore_after_stop`].
pub fn restore_after_stop(
    board: Option<&BoardDefinition>,
    state: Option<&Path>,
    sys: &SysRoot,
    all: bool,
) -> Vec<Restored> {
    let fans = lemnos_board::fans::board_fans(board, sys);
    lemnos_drivers_linux::restore_after_stop(&fans, state, sys, all)
}
