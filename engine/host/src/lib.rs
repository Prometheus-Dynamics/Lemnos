//! Runs drivers: reads a board file, binds devices children first, carries
//! calls to them, and keeps them running through panics, hangs and errors.
//!
//! Nothing here knows what any device is. See `docs/design.md`.

pub mod board;
mod contain;
mod host;

pub use contain::{AutoOff, Options};
pub use host::{DeviceInfo, DeviceState, DriverEntry, Host, HostError, OffReason, entry};
