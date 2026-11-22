//! `lemnos-way-back rpiboot [--device NODE]`: asks the board's firmware for its way back at the next
//! boot, with no daemon and no board file, so an initramfs `/init` (a RAM-booted image) can arm its own
//! return before lemnosd exists.
//!
//! The one way back is `rpiboot`: the Raspberry Pi firmware's reboot order for the NEXT BOOT ONLY,
//! RPIBOOT (the flashing mode), set through the property mailbox (`/dev/vcio`, tag `0x0003808b`,
//! value 3), the same request `lemnos-slot-init` makes after a run of failed boots and
//! `system.reboot/1`'s `reboot_to` makes on request (`lemnos_driver_rpi_firmware_sys::Mailbox::
//! request_rpiboot`). It does not restart: the caller does, or the hardware watchdog does.
//!
//! The order applies to whatever ends this boot, a restart that was meant to go elsewhere included.
//! There is no request here that takes it back: no value that clears it is documented in the sources
//! this was written from, so none is sent.

use std::path::{Path, PathBuf};

pub use lemnos_driver_rpi_firmware_sys::DEFAULT_DEVICE;

/// What the program is asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// The mailbox's node.
    pub device: PathBuf,
}

/// The usage line, the answer to anything that is not a request.
pub const USAGE: &str = "usage: lemnos-way-back rpiboot [--device NODE]";

/// Reads the command line (without the program's name).
///
/// # Errors
///
/// The usage line, for a command line that is not `rpiboot [--device NODE]`.
pub fn parse(args: &[String]) -> Result<Ask, String> {
    let mut words = args.iter();
    match words.next().map(String::as_str) {
        Some("rpiboot") => {}
        Some(other) => {
            return Err(format!(
                "{other:?} is not a way back: the only one is \"rpiboot\"; {USAGE}"
            ));
        }
        None => return Err(USAGE.to_string()),
    }
    let mut device = PathBuf::from(DEFAULT_DEVICE);
    while let Some(word) = words.next() {
        match (word.as_str(), words.next()) {
            ("--device", Some(node)) if !node.is_empty() => device = PathBuf::from(node),
            _ => return Err(USAGE.to_string()),
        }
    }
    Ok(Ask { device })
}

/// The firmware's mailbox, as the program uses it: one request.
pub trait Firmware {
    /// Asks for RPIBOOT at the next boot through the node at `device`.
    ///
    /// # Errors
    ///
    /// The node could not be opened, or the firmware did not take the request.
    fn request_rpiboot(&self, device: &Path) -> std::io::Result<()>;
}

/// The board's own mailbox.
pub struct Mailbox;

impl Firmware for Mailbox {
    fn request_rpiboot(&self, device: &Path) -> std::io::Result<()> {
        lemnos_driver_rpi_firmware_sys::Mailbox::open(device)?.request_rpiboot()
    }
}

/// Runs the request: the line to print on success, or why it failed.
///
/// # Errors
///
/// Why the way back is not armed: the node, and what the firmware or the system said.
pub fn run(ask: &Ask, firmware: &impl Firmware) -> Result<String, String> {
    firmware
        .request_rpiboot(&ask.device)
        .map(|()| {
            format!(
                "the firmware took RPIBOOT for the next boot (through {}); the next restart, whatever asks for it, goes there",
                ask.device.display()
            )
        })
        .map_err(|e| {
            format!(
                "the way back is not armed: RPIBOOT through {}: {e}",
                ask.device.display()
            )
        })
}

#[cfg(test)]
mod tests;
