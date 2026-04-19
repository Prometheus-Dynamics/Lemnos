use super::operations::close_detached_bindings;
use super::*;
use crate::runtime::operations::DetachedBoundDevice;
use std::sync::Arc;

pub(crate) struct PreparedRefreshCommit {
    discovery: DiscoveryRunReport,
    mode: RefreshMode,
    probe_count: usize,
    started_at: std::time::Instant,
    probe_failures: usize,
    enrichment_failures: usize,
    retained_prior_inventory: bool,
    diff: InventoryDiff,
    detached: Vec<DetachedBoundDevice>,
    rebind_targets: Vec<DeviceId>,
    snapshot: Arc<InventorySnapshot>,
}

impl PreparedRefreshCommit {
    pub(crate) fn take_detached(&mut self) -> Vec<DetachedBoundDevice> {
        std::mem::take(&mut self.detached)
    }

    pub(crate) fn take_rebind_targets(&mut self) -> Vec<DeviceId> {
        std::mem::take(&mut self.rebind_targets)
    }

    pub(crate) fn finish(self, rebinds: RuntimeRebindReport) -> RuntimeRefreshReport {
        #[cfg(not(feature = "tracing"))]
        let _ = self.started_at;
        let is_degraded = match self.mode {
            RefreshMode::Full => self.retained_prior_inventory || self.enrichment_failures > 0,
            RefreshMode::Incremental => self.probe_failures > 0 || self.enrichment_failures > 0,
        };
        if is_degraded {
            match self.mode {
                RefreshMode::Full => runtime_warn!(
                    elapsed_ms = self.started_at.elapsed().as_millis() as u64,
                    probe_count = self.probe_count,
                    probe_reports = self.discovery.probe_reports.len(),
                    probe_failures = self.probe_failures,
                    enrichment_reports = self.discovery.enrichment_reports.len(),
                    enrichment_failures = self.enrichment_failures,
                    retained_prior_inventory = self.retained_prior_inventory,
                    snapshot_size = self.snapshot.len(),
                    added = self.diff.added.len(),
                    removed = self.diff.removed.len(),
                    changed = self.diff.changed.len(),
                    rebound = rebinds.rebound.len(),
                    failed_rebinds = rebinds.failed.len(),
                    "runtime refresh completed with degraded discovery data"
                ),
                RefreshMode::Incremental => runtime_warn!(
                    elapsed_ms = self.started_at.elapsed().as_millis() as u64,
                    probe_count = self.probe_count,
                    probe_reports = self.discovery.probe_reports.len(),
                    probe_failures = self.probe_failures,
                    enrichment_reports = self.discovery.enrichment_reports.len(),
                    enrichment_failures = self.enrichment_failures,
                    retained_prior_inventory = self.retained_prior_inventory,
                    snapshot_size = self.snapshot.len(),
                    added = self.diff.added.len(),
                    removed = self.diff.removed.len(),
                    changed = self.diff.changed.len(),
                    rebound = rebinds.rebound.len(),
                    failed_rebinds = rebinds.failed.len(),
                    "runtime incremental refresh completed with degraded discovery data"
                ),
            }
        }
        #[cfg(not(feature = "tracing"))]
        let _ = self.probe_count;

        match self.mode {
            RefreshMode::Full => runtime_info!(
                elapsed_ms = self.started_at.elapsed().as_millis() as u64,
                probe_count = self.probe_count,
                probe_reports = self.discovery.probe_reports.len(),
                probe_failures = self.probe_failures,
                enrichment_reports = self.discovery.enrichment_reports.len(),
                enrichment_failures = self.enrichment_failures,
                retained_prior_inventory = self.retained_prior_inventory,
                snapshot_size = self.snapshot.len(),
                added = self.diff.added.len(),
                removed = self.diff.removed.len(),
                changed = self.diff.changed.len(),
                rebound = rebinds.rebound.len(),
                failed_rebinds = rebinds.failed.len(),
                "runtime refresh completed"
            ),
            RefreshMode::Incremental => runtime_info!(
                elapsed_ms = self.started_at.elapsed().as_millis() as u64,
                probe_count = self.probe_count,
                probe_reports = self.discovery.probe_reports.len(),
                probe_failures = self.probe_failures,
                enrichment_reports = self.discovery.enrichment_reports.len(),
                enrichment_failures = self.enrichment_failures,
                retained_prior_inventory = self.retained_prior_inventory,
                snapshot_size = self.snapshot.len(),
                added = self.diff.added.len(),
                removed = self.diff.removed.len(),
                changed = self.diff.changed.len(),
                rebound = rebinds.rebound.len(),
                failed_rebinds = rebinds.failed.len(),
                "runtime incremental refresh completed"
            ),
        }

        RuntimeRefreshReport {
            discovery: DiscoveryRunReport {
                snapshot: self.snapshot,
                probe_reports: self.discovery.probe_reports,
                enrichment_reports: self.discovery.enrichment_reports,
            },
            diff: self.diff,
            rebinds,
        }
    }
}

impl Runtime {
    pub fn refresh(
        &mut self,
        context: &DiscoveryContext,
        probes: &[&dyn DiscoveryProbe],
    ) -> RuntimeResult<RuntimeRefreshReport> {
        self.refresh_with_mode(context, probes, RefreshMode::Full)
    }

    pub fn refresh_incremental(
        &mut self,
        context: &DiscoveryContext,
        probes: &[&dyn DiscoveryProbe],
    ) -> RuntimeResult<RuntimeRefreshReport> {
        self.refresh_with_mode(context, probes, RefreshMode::Incremental)
    }

    fn refresh_with_mode(
        &mut self,
        context: &DiscoveryContext,
        probes: &[&dyn DiscoveryProbe],
        mode: RefreshMode,
    ) -> RuntimeResult<RuntimeRefreshReport> {
        let started_at = std::time::Instant::now();
        match mode {
            RefreshMode::Full => runtime_debug!(
                probe_count = probes.len(),
                requested_interfaces = context.requested_interfaces.len(),
                "runtime refresh starting"
            ),
            RefreshMode::Incremental => runtime_debug!(
                probe_count = probes.len(),
                requested_interfaces = context.requested_interfaces.len(),
                "runtime incremental refresh starting"
            ),
        }
        self.ensure_running()?;
        let discovery = run_probes(context, probes)?;
        self.finish_refresh(discovery, mode, probes.len(), started_at)
    }

    pub(crate) fn finish_refresh(
        &mut self,
        discovery: DiscoveryRunReport,
        mode: RefreshMode,
        probe_count: usize,
        started_at: std::time::Instant,
    ) -> RuntimeResult<RuntimeRefreshReport> {
        let mut prepared = self.prepare_refresh_commit(discovery, mode, probe_count, started_at)?;
        close_detached_bindings(prepared.take_detached());
        let rebinds = self.rebind_tracked_devices(prepared.take_rebind_targets());
        Ok(prepared.finish(rebinds))
    }

    pub(crate) fn prepare_refresh_commit(
        &mut self,
        discovery: DiscoveryRunReport,
        mode: RefreshMode,
        probe_count: usize,
        started_at: std::time::Instant,
    ) -> RuntimeResult<PreparedRefreshCommit> {
        self.ensure_running()?;
        let probe_failures = discovery.probe_failure_count();
        let enrichment_failures = discovery.enrichment_failure_count();
        let retained_prior_inventory = matches!(mode, RefreshMode::Full) && probe_failures > 0;
        let next_inventory = match mode {
            RefreshMode::Full => self.materialize_refresh_inventory(&discovery)?,
            RefreshMode::Incremental => self
                .probe_inventory
                .merge_snapshot(&self.inventory, &discovery)?,
        };
        let diff = self.inventory.diff(&next_inventory);
        let invalidated_bindings = self.collect_invalidated_bindings(&diff);
        let rebind_targets = self.collect_rebind_targets(&diff, &invalidated_bindings);

        self.record_inventory_events(&diff);
        let detached = self.detach_invalidated_bindings(&diff, &invalidated_bindings);
        self.inventory = Arc::new(next_inventory);
        match mode {
            RefreshMode::Full => {
                self.probe_inventory = ProbeInventoryIndex::from_run(&discovery);
            }
            RefreshMode::Incremental => {
                self.probe_inventory.record_run(&discovery);
            }
        }

        Ok(PreparedRefreshCommit {
            discovery,
            mode,
            probe_count,
            started_at,
            probe_failures,
            enrichment_failures,
            retained_prior_inventory,
            diff,
            detached,
            rebind_targets,
            snapshot: Arc::clone(&self.inventory),
        })
    }

    fn collect_invalidated_bindings(&self, diff: &InventoryDiff) -> BTreeSet<DeviceId> {
        diff.changed
            .iter()
            .filter(|changed| self.binding_requires_rebind(changed))
            .map(|changed| changed.current.id.clone())
            .collect()
    }

    fn collect_rebind_targets(
        &self,
        diff: &InventoryDiff,
        invalidated_bindings: &BTreeSet<DeviceId>,
    ) -> Vec<DeviceId> {
        if !self.config.auto_rebind_on_refresh {
            return Vec::new();
        }

        let mut targets = BTreeSet::new();
        for device in &diff.added {
            if self.desired_bindings.contains(&device.id) {
                targets.insert(device.id.clone());
            }
        }
        for changed in &diff.changed {
            if self.desired_bindings.contains(&changed.current.id)
                && (invalidated_bindings.contains(&changed.current.id)
                    || !self.bindings.contains_key(&changed.current.id))
            {
                targets.insert(changed.current.id.clone());
            }
        }
        targets.into_iter().collect()
    }

    fn binding_requires_rebind(&self, changed: &lemnos_discovery::ChangedDevice) -> bool {
        let Some(binding) = self.bound_device(&changed.current.id) else {
            return false;
        };

        if changed.previous.interface != changed.current.interface
            || changed.previous.kind != changed.current.kind
            || changed.previous.address != changed.current.address
            || changed.previous.control_surface != changed.current.control_surface
        {
            return true;
        }

        let bound_driver_id = {
            let bound = super::operations::lock_bound(&binding);
            bound.driver_id().to_string()
        };

        match self.registry.resolve(&changed.current) {
            Ok(candidate) => candidate.driver_id != bound_driver_id,
            Err(_) => true,
        }
    }

    pub(crate) fn rebind_tracked_devices(
        &mut self,
        device_ids: Vec<DeviceId>,
    ) -> RuntimeRebindReport {
        let mut report = RuntimeRebindReport::default();

        for device_id in device_ids {
            report.attempted.push(device_id.clone());
            runtime_debug!(device_id = ?device_id, "runtime attempting rebind");
            let result = self.bind_device_by_id(&device_id);
            self.complete_operation(device_id.clone(), RuntimeFailureOperation::Rebind, &result);
            if result.is_ok() {
                runtime_info!(device_id = ?device_id, "runtime device rebound");
                report.rebound.push(device_id);
            } else {
                if let Err(_error) = &result {
                    let _failure = self.failures.get(&device_id);
                    runtime_warn!(
                        device_id = ?device_id,
                        category = ?_failure.map(|failure| failure.category),
                        driver_id = ?_failure.and_then(|failure| failure.driver_id.as_ref().map(|driver_id| driver_id.as_str())),
                        error = %_error,
                        "runtime device rebind failed"
                    );
                }
                report.failed.push(device_id);
            }
        }

        report
    }

    fn materialize_refresh_inventory(
        &self,
        discovery: &DiscoveryRunReport,
    ) -> RuntimeResult<InventorySnapshot> {
        if discovery.has_probe_failures() {
            self.probe_inventory
                .merge_snapshot(&self.inventory, discovery)
                .map_err(RuntimeError::from)
        } else {
            Ok((*discovery.snapshot).clone())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshMode {
    Full,
    Incremental,
}
