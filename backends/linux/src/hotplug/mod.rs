//! Watching devices appear and go away, from `lemnos-linux-hotplug` (re-exported here whole), and
//! [`BoardHints`]: a watcher's events as the hints a host takes, which of a board file's names may
//! have changed.

mod hints;

#[cfg(test)]
mod tests;

pub use hints::{BOARD, BoardHints, USB, UsbHints};
pub use lemnos_linux_hotplug::*;
