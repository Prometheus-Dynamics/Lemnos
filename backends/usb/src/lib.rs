//! The USB host on Linux: [`UsbHost`] and [`UsbDevice`] over usbfs.
//!
//! [`UsbfsHost`] lists what's plugged in from sysfs and opens the device nodes under
//! `/dev/bus/usb`. Both roots can be set, so tests run on fixture directories.
//!
//! [`UsbfsDevice`] carries control, bulk and interrupt transfers on the kernel's synchronous
//! ioctls and isochronous transfers on a ring of URBs. Nothing here keeps a device from being
//! retried: every failure goes back to the caller, and a URB the kernel holds on to past a
//! timeout makes the next isochronous call return `Busy` until it comes back.
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
