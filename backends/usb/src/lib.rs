//! The USB host on Linux: [`UsbHost`] and [`UsbDevice`] over usbfs.
//!
//! [`UsbfsHost`] lists what's plugged in from sysfs and opens the device nodes under
//! `/dev/bus/usb`. Both roots can be set, so tests run on fixture directories.
//!
//! [`UsbfsDevice`] carries control, bulk and interrupt transfers on the kernel's synchronous
//! ioctls and an isochronous stream on a ring of URBs, every one of them submitted from the
//! start and submitted again as soon as it is handed over, so the kernel always has the next
//! frames' URBs. Nothing here keeps a device from being retried: every failure goes back to the
//! caller.
//!
//! [`UsbHost`]: lemnos_hal::UsbHost
//! [`UsbDevice`]: lemnos_hal::UsbDevice

#![cfg(target_os = "linux")]

mod descriptors;
mod device;
mod error;
mod host;

pub use device::UsbfsDevice;
pub use host::UsbfsHost;

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

#[cfg(test)]
mod tests;
