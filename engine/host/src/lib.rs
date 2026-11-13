//! Runs drivers: reads a board file, binds devices children first, carries
//! calls to them, and keeps them running through panics, hangs and errors.
//!
//! Nothing here knows what any device is. See `docs/design.md`.

pub mod board;
mod contain;
mod guest;
mod host;
pub mod schedule;
pub mod stream;

pub use contain::{MAX_RETRY_CAP, Options};
pub use host::{
    Binder, DeviceInfo, DeviceState, DriverEntry, Host, HostError, Instance, JOB_RESULT_MAX,
    JobEvent, JobUpdate, Liveness, MAX_JOBS, OffReason, entry,
};
