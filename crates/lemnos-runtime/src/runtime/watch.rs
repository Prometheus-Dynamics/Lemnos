use super::{Runtime, RuntimeRefreshReport, RuntimeWatchedRefreshReport};
use crate::{RuntimeConfig, RuntimeResult, RuntimeWatchRefreshMode};
use lemnos_discovery::{DiscoveryContext, DiscoveryProbe, InventoryWatchEvent, InventoryWatcher};
use std::collections::BTreeSet;

impl Runtime {
    pub fn poll_watcher_and_refresh(
        &mut self,
        context: &DiscoveryContext,
        probes: &[&dyn DiscoveryProbe],
        watcher: &mut dyn InventoryWatcher,
    ) -> RuntimeResult<Option<RuntimeWatchedRefreshReport>> {
        let watch_events = watcher.poll()?;
        self.handle_watch_events_and_refresh_with_mode(
            context,
            probes,
            watcher.name(),
            watch_events,
            WatchedRefreshMode::Full,
        )
    }

    pub fn poll_watcher_and_refresh_incremental(
        &mut self,
        context: &DiscoveryContext,
        probes: &[&dyn DiscoveryProbe],
        watcher: &mut dyn InventoryWatcher,
    ) -> RuntimeResult<Option<RuntimeWatchedRefreshReport>> {
        let watch_events = watcher.poll()?;
        self.handle_watch_events_and_refresh_with_mode(
            context,
            probes,
            watcher.name(),
            watch_events,
            WatchedRefreshMode::Incremental,
        )
    }

    fn handle_watch_events_and_refresh_with_mode(
        &mut self,
        context: &DiscoveryContext,
        probes: &[&dyn DiscoveryProbe],
        watcher_name: &'static str,
        watch_events: Vec<InventoryWatchEvent>,
        mode: WatchedRefreshMode,
    ) -> RuntimeResult<Option<RuntimeWatchedRefreshReport>> {
        let Some(prepared) = prepare_watch_refresh(
            &self.config,
            context,
            probes,
            watcher_name,
            watch_events,
            mode,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(prepared.run()?.finish(self)?))
    }
}

pub(crate) fn prepare_watch_refresh<'a>(
    config: &RuntimeConfig,
    context: &DiscoveryContext,
    probes: &[&'a dyn DiscoveryProbe],
    watcher_name: &'static str,
    watch_events: Vec<InventoryWatchEvent>,
    mode: WatchedRefreshMode,
) -> RuntimeResult<Option<PreparedWatchRefresh<'a>>> {
    let started_at = std::time::Instant::now();
    if watch_events.is_empty() {
        return Ok(None);
    }

    match mode {
        WatchedRefreshMode::Full => super::runtime_debug!(
            watcher = watcher_name,
            watch_event_count = watch_events.len(),
            touched_interfaces = touched_interface_count(&watch_events),
            touched_paths = touched_path_count(&watch_events),
            "runtime watcher reported inventory changes"
        ),
        WatchedRefreshMode::Incremental => super::runtime_debug!(
            watcher = watcher_name,
            watch_event_count = watch_events.len(),
            touched_interfaces = touched_interface_count(&watch_events),
            touched_paths = touched_path_count(&watch_events),
            "runtime watcher reported incremental inventory changes"
        ),
    }

    let Some((scoped_context, scoped_probes)) =
        watch_refresh_scope(config, context, probes, &watch_events)
    else {
        super::runtime_debug!(
            watcher = watcher_name,
            watch_event_count = watch_events.len(),
            "runtime watcher events did not match any configured probes"
        );
        return Ok(None);
    };

    super::runtime_debug!(
        watcher = watcher_name,
        scoped_probe_count = scoped_probes.len(),
        scoped_interface_count = scoped_context.requested_interfaces.len(),
        watch_refresh_mode = ?config.watch_refresh_mode,
        "runtime watcher selected refresh scope"
    );

    Ok(Some(PreparedWatchRefresh {
        watcher_name,
        watch_events,
        scoped_context,
        scoped_probes,
        mode,
        started_at,
    }))
}

pub(crate) struct PreparedWatchRefresh<'a> {
    watcher_name: &'static str,
    watch_events: Vec<InventoryWatchEvent>,
    scoped_context: DiscoveryContext,
    scoped_probes: Vec<&'a dyn DiscoveryProbe>,
    mode: WatchedRefreshMode,
    started_at: std::time::Instant,
}

impl<'a> PreparedWatchRefresh<'a> {
    pub(crate) fn run(self) -> RuntimeResult<CompletedWatchRefresh> {
        let scoped_probe_count = self.scoped_probes.len();
        let discovery = super::run_probes(&self.scoped_context, &self.scoped_probes)?;
        Ok(CompletedWatchRefresh {
            watcher_name: self.watcher_name,
            watch_events: self.watch_events,
            discovery,
            mode: self.mode,
            scoped_probe_count,
            started_at: self.started_at,
        })
    }
}

pub(crate) struct CompletedWatchRefresh {
    watcher_name: &'static str,
    watch_events: Vec<InventoryWatchEvent>,
    discovery: lemnos_discovery::DiscoveryRunReport,
    mode: WatchedRefreshMode,
    scoped_probe_count: usize,
    started_at: std::time::Instant,
}

impl CompletedWatchRefresh {
    fn build_report(
        watcher_name: &'static str,
        watch_events: Vec<InventoryWatchEvent>,
        mode: WatchedRefreshMode,
        scoped_probe_count: usize,
        started_at: std::time::Instant,
        refresh: RuntimeRefreshReport,
    ) -> RuntimeWatchedRefreshReport {
        #[cfg(not(feature = "tracing"))]
        let _ = watcher_name;

        if refresh.discovery.has_probe_failures() || refresh.discovery.has_enrichment_failures() {
            match mode {
                WatchedRefreshMode::Full => super::runtime_warn!(
                    watcher = watcher_name,
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    watch_event_count = watch_events.len(),
                    scoped_probe_count = scoped_probe_count,
                    probe_failures = refresh.discovery.probe_failure_count(),
                    enrichment_failures = refresh.discovery.enrichment_failure_count(),
                    "runtime watch refresh observed degraded discovery data"
                ),
                WatchedRefreshMode::Incremental => super::runtime_warn!(
                    watcher = watcher_name,
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    watch_event_count = watch_events.len(),
                    scoped_probe_count = scoped_probe_count,
                    probe_failures = refresh.discovery.probe_failure_count(),
                    enrichment_failures = refresh.discovery.enrichment_failure_count(),
                    "runtime incremental watch refresh observed degraded discovery data"
                ),
            }
        }
        match mode {
            WatchedRefreshMode::Full => super::runtime_info!(
                watcher = watcher_name,
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                watch_event_count = watch_events.len(),
                scoped_probe_count = scoped_probe_count,
                added = refresh.diff.added.len(),
                removed = refresh.diff.removed.len(),
                changed = refresh.diff.changed.len(),
                rebound = refresh.rebinds.rebound.len(),
                failed_rebinds = refresh.rebinds.failed.len(),
                "runtime watch refresh completed"
            ),
            WatchedRefreshMode::Incremental => super::runtime_info!(
                watcher = watcher_name,
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                watch_event_count = watch_events.len(),
                scoped_probe_count = scoped_probe_count,
                added = refresh.diff.added.len(),
                removed = refresh.diff.removed.len(),
                changed = refresh.diff.changed.len(),
                rebound = refresh.rebinds.rebound.len(),
                failed_rebinds = refresh.rebinds.failed.len(),
                "runtime incremental watch refresh completed"
            ),
        }
        RuntimeWatchedRefreshReport {
            watch_events,
            refresh,
        }
    }

    pub(crate) fn finish(
        self,
        runtime: &mut Runtime,
    ) -> RuntimeResult<RuntimeWatchedRefreshReport> {
        let Self {
            watcher_name,
            watch_events,
            discovery,
            mode,
            scoped_probe_count,
            started_at,
        } = self;
        let refresh = runtime.finish_refresh(
            discovery,
            mode.refresh_mode(),
            scoped_probe_count,
            started_at,
        )?;
        Ok(Self::build_report(
            watcher_name,
            watch_events,
            mode,
            scoped_probe_count,
            started_at,
            refresh,
        ))
    }

    #[cfg(feature = "tokio")]
    pub(crate) fn finish_async(
        self,
        runtime: &std::sync::Arc<std::sync::RwLock<Runtime>>,
    ) -> RuntimeResult<RuntimeWatchedRefreshReport> {
        let Self {
            watcher_name,
            watch_events,
            discovery,
            mode,
            scoped_probe_count,
            started_at,
        } = self;
        let mut prepared = {
            let mut runtime = runtime
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            runtime.prepare_refresh_commit(
                discovery,
                mode.refresh_mode(),
                scoped_probe_count,
                started_at,
            )?
        };
        super::operations::close_detached_bindings(prepared.take_detached());
        let rebinds = {
            let mut runtime = runtime
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            runtime.rebind_tracked_devices(prepared.take_rebind_targets())
        };
        let refresh = prepared.finish(rebinds);
        Ok(Self::build_report(
            watcher_name,
            watch_events,
            mode,
            scoped_probe_count,
            started_at,
            refresh,
        ))
    }
}

#[cfg(feature = "tracing")]
fn touched_interface_count(events: &[InventoryWatchEvent]) -> usize {
    events
        .iter()
        .flat_map(|event| event.interfaces.iter().copied())
        .collect::<BTreeSet<_>>()
        .len()
}

#[cfg(feature = "tracing")]
fn touched_path_count(events: &[InventoryWatchEvent]) -> usize {
    events.iter().map(|event| event.paths.len()).sum()
}

fn watch_refresh_scope<'a>(
    config: &RuntimeConfig,
    context: &DiscoveryContext,
    probes: &[&'a dyn DiscoveryProbe],
    watch_events: &[InventoryWatchEvent],
) -> Option<(DiscoveryContext, Vec<&'a dyn DiscoveryProbe>)> {
    if matches!(config.watch_refresh_mode, RuntimeWatchRefreshMode::Full) {
        super::runtime_debug!(
            watch_refresh_mode = ?config.watch_refresh_mode,
            requested_interface_count = context.requested_interfaces.len(),
            configured_probe_count = probes.len(),
            "runtime watch refresh forced full probe scope by configuration"
        );
        return Some((context.clone(), probes.to_vec()));
    }

    let touched_interfaces = watch_events
        .iter()
        .flat_map(|event| event.interfaces.iter().copied())
        .collect::<BTreeSet<_>>();

    if touched_interfaces.is_empty() {
        super::runtime_debug!(
            watch_refresh_mode = ?config.watch_refresh_mode,
            requested_interface_count = context.requested_interfaces.len(),
            configured_probe_count = probes.len(),
            "runtime watch refresh received no interface hints"
        );
        return match config.watch_refresh_mode {
            RuntimeWatchRefreshMode::StrictScoped => None,
            RuntimeWatchRefreshMode::FallbackToFull => Some((context.clone(), probes.to_vec())),
            RuntimeWatchRefreshMode::Full => Some((context.clone(), probes.to_vec())),
        };
    }

    let requested_interfaces = if context.requested_interfaces.is_empty() {
        touched_interfaces.clone()
    } else {
        context
            .requested_interfaces
            .intersection(&touched_interfaces)
            .copied()
            .collect()
    };

    if requested_interfaces.is_empty() {
        super::runtime_debug!(
            watch_refresh_mode = ?config.watch_refresh_mode,
            requested_interface_count = context.requested_interfaces.len(),
            touched_interface_count = touched_interfaces.len(),
            configured_probe_count = probes.len(),
            "runtime watch refresh found no overlap between requested and touched interfaces"
        );
        return match config.watch_refresh_mode {
            RuntimeWatchRefreshMode::StrictScoped => None,
            RuntimeWatchRefreshMode::FallbackToFull | RuntimeWatchRefreshMode::Full => {
                Some((context.clone(), probes.to_vec()))
            }
        };
    }

    let scoped_probes = probes
        .iter()
        .copied()
        .filter(|probe| {
            probe
                .interfaces()
                .iter()
                .any(|interface| requested_interfaces.contains(interface))
        })
        .collect::<Vec<_>>();

    if scoped_probes.is_empty() {
        super::runtime_debug!(
            watch_refresh_mode = ?config.watch_refresh_mode,
            requested_interface_count = requested_interfaces.len(),
            touched_interface_count = touched_interfaces.len(),
            configured_probe_count = probes.len(),
            "runtime watch refresh found no probes for the touched interface scope"
        );
        return match config.watch_refresh_mode {
            RuntimeWatchRefreshMode::StrictScoped => None,
            RuntimeWatchRefreshMode::FallbackToFull | RuntimeWatchRefreshMode::Full => {
                Some((context.clone(), probes.to_vec()))
            }
        };
    }

    let mut scoped_context = context.clone();
    scoped_context.requested_interfaces = requested_interfaces;
    super::runtime_debug!(
        watch_refresh_mode = ?config.watch_refresh_mode,
        requested_interface_count = scoped_context.requested_interfaces.len(),
        configured_probe_count = probes.len(),
        scoped_probe_count = scoped_probes.len(),
        "runtime watch refresh selected scoped probe set"
    );
    Some((scoped_context, scoped_probes))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchedRefreshMode {
    Full,
    Incremental,
}

impl WatchedRefreshMode {
    fn refresh_mode(self) -> super::refresh::RefreshMode {
        match self {
            Self::Full => super::refresh::RefreshMode::Full,
            Self::Incremental => super::refresh::RefreshMode::Incremental,
        }
    }
}
