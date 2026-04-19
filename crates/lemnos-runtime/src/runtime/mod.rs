use crate::{
    RuntimeBackends, RuntimeConfig, RuntimeError, RuntimeFailureOperation, RuntimeFailureRecord,
    RuntimeResult,
};
use lemnos_core::{DeviceId, DeviceRequest, DeviceResponse, DeviceStateSnapshot, LemnosEvent};
use lemnos_discovery::{
    DiscoveryContext, DiscoveryProbe, DiscoveryRunReport, InventoryDiff, InventorySnapshot,
    InventoryWatchEvent, ProbeInventoryIndex, run_probes,
};
use lemnos_driver_sdk::{BoundDevice, Driver};
use lemnos_registry::DriverRegistry;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use crate::subscription::RuntimeEventNotifier;

mod events;
mod failures;
mod lifecycle;
mod operations;
mod refresh;
mod requests;
mod retention;
mod watch;

#[cfg(feature = "tokio")]
pub(crate) use operations::PreparedBindingOutput;
#[cfg(any(feature = "tokio", test))]
pub(crate) use operations::close_detached_bindings;
#[cfg(feature = "tokio")]
pub(crate) use refresh::RefreshMode;
#[cfg(feature = "tokio")]
pub(crate) use requests::interaction_name_owned;
#[cfg(feature = "tokio")]
pub(crate) use watch::{CompletedWatchRefresh, WatchedRefreshMode, prepare_watch_refresh};

#[cfg(feature = "tracing")]
macro_rules! runtime_debug {
    ($($arg:tt)*) => {
        { tracing::debug!($($arg)*) }
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! runtime_debug {
    ($($arg:tt)*) => {
        ()
    };
}

#[cfg(feature = "tracing")]
macro_rules! runtime_info {
    ($($arg:tt)*) => {
        { tracing::info!($($arg)*) }
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! runtime_info {
    ($($arg:tt)*) => {
        ()
    };
}

#[cfg(feature = "tracing")]
macro_rules! runtime_warn {
    ($($arg:tt)*) => {
        { tracing::warn!($($arg)*) }
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! runtime_warn {
    ($($arg:tt)*) => {
        ()
    };
}

pub(crate) use runtime_debug;
pub(crate) use runtime_info;
pub(crate) use runtime_warn;

pub(crate) type SharedBoundDevice = Arc<Mutex<Box<dyn BoundDevice>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Result of a refresh pass, including discovery output, inventory diff, and
/// any automatic rebind work triggered by the refresh.
pub struct RuntimeRefreshReport {
    pub discovery: DiscoveryRunReport,
    pub diff: InventoryDiff,
    pub rebinds: RuntimeRebindReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Result of a watcher-triggered refresh, including the watch events that
/// caused the refresh and the refresh report itself.
pub struct RuntimeWatchedRefreshReport {
    pub watch_events: Vec<InventoryWatchEvent>,
    pub refresh: RuntimeRefreshReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// Summary of auto-rebind attempts performed after a refresh.
pub struct RuntimeRebindReport {
    pub attempted: Vec<DeviceId>,
    pub rebound: Vec<DeviceId>,
    pub failed: Vec<DeviceId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Snapshot of the in-memory retained-event buffer state.
pub struct RuntimeEventRetentionStats {
    pub event_base_index: usize,
    pub event_tail_index: usize,
    pub retained_events: usize,
    pub retained_event_bytes: usize,
    pub max_retained_events: usize,
    pub max_retained_event_bytes: Option<usize>,
}

/// Embeddable synchronous runtime for discovery, binding, requests, and
/// retained event/state tracking.
pub struct Runtime {
    config: RuntimeConfig,
    running: bool,
    inventory: Arc<InventorySnapshot>,
    registry: DriverRegistry,
    states: BTreeMap<DeviceId, Arc<DeviceStateSnapshot>>,
    bindings: BTreeMap<DeviceId, SharedBoundDevice>,
    probe_inventory: ProbeInventoryIndex,
    desired_bindings: BTreeSet<DeviceId>,
    failures: BTreeMap<DeviceId, RuntimeFailureRecord>,
    backends: RuntimeBackends,
    event_base_index: usize,
    retained_event_bytes: usize,
    events: Vec<LemnosEvent>,
    event_notifier: Arc<RuntimeEventNotifier>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            config: RuntimeConfig::default(),
            running: true,
            inventory: Arc::new(InventorySnapshot::default()),
            registry: DriverRegistry::default(),
            states: BTreeMap::default(),
            bindings: BTreeMap::default(),
            probe_inventory: ProbeInventoryIndex::default(),
            desired_bindings: BTreeSet::default(),
            failures: BTreeMap::default(),
            backends: RuntimeBackends::default(),
            event_base_index: 0,
            retained_event_bytes: 0,
            events: Vec::default(),
            event_notifier: Arc::new(RuntimeEventNotifier::default()),
        }
    }
}
