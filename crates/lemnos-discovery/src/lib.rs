#![forbid(unsafe_code)]

mod context;
mod error;
#[cfg(any(test, feature = "test-utils"))]
mod fixtures;
mod inventory;
mod probe;
mod report;
mod run;

#[cfg(test)]
mod tests;

pub use context::DiscoveryContext;
pub use error::{DiscoveryError, DiscoveryResult};
#[cfg(any(test, feature = "test-utils"))]
pub use fixtures::{
    DeviceFixtureBuilder, InventoryDiffFixture, InventoryDiffFixtureBuilder,
    InventoryFixtureBuilder,
};
pub use inventory::{ChangedDevice, InventoryDiff, InventorySnapshot};
pub use probe::{
    ConfiguredDeviceProbe, DiscoveryEnricher, DiscoveryProbe, EnrichmentOutput,
    InventoryWatchEvent, InventoryWatcher, ProbeDiscovery,
};
pub use report::{DiscoveryRunReport, EnrichmentReport, ProbeInventoryIndex, ProbeReport};
pub use run::{
    DEFAULT_INLINE_PROBE_THRESHOLD, DEFAULT_MAX_PARALLEL_PROBE_WORKERS, apply_enrichers,
    run_probes, run_probes_with_enrichers,
};
