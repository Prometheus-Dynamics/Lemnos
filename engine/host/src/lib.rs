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

pub use contain::{MAX_RETRY_CAP, MIN_READ_FLOOR, Options};
pub use host::{
    Binder, CallSink, CallTrace, DeviceInfo, DeviceState, DriverEntry, Host, HostError, Instance,
    JOB_RESULT_MAX, JobEvent, JobUpdate, Liveness, MAX_JOBS, OffReason, REFUSAL_WORDS, Stall,
    check_settings, entry, set_start_trace, set_warning_sink, settings_refusals,
    with_device_config,
};
