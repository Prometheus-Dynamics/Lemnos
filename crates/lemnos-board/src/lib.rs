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
mod gravity;
mod i2c_select;
mod light;
pub mod looks;
pub mod raw;
mod registry;
mod schema;

#[cfg(test)]
mod looks_tests;
#[cfg(test)]
mod tests;

#[cfg(feature = "linux")]
pub use buses::LinuxBuses;
pub use buses::{Buses, DynI2c, DynInputPin, DynOutputPin, GpioRef};
pub use error::BoardError;
pub use gravity::{
    Bottom, FLAT_FRACTION, GRAVITY_KEYS, Gravity, gravity as gravity_config, parse_axis,
};
pub use i2c_select::I2cSelector;
pub use light::{LIGHT_KEYS, light_defaults, strip_config};
pub use registry::{Build, DriverEntry, DriverRegistry, Interface};
pub use schema::{
    Backend, BoardDefinition, BoardInfo, BusRef, ConfigValue, DeviceSpec, FORMAT, LineSpec,
    PwmSpec, SCHEMA_VERSION, client_matches, is_valid_client_entry, is_valid_id,
};
