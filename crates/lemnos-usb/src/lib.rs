//! One USB layer for Lemnos.
//!
//! Listing, hotplug, descriptors, control and bulk and interrupt transfers,
//! and isochronous transfers on Linux, behind Lemnos's own types. A consumer
//! imports this crate, not the platform's USB stack.
//!
//! | Platform | Backend | Isochronous |
//! |----------|---------|-------------|
//! | Linux | usbdevfs (`lemnos-linux-sys`): ioctls and URBs | yes |
//! | Windows | nusb over WinUSB | no |
//! | macOS | nusb over IOKit | no |
//!
//! Listing and hotplug use nusb on every platform. There is no libusb.
//!
//! Transfers come in two forms. The blocking calls (`bulk_read`, `control_in`,
//! ...) return when the transfer finishes. The submit calls (`submit_bulk_in`,
//! ...) return a [`Transfer`] at once: await it (it is a future) or wait for it
//! on a thread. [`UsbDevice::bulk_reader`] and [`UsbDevice::bulk_writer`] keep
//! several transfers in flight behind `std::io::Read` and `Write`.
//!
//! ```no_run
//! use lemnos_usb::{ControlKind, ControlRecipient, ControlSetup, Direction, UsbDevice};
//! use std::time::Duration;
//!
//! # fn main() -> lemnos_usb::Result<()> {
//! let info = lemnos_usb::list_devices()?.into_iter().next().expect("a device");
//! let device = UsbDevice::open(&info)?;
//! let _interface = device.claim_interface(0)?;
//! let setup = ControlSetup {
//!     direction: Direction::In,
//!     kind: ControlKind::Vendor,
//!     recipient: ControlRecipient::Device,
//!     request: 0x01,
//!     value: 0,
//!     index: 0,
//! };
//! let reply = device.control_in(&setup, 64, Duration::from_secs(1))?;
//! # let _ = reply;
//! # Ok(())
//! # }
//! ```
//!
//! Buffers are owned `Vec<u8>`s that move into a transfer and come back in its
//! [`TransferOutcome`], so a caller that reuses them does not allocate per
//! transfer. On Linux they are the kernel's copy source and target directly;
//! on Windows and macOS nusb copies them into its own buffers.

mod buffer;
mod config;
mod descriptor;
mod device;
mod error;
mod info;
#[cfg(target_os = "linux")]
mod linux;
mod listing;
mod location;
#[cfg(not(target_os = "linux"))]
mod native;
mod stream;
mod transfer;
mod types;
mod wait;

#[cfg(test)]
mod tests;

pub use config::{AlternateSetting, ConfigurationInfo, EndpointInfo, InterfaceInfo};
pub use descriptor::DeviceDescriptor;
pub use device::{Interface, UsbDevice};
pub use error::{Error, ErrorKind, Result};
pub use info::{InterfaceSummary, Speed, UsbDeviceInfo};
pub use listing::{UsbHotplugEvent, UsbWatch, find_device, list_devices, watch};
pub use location::UsbLocation;
#[cfg(target_os = "linux")]
pub use stream::IsoReader;
pub use stream::{BulkReader, BulkWriter};
pub use stream::{IsoBlock, IsoPackets};
pub use transfer::{IsoPacket, Transfer, TransferBuffer, TransferOutcome, TransferStatus};
pub use types::{ControlKind, ControlRecipient, ControlSetup, Direction, TransferType};
