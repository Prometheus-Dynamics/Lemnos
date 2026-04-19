use super::*;
use crate::async_runtime::sync::{lock, read_lock, write_lock};

impl AsyncRuntime {
    pub async fn refresh(
        &self,
        context: DiscoveryContext,
        probes: Vec<SharedDiscoveryProbe>,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        self.run_refresh(context, probes, RefreshMode::Full).await
    }

    pub async fn refresh_incremental(
        &self,
        context: DiscoveryContext,
        probes: Vec<SharedDiscoveryProbe>,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        self.run_refresh(context, probes, RefreshMode::Incremental)
            .await
    }

    pub async fn poll_watcher_and_refresh<W>(
        &self,
        context: DiscoveryContext,
        probes: Vec<SharedDiscoveryProbe>,
        watcher: &AsyncInventoryWatcher<W>,
    ) -> AsyncRuntimeResult<Option<RuntimeWatchedRefreshReport>>
    where
        W: InventoryWatcher + Send + 'static,
    {
        self.run_watch_refresh(context, probes, watcher, WatchedRefreshMode::Full)
            .await
    }

    pub async fn poll_watcher_and_refresh_incremental<W>(
        &self,
        context: DiscoveryContext,
        probes: Vec<SharedDiscoveryProbe>,
        watcher: &AsyncInventoryWatcher<W>,
    ) -> AsyncRuntimeResult<Option<RuntimeWatchedRefreshReport>>
    where
        W: InventoryWatcher + Send + 'static,
    {
        self.run_watch_refresh(context, probes, watcher, WatchedRefreshMode::Incremental)
            .await
    }

    async fn run_refresh(
        &self,
        context: DiscoveryContext,
        probes: Vec<SharedDiscoveryProbe>,
        operation: RefreshOperation,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        let probe_count = probes.len();
        let started_at = std::time::Instant::now();
        let discovery = tokio::task::spawn_blocking(move || {
            let probe_refs: Vec<&dyn DiscoveryProbe> =
                probes.iter().map(|probe| probe.as_ref()).collect();
            run_probes(&context, &probe_refs)
        })
        .await?
        .map_err(RuntimeError::from)
        .map_err(AsyncRuntimeError::from)?;

        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut prepared = {
                let mut runtime = write_lock(&inner);
                runtime.prepare_refresh_commit(discovery, operation, probe_count, started_at)?
            };
            crate::runtime::close_detached_bindings(prepared.take_detached());
            let rebinds = {
                let mut runtime = write_lock(&inner);
                runtime.rebind_tracked_devices(prepared.take_rebind_targets())
            };
            Ok::<_, RuntimeError>(prepared.finish(rebinds))
        })
        .await?
        .map_err(AsyncRuntimeError::from)
    }

    async fn run_watch_refresh<W>(
        &self,
        context: DiscoveryContext,
        probes: Vec<SharedDiscoveryProbe>,
        watcher: &AsyncInventoryWatcher<W>,
        mode: WatchedRefreshMode,
    ) -> AsyncRuntimeResult<Option<RuntimeWatchedRefreshReport>>
    where
        W: InventoryWatcher + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        let watcher_inner = watcher.inner();
        let completed = tokio::task::spawn_blocking(move || {
            let (watcher_name, watch_events) = {
                let mut watcher = lock(&watcher_inner);
                (watcher.name(), watcher.poll()?)
            };

            let config = {
                let runtime = read_lock(&inner);
                *runtime.config()
            };
            let probe_refs: Vec<&dyn DiscoveryProbe> =
                probes.iter().map(|probe| probe.as_ref()).collect();
            prepare_watch_refresh(
                &config,
                &context,
                &probe_refs,
                watcher_name,
                watch_events,
                mode,
            )?
            .map(|prepared| prepared.run())
            .transpose()
        })
        .await??;

        let Some(completed) = completed else {
            return Ok(None);
        };

        self.finish_completed_watch_refresh(completed)
            .await
            .map(Some)
    }

    async fn finish_completed_watch_refresh(
        &self,
        completed: CompletedWatchRefresh,
    ) -> AsyncRuntimeResult<RuntimeWatchedRefreshReport> {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || completed.finish_async(&inner))
            .await?
            .map_err(AsyncRuntimeError::from)
    }
}
