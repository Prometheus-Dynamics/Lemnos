use lemnos_core::{DeviceId, InterfaceKind};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::{DiscoveryResult, InventorySnapshot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    pub probe: String,
    pub interfaces: Vec<InterfaceKind>,
    pub refreshed_interfaces: Vec<InterfaceKind>,
    pub discovered_devices: usize,
    pub discovered_device_ids: Vec<DeviceId>,
    pub notes: Vec<String>,
    pub error: Option<String>,
}

impl ProbeReport {
    pub fn success(
        probe: impl Into<String>,
        interfaces: Vec<InterfaceKind>,
        refreshed_interfaces: Vec<InterfaceKind>,
        discovered_devices: usize,
        discovered_device_ids: Vec<DeviceId>,
        notes: Vec<String>,
    ) -> Self {
        Self {
            probe: probe.into(),
            interfaces,
            refreshed_interfaces,
            discovered_devices,
            discovered_device_ids,
            notes,
            error: None,
        }
    }

    pub fn failure(
        probe: impl Into<String>,
        interfaces: Vec<InterfaceKind>,
        refreshed_interfaces: Vec<InterfaceKind>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            probe: probe.into(),
            interfaces,
            refreshed_interfaces,
            discovered_devices: 0,
            discovered_device_ids: Vec::new(),
            notes: Vec::new(),
            error: Some(error.into()),
        }
    }

    pub fn succeeded(&self) -> bool {
        self.error.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnrichmentReport {
    pub enricher: String,
    pub interfaces: Vec<InterfaceKind>,
    pub refreshed_interfaces: Vec<InterfaceKind>,
    pub notes: Vec<String>,
    pub error: Option<String>,
}

impl EnrichmentReport {
    pub fn success(
        enricher: impl Into<String>,
        interfaces: Vec<InterfaceKind>,
        refreshed_interfaces: Vec<InterfaceKind>,
        notes: Vec<String>,
    ) -> Self {
        Self {
            enricher: enricher.into(),
            interfaces,
            refreshed_interfaces,
            notes,
            error: None,
        }
    }

    pub fn failure(
        enricher: impl Into<String>,
        interfaces: Vec<InterfaceKind>,
        refreshed_interfaces: Vec<InterfaceKind>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            enricher: enricher.into(),
            interfaces,
            refreshed_interfaces,
            notes: Vec::new(),
            error: Some(error.into()),
        }
    }

    pub fn succeeded(&self) -> bool {
        self.error.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoveryRunReport {
    pub snapshot: Arc<InventorySnapshot>,
    pub probe_reports: Vec<ProbeReport>,
    pub enrichment_reports: Vec<EnrichmentReport>,
}

impl DiscoveryRunReport {
    pub fn has_probe_failures(&self) -> bool {
        self.probe_reports.iter().any(|report| !report.succeeded())
    }

    pub fn probe_failure_count(&self) -> usize {
        self.probe_reports
            .iter()
            .filter(|report| !report.succeeded())
            .count()
    }

    pub fn has_enrichment_failures(&self) -> bool {
        self.enrichment_reports
            .iter()
            .any(|report| !report.succeeded())
    }

    pub fn enrichment_failure_count(&self) -> usize {
        self.enrichment_reports
            .iter()
            .filter(|report| !report.succeeded())
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProbeInventoryIndex {
    devices_by_probe: BTreeMap<String, BTreeMap<InterfaceKind, BTreeSet<DeviceId>>>,
}

impl ProbeInventoryIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_run(run: &DiscoveryRunReport) -> Self {
        let mut index = Self::new();
        index.record_run(run);
        index
    }

    pub fn record_run(&mut self, run: &DiscoveryRunReport) {
        let devices = run
            .snapshot
            .devices
            .iter()
            .map(|device| (&device.id, device.interface))
            .collect::<BTreeMap<_, _>>();

        for report in &run.probe_reports {
            if !report.succeeded() {
                continue;
            }

            let probe_entry = self
                .devices_by_probe
                .entry(report.probe.clone())
                .or_default();
            for interface in &report.refreshed_interfaces {
                probe_entry.insert(*interface, BTreeSet::new());
            }

            for device_id in &report.discovered_device_ids {
                if let Some(interface) = devices.get(device_id) {
                    probe_entry
                        .entry(*interface)
                        .or_default()
                        .insert(device_id.clone());
                }
            }
        }
    }

    pub fn ids_for_probe_interface(
        &self,
        probe: &str,
        interface: InterfaceKind,
    ) -> Option<&BTreeSet<DeviceId>> {
        self.devices_by_probe.get(probe)?.get(&interface)
    }

    pub fn merge_snapshot(
        &self,
        current: &InventorySnapshot,
        run: &DiscoveryRunReport,
    ) -> DiscoveryResult<InventorySnapshot> {
        let mut devices = current
            .devices
            .iter()
            .filter(|device| !self.was_replaced_by_run(device.id.as_str(), device.interface, run))
            .cloned()
            .collect::<Vec<_>>();
        devices.extend(run.snapshot.devices.iter().cloned());

        let observed_at = run.snapshot.observed_at.or(current.observed_at);
        InventorySnapshot::with_observed_at(devices, observed_at)
    }

    fn was_replaced_by_run(
        &self,
        device_id: &str,
        interface: InterfaceKind,
        run: &DiscoveryRunReport,
    ) -> bool {
        run.probe_reports.iter().any(|report| {
            report.succeeded()
                && report.refreshed_interfaces.contains(&interface)
                && self
                    .ids_for_probe_interface(&report.probe, interface)
                    .is_some_and(|ids| ids.iter().any(|id| id.as_str() == device_id))
        })
    }
}
