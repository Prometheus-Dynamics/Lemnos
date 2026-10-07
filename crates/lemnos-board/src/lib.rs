//! Lemnos board definitions.
//!
//! A board definition is a versioned TOML or JSON file that lists a board's
//! devices: which generic driver each uses, on which bus and address (or
//! which kernel class device), with which settings. It is written once per
//! board, by hand or generated (Atlas generates it from a device manifest),
//! and every host consumes the same file:
//!
//! - the full runtime (`lemnos` facade, feature `board`) turns each device
//!   into a configured descriptor and binds it through the generic
//!   device-model adapter;
//! - `lemnosd` hosts the devices directly;
//! - small Linux programs build them with [`DriverRegistry::build`] and put
//!   them in a `lemnos-lite` table.
//!
//! ```toml
//! format = "lemnos.board"
//! schema_version = 1
//!
//! [board]
//! id = "raze"
//!
//! [[devices]]
//! id = "imu"
//! driver = "bmi088"
//! bus = "i2c-1"
//! address = 0x18
//! config = { gyro_address = 0x68, accel_range = "6g" }
//! ```
//!
//! [`DriverRegistry`] maps driver names to factories that return a
//! [`BoxedDevice`](lemnos_device::BoxedDevice). A factory gets its buses
//! from the host through [`Buses`], and picks the backend: the chip's
//! userspace driver over the bus, or its mainline kernel driver (IIO, hwmon)
//! through `lemnos-drivers-linux`'s generic binding, which serves the same
//! channels. See `docs/board-definition.md`.

#![forbid(unsafe_code)]

mod buses;
mod error;
mod registry;
mod schema;

#[cfg(test)]
mod tests;

#[cfg(feature = "linux")]
pub use buses::LinuxBuses;
pub use buses::{Buses, DynI2c};
pub use error::BoardError;
pub use registry::{Build, DriverEntry, DriverRegistry, Interface};
pub use schema::{
    Backend, BoardDefinition, BoardInfo, BusRef, ConfigValue, DeviceSpec, FORMAT, SCHEMA_VERSION,
    is_valid_id,
};
