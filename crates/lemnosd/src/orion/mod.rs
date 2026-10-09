//! The Orion bridge (`lemnos-orion`, `docs/orion.md`): lemnosd's devices as
//! Orion resources, readings and actions, over local IPC on both sides.
//!
//! - [`mirror`]: resource records, status entries, deadbands (no I/O);
//! - [`ops`]: action arguments and outcomes (no I/O);
//! - [`bridge`]: the runtime that connects lemnosd and Orion.

pub mod bridge;
pub mod mirror;
pub mod ops;
