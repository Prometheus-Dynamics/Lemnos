//! The K230 USB-boot host: how the boot ROM's download mode loads a RAM image, how the burner
//! protocol writes and verifies a medium, the flasher's write order, and a simulated board that
//! runs the stage's own code.
//!
//! The pieces:
//! - [`transport`]: the USB operations the host needs, as a trait. This crate opens no device.
//! - [`bootrom`]: the boot ROM's requests (CPU info, the load address, the start).
//! - [`packet`], [`burner`], [`write`]: the burner protocol, the reads and the write path.
//! - [`flash`]: the flasher. Its order never leaves a valid header over an incomplete payload.
//! - [`usb`]: the host's USB transport over libusb (`rusb`). It opens a device only when asked.
//! - [`b2`]: the host's side of the B2 run (the FL1 stage's own USB device path), over traits; [`b2sim`]
//!   is the rehearsal's board for it (the check build's code over a register model of the dwc2 core).
//! - [`sim`]: a simulated board. Its boot ROM, its stage (the `lemnos-k230-usbboot-stage` code)
//!   and its watchdog model, with power cuts and the faults a real medium can show.
//!
//! The facts each request depends on are the vendor sources' (see `docs/flashing.md`). Nothing
//! here has run on a board.

pub mod b2;
pub mod b2sim;
pub mod bootrom;
pub mod burner;
pub mod console;
pub mod flash;
pub mod packet;
pub mod probe;
pub mod sim;
pub mod stage_report;
pub mod transport;
pub mod usb;
pub mod write;

pub use transport::{UsbError, UsbIo};

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
