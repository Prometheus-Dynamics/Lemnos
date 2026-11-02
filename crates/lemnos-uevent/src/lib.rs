//! Linux device watching, shared by Lemnos and the camera stack.
//!
//! - [`uevent`]: the kernel's uevent messages, read from netlink (`UeventSocket`)
//!   and parsed without allocation (`UeventMsg`).
//! - [`device_watch`]: [`DeviceWatcher`](device_watch::DeviceWatcher), one watcher over
//!   netlink, inotify or a sysfs rescan, for any set of subsystems, and
//!   `AsyncDeviceWatcher` with the `tokio` feature.
//! - [`paths`]: the sysfs and dev roots a watcher reads, `LinuxPaths`.
//!
//! This crate sits below `lemnos-linux` and `lemnos-usb`, which both watch
//! hotplug through it. It has no device code: a consumer decides what an event
//! means.

#![forbid(unsafe_code)]

#[macro_use]
mod trace;

pub mod device_watch;
pub mod paths;
pub mod uevent;
mod util;

pub use device_watch::{DeviceWatchSource, DeviceWatcher, Filter};
pub use paths::LinuxPaths;
