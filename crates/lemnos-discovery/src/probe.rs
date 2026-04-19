use crate::{DiscoveryContext, DiscoveryError, DiscoveryResult, InventorySnapshot};
use lemnos_core::{ConfiguredDeviceModel, DeviceDescriptor, InterfaceKind};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProbeDiscovery {
    pub devices: Vec<DeviceDescriptor>,
    pub notes: Vec<String>,
}

impl ProbeDiscovery {
    pub fn new(devices: Vec<DeviceDescriptor>) -> Self {
        Self {
            devices,
            notes: Vec::new(),
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

pub trait DiscoveryProbe: Send + Sync {
    fn name(&self) -> &'static str;
    fn interfaces(&self) -> &'static [InterfaceKind];
    fn discover(&self, context: &DiscoveryContext) -> DiscoveryResult<ProbeDiscovery>;
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnrichmentOutput {
    pub snapshot: InventorySnapshot,
    pub notes: Vec<String>,
}

impl EnrichmentOutput {
    pub fn new(snapshot: InventorySnapshot) -> Self {
        Self {
            snapshot,
            notes: Vec::new(),
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

pub trait DiscoveryEnricher: Send + Sync {
    fn name(&self) -> &'static str;
    fn interfaces(&self) -> &'static [InterfaceKind];
    /// Enrich an already-built snapshot.
    ///
    /// This is currently a standalone discovery-layer extension point. The
    /// `lemnos-runtime` and `lemnos` refresh APIs do not invoke enrichers
    /// implicitly; callers that want enrichment must opt into
    /// [`crate::run_probes_with_enrichers`] or [`crate::apply_enrichers`]
    /// directly.
    fn enrich(
        &self,
        context: &DiscoveryContext,
        snapshot: &InventorySnapshot,
    ) -> DiscoveryResult<EnrichmentOutput>;
}

const I2C_ONLY: [InterfaceKind; 1] = [InterfaceKind::I2c];

pub struct ConfiguredDeviceProbe<T> {
    name: &'static str,
    interfaces: &'static [InterfaceKind],
    configs: Vec<T>,
}

impl<T> ConfiguredDeviceProbe<T> {
    pub fn new(name: &'static str, interfaces: &'static [InterfaceKind], configs: Vec<T>) -> Self {
        Self {
            name,
            interfaces,
            configs,
        }
    }

    pub fn from_configs(name: &'static str, configs: Vec<T>) -> Self
    where
        T: ConfiguredDeviceModel,
    {
        Self::new(name, T::configured_interfaces(), configs)
    }

    pub fn i2c(name: &'static str, configs: Vec<T>) -> Self {
        Self::new(name, &I2C_ONLY, configs)
    }
}

impl<T> DiscoveryProbe for ConfiguredDeviceProbe<T>
where
    T: ConfiguredDeviceModel + Send + Sync,
{
    fn name(&self) -> &'static str {
        self.name
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        self.interfaces
    }

    fn discover(&self, context: &DiscoveryContext) -> DiscoveryResult<ProbeDiscovery> {
        let wants_any = self
            .interfaces
            .iter()
            .any(|interface| context.wants(*interface));
        if !wants_any {
            return Ok(ProbeDiscovery::default());
        }

        let mut devices = Vec::new();
        for config in &self.configs {
            devices.extend(config.configured_descriptors().map_err(|source| {
                DiscoveryError::ProbeFailed {
                    probe: self.name.to_string(),
                    message: source.to_string(),
                }
            })?);
        }

        Ok(ProbeDiscovery::new(devices))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InventoryWatchEvent {
    pub watcher: &'static str,
    pub interfaces: Vec<InterfaceKind>,
    pub paths: Vec<PathBuf>,
}

impl InventoryWatchEvent {
    pub fn new(watcher: &'static str, interfaces: Vec<InterfaceKind>, paths: Vec<PathBuf>) -> Self {
        Self {
            watcher,
            interfaces,
            paths,
        }
    }

    pub fn touches(&self, interface: InterfaceKind) -> bool {
        self.interfaces.contains(&interface)
    }
}

pub trait InventoryWatcher: Send {
    fn name(&self) -> &'static str;
    fn poll(&mut self) -> DiscoveryResult<Vec<InventoryWatchEvent>>;
}
