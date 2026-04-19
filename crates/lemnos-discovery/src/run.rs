use crate::{
    DiscoveryContext, DiscoveryEnricher, DiscoveryError, DiscoveryProbe, DiscoveryResult,
    DiscoveryRunReport, EnrichmentReport, InventorySnapshot, ProbeReport,
};
use lemnos_core::InterfaceKind;
use std::sync::Arc;
use std::thread;

pub const DEFAULT_INLINE_PROBE_THRESHOLD: usize = 2;
pub const DEFAULT_MAX_PARALLEL_PROBE_WORKERS: usize = 4;

struct SelectedProbe<'a> {
    probe: &'a dyn DiscoveryProbe,
    interfaces: &'static [InterfaceKind],
    refreshed_interfaces: Vec<InterfaceKind>,
}

pub fn run_probes(
    context: &DiscoveryContext,
    probes: &[&dyn DiscoveryProbe],
) -> DiscoveryResult<DiscoveryRunReport> {
    let selected_probes = select_probes(context, probes);

    let mut devices = Vec::new();
    let mut probe_reports = Vec::with_capacity(selected_probes.len());

    let probe_results = run_selected_probes(context, selected_probes);

    for (_index, selected_probe, discovery) in probe_results {
        process_probe_result(selected_probe, discovery, &mut devices, &mut probe_reports)?;
    }

    let snapshot = InventorySnapshot::with_observed_at(devices, context.observed_at)?;
    Ok(DiscoveryRunReport {
        snapshot: Arc::new(snapshot),
        probe_reports,
        enrichment_reports: Vec::new(),
    })
}

pub fn apply_enrichers(
    context: &DiscoveryContext,
    snapshot: &InventorySnapshot,
    enrichers: &[&dyn DiscoveryEnricher],
) -> DiscoveryResult<(InventorySnapshot, Vec<EnrichmentReport>)> {
    let mut current = snapshot.clone();
    let mut reports = Vec::with_capacity(enrichers.len());

    for enricher in enrichers {
        let interfaces = enricher.interfaces();
        let refreshed_interfaces = selected_interfaces(context, interfaces);
        if refreshed_interfaces.is_empty() {
            continue;
        }

        match enricher.enrich(context, &current) {
            Ok(output) => {
                current = InventorySnapshot::with_observed_at(
                    output.snapshot.devices,
                    output.snapshot.observed_at.or(current.observed_at),
                )?;
                reports.push(EnrichmentReport::success(
                    enricher.name(),
                    interfaces.to_vec(),
                    refreshed_interfaces,
                    output.notes,
                ));
            }
            Err(error) => {
                reports.push(EnrichmentReport::failure(
                    enricher.name(),
                    interfaces.to_vec(),
                    refreshed_interfaces,
                    error.to_string(),
                ));
            }
        }
    }

    Ok((current, reports))
}

pub fn run_probes_with_enrichers(
    context: &DiscoveryContext,
    probes: &[&dyn DiscoveryProbe],
    enrichers: &[&dyn DiscoveryEnricher],
) -> DiscoveryResult<DiscoveryRunReport> {
    // Enrichers stay explicit for `0.1.x`: the runtime/facade refresh path
    // continues to use `run_probes` directly so embedders can opt into
    // enrichment only when they want the extra pass.
    let mut run = run_probes(context, probes)?;
    let (snapshot, enrichment_reports) = apply_enrichers(context, &run.snapshot, enrichers)?;
    run.snapshot = Arc::new(snapshot);
    run.enrichment_reports = enrichment_reports;
    Ok(run)
}

fn process_probe_result(
    selected_probe: SelectedProbe<'_>,
    discovery: DiscoveryResult<crate::ProbeDiscovery>,
    devices: &mut Vec<lemnos_core::DeviceDescriptor>,
    probe_reports: &mut Vec<ProbeReport>,
) -> DiscoveryResult<()> {
    match discovery {
        Ok(discovery) => {
            for device in &discovery.devices {
                device
                    .validate()
                    .map_err(|source| DiscoveryError::InvalidDescriptor {
                        probe: selected_probe.probe.name().to_string(),
                        device_id: device.id.clone(),
                        source,
                    })?;
            }
            let discovered_devices = discovery.devices.len();
            let discovered_device_ids = discovery
                .devices
                .iter()
                .map(|device| device.id.clone())
                .collect();
            devices.extend(discovery.devices);
            probe_reports.push(ProbeReport::success(
                selected_probe.probe.name(),
                selected_probe.interfaces.to_vec(),
                selected_probe.refreshed_interfaces,
                discovered_devices,
                discovered_device_ids,
                discovery.notes,
            ));
        }
        Err(error) => {
            probe_reports.push(ProbeReport::failure(
                selected_probe.probe.name(),
                selected_probe.interfaces.to_vec(),
                selected_probe.refreshed_interfaces,
                error.to_string(),
            ));
        }
    }

    Ok(())
}

fn selected_interfaces(
    context: &DiscoveryContext,
    interfaces: &[InterfaceKind],
) -> Vec<InterfaceKind> {
    if context.requested_interfaces.is_empty() {
        return interfaces.to_vec();
    }

    interfaces
        .iter()
        .copied()
        .filter(|interface| context.wants(*interface))
        .collect()
}

struct IndexedSelectedProbe<'a> {
    index: usize,
    selected_probe: SelectedProbe<'a>,
}

type ProbeRunResult<'a> = (
    usize,
    SelectedProbe<'a>,
    DiscoveryResult<crate::ProbeDiscovery>,
);

fn select_probes<'a>(
    context: &DiscoveryContext,
    probes: &'a [&'a dyn DiscoveryProbe],
) -> Vec<SelectedProbe<'a>> {
    probes
        .iter()
        .filter_map(|probe| {
            let interfaces = probe.interfaces();
            let refreshed_interfaces = selected_interfaces(context, interfaces);
            if refreshed_interfaces.is_empty() {
                None
            } else {
                Some(SelectedProbe {
                    probe: *probe,
                    interfaces,
                    refreshed_interfaces,
                })
            }
        })
        .collect()
}

fn run_selected_probes<'a>(
    context: &DiscoveryContext,
    selected_probes: Vec<SelectedProbe<'a>>,
) -> Vec<ProbeRunResult<'a>> {
    let worker_count = parallel_worker_count(context, selected_probes.len());
    if worker_count <= 1 {
        return selected_probes
            .into_iter()
            .enumerate()
            .map(|(index, selected_probe)| {
                let discovery = selected_probe.probe.discover(context);
                (index, selected_probe, discovery)
            })
            .collect();
    }

    run_selected_probes_parallel(context, selected_probes, worker_count)
}

fn run_selected_probes_parallel<'a>(
    context: &DiscoveryContext,
    selected_probes: Vec<SelectedProbe<'a>>,
    worker_count: usize,
) -> Vec<ProbeRunResult<'a>> {
    let batches = build_probe_batches(selected_probes, worker_count);
    let mut results = thread::scope(|scope| {
        let mut handles = Vec::with_capacity(worker_count);
        for batch in batches {
            handles.push(scope.spawn(move || run_probe_batch(context, batch)));
        }

        handles
            .into_iter()
            .flat_map(|handle| match handle.join() {
                Ok(result) => result,
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect::<Vec<_>>()
    });
    results.sort_by_key(|(index, _, _)| *index);
    results
}

fn build_probe_batches<'a>(
    selected_probes: Vec<SelectedProbe<'a>>,
    worker_count: usize,
) -> Vec<Vec<IndexedSelectedProbe<'a>>> {
    // For the small built-in probe set, static round-robin batching keeps the
    // implementation simple while still spreading mixed interface probes
    // across workers. A dynamic work queue can be revisited if probe counts or
    // probe-cost skew grow enough to make this batching visibly imbalanced.
    let mut batches = (0..worker_count).map(|_| Vec::new()).collect::<Vec<_>>();
    for (index, selected_probe) in selected_probes.into_iter().enumerate() {
        batches[index % worker_count].push(IndexedSelectedProbe {
            index,
            selected_probe,
        });
    }
    batches
}

fn run_probe_batch<'a>(
    context: &DiscoveryContext,
    batch: Vec<IndexedSelectedProbe<'a>>,
) -> Vec<ProbeRunResult<'a>> {
    batch
        .into_iter()
        .map(|indexed_probe| {
            let discovery = indexed_probe.selected_probe.probe.discover(context);
            (indexed_probe.index, indexed_probe.selected_probe, discovery)
        })
        .collect()
}

pub(crate) fn parallel_worker_count(
    context: &DiscoveryContext,
    selected_probe_count: usize,
) -> usize {
    let inline_probe_threshold = context
        .inline_probe_threshold
        .unwrap_or(DEFAULT_INLINE_PROBE_THRESHOLD);
    if selected_probe_count <= inline_probe_threshold {
        return 1;
    }

    let max_parallel_probe_workers = context
        .max_parallel_probe_workers
        .unwrap_or(DEFAULT_MAX_PARALLEL_PROBE_WORKERS)
        .max(1);
    let available_parallelism = thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    selected_probe_count
        .min(available_parallelism)
        .min(max_parallel_probe_workers)
}
