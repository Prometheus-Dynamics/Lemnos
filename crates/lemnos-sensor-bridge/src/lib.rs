//! The Lemnos sensor bridge client: the userspace side of `kernel-modules/lemnos-sensor-bridge`
//! (`PROTOCOL.md`): pad format, timing controls, power, stream start/stop handshake.
//!
//! The bridge module is the sensor subdevice the CSI-2 receiver sees. A userspace sensor driver
//! opens the bridge's subdev node with [`SensorBridge::open`] and works through the handshake in
//! [`bridge`]. The V4L2 calls come from `lemnos-v4l2`; this crate has no `unsafe` of its own.
//!
//! The generic buses a userspace sensor driver also needs (I²C over i2c-dev, GPIO over the
//! character device, embedded-hal over them, kernel uevents) are `lemnos-linux`'s
//! (`lemnos_linux::hal`, `lemnos_linux::uevent`).

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

pub mod bridge;
mod bridge_sys;

pub use bridge::{
    BridgeLocation, PadFormat, SensorBridge, StreamAction, StreamRequest, StreamState, Timing,
    find_bridges,
};
