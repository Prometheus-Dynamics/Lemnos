//! Reads happen when someone wants them. Subscriptions decide how often and
//! which values a device reads; [`rules`] decides which device goes next.

pub mod rules;

mod plan;

pub use plan::{Action, DevicePace, Grant, ReadPlan, Refusal, Scheduler};
