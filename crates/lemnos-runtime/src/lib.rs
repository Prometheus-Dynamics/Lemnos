#![forbid(unsafe_code)]

#[cfg(feature = "tokio")]
mod async_runtime;
mod backends;
mod config;
mod diagnostics;
mod error;
mod runtime;
mod subscription;

#[cfg(feature = "tokio")]
pub use async_runtime::{
    AsyncInventoryWatcher, AsyncRuntime, AsyncRuntimeError, AsyncRuntimeEventSubscription,
    AsyncRuntimeResult, SharedDiscoveryProbe,
};
pub use backends::RuntimeBackends;
pub use config::{RuntimeConfig, RuntimeWatchRefreshMode};
pub use diagnostics::{RuntimeFailureCategory, RuntimeFailureOperation, RuntimeFailureRecord};
pub use error::{RuntimeError, RuntimeResult};
pub use lemnos_registry::DriverId;
pub use runtime::{
    Runtime, RuntimeEventRetentionStats, RuntimeRebindReport, RuntimeRefreshReport,
    RuntimeWatchedRefreshReport,
};
pub use subscription::{RuntimeEventCursor, RuntimeEventPoll, RuntimeEventSubscription};

#[cfg(test)]
mod tests;
