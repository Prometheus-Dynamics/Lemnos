use crate::{
    Runtime, RuntimeBackends, RuntimeConfig, RuntimeError, RuntimeEventCursor,
    RuntimeEventRetentionStats, RuntimeEventSubscription, RuntimeFailureRecord,
    RuntimeRefreshReport, RuntimeResult, RuntimeWatchedRefreshReport,
    runtime::{CompletedWatchRefresh, RefreshMode, WatchedRefreshMode, prepare_watch_refresh},
};
use lemnos_bus::{
    GpioBusBackend, I2cBusBackend, PwmBusBackend, SpiBusBackend, UartBusBackend, UsbBusBackend,
};
use lemnos_core::{DeviceId, DeviceRequest, DeviceResponse, DeviceStateSnapshot, LemnosEvent};
use lemnos_discovery::{
    DiscoveryContext, DiscoveryProbe, InventorySnapshot, InventoryWatcher, run_probes,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;
use thiserror::Error;
use tokio::task::JoinError;

macro_rules! runtime_debug_async {
    ($($arg:tt)*) => {
        crate::runtime::runtime_debug!($($arg)*)
    };
}

macro_rules! runtime_info_async {
    ($($arg:tt)*) => {
        crate::runtime::runtime_info!($($arg)*)
    };
}

macro_rules! runtime_warn_async {
    ($($arg:tt)*) => {
        crate::runtime::runtime_warn!($($arg)*)
    };
}

mod refresh;
mod requests;
mod state;
mod subscription;
mod sync;

pub type SharedDiscoveryProbe = Arc<dyn DiscoveryProbe>;
pub type AsyncRuntimeResult<T> = Result<T, AsyncRuntimeError>;

pub(crate) type RefreshOperation = RefreshMode;

#[derive(Debug, Error)]
pub enum AsyncRuntimeError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("tokio blocking task failed: {0}")]
    Join(#[from] JoinError),
}

/// Tokio-backed adapter over the synchronous [`Runtime`].
///
/// This type intentionally preserves the synchronous runtime semantics instead
/// of attempting to make the underlying driver and discovery layers natively
/// async. Mutating and waiting operations are offloaded onto Tokio's blocking
/// pool, while shared runtime state is protected by an `RwLock` so read-heavy
/// queries can proceed independently of unrelated mutations.
///
/// This is best treated as an async integration surface for control-plane
/// workloads, not as a high-fanout non-blocking event system.
///
/// Methods without an `_async` suffix are synchronous escape hatches. They may
/// block the current thread on `std::sync` locks, so prefer the `_async`
/// variants when calling from a Tokio task.
///
/// Internal lock domains:
/// - runtime state is protected by `RwLock<Runtime>`
/// - watcher state is protected by `Mutex<W>`
/// - retained-event subscription cursors are protected by `Mutex<RuntimeEventSubscription>`
///
/// Lock ordering:
/// - watcher/subscription mutex -> runtime read lock is allowed
/// - runtime lock -> watcher/subscription mutex is intentionally avoided
/// - runtime write lock is taken only after any watcher/subscription mutex has been released
pub struct AsyncRuntime {
    pub(crate) inner: Arc<RwLock<Runtime>>,
    pub(crate) bind_locks: Arc<Mutex<BTreeMap<DeviceId, Weak<Mutex<()>>>>>,
}

/// Async handle around a blocking [`InventoryWatcher`].
///
/// Like [`AsyncRuntime`], this wrapper keeps the watcher on Tokio's blocking
/// pool rather than converting it into a non-blocking primitive.
pub struct AsyncInventoryWatcher<W> {
    pub(crate) inner: Arc<Mutex<W>>,
}

/// Async subscription wrapper for retained runtime events.
///
/// Waiting methods block on a condition variable inside Tokio's blocking pool.
/// They are convenient for async integration, but they are not a high-fanout
/// non-blocking broadcast channel.
///
/// Cloning this type keeps one shared subscription cursor. Multiple clones
/// therefore coordinate on the same pending-event stream instead of each
/// receiving an independent copy of retained events.
///
/// Methods without an `_async` suffix are synchronous escape hatches. Prefer
/// the `_async` variants from async code so subscription inspection also stays
/// off executor threads.
pub struct AsyncRuntimeEventSubscription {
    pub(crate) runtime: Arc<RwLock<Runtime>>,
    pub(crate) subscription: Arc<Mutex<RuntimeEventSubscription>>,
}
