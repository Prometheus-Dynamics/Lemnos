use super::*;
#[cfg(any(feature = "linux", feature = "mock"))]
use lemnos_runtime::AsyncRuntimeResult;

impl AsyncLemnos {
    impl_linux_backend_methods!(async);

    #[cfg(feature = "mock")]
    pub async fn refresh_with_mock(
        &self,
        context: DiscoveryContext,
        hardware: &MockHardware,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        self.refresh_with_probe(context, hardware.clone()).await
    }

    #[cfg(feature = "mock")]
    pub async fn refresh_with_mock_default(
        &self,
        hardware: &MockHardware,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        self.refresh_with_mock(DiscoveryContext::new(), hardware)
            .await
    }

    #[cfg(feature = "mock")]
    pub async fn refresh_incremental_with_mock(
        &self,
        context: DiscoveryContext,
        hardware: &MockHardware,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        self.refresh_incremental_with_probe(context, hardware.clone())
            .await
    }

    #[cfg(feature = "mock")]
    pub async fn refresh_incremental_with_mock_default(
        &self,
        hardware: &MockHardware,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        self.refresh_incremental_with_mock(DiscoveryContext::new(), hardware)
            .await
    }

    #[cfg(feature = "linux")]
    pub async fn refresh_with_linux(
        &self,
        context: DiscoveryContext,
        backend: &LinuxBackend,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        let backend = backend.clone();
        self.runtime()
            .with_runtime(move |runtime| {
                backend.with_probes(|probes| runtime.refresh(&context, &probes))
            })
            .await
    }

    #[cfg(feature = "linux")]
    pub async fn refresh_with_linux_default(
        &self,
        backend: &LinuxBackend,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        self.refresh_with_linux(DiscoveryContext::new(), backend)
            .await
    }

    #[cfg(feature = "linux")]
    pub async fn refresh_incremental_with_linux(
        &self,
        context: DiscoveryContext,
        backend: &LinuxBackend,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        let backend = backend.clone();
        self.runtime()
            .with_runtime(move |runtime| {
                backend.with_probes(|probes| runtime.refresh_incremental(&context, &probes))
            })
            .await
    }

    #[cfg(feature = "linux")]
    pub async fn refresh_incremental_with_linux_default(
        &self,
        backend: &LinuxBackend,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        self.refresh_incremental_with_linux(DiscoveryContext::new(), backend)
            .await
    }

    #[cfg(all(feature = "linux", feature = "linux-hotplug"))]
    pub async fn poll_watcher_and_refresh_with_linux(
        &self,
        context: DiscoveryContext,
        backend: &LinuxBackend,
        watcher: LinuxHotplugWatcher,
    ) -> AsyncRuntimeResult<(Option<RuntimeWatchedRefreshReport>, LinuxHotplugWatcher)> {
        let backend = backend.clone();
        self.runtime()
            .with_runtime(move |runtime| {
                let mut watcher = watcher;
                let report = backend.with_probes(|probes| {
                    runtime.poll_watcher_and_refresh(&context, &probes, &mut watcher)
                })?;
                Ok((report, watcher))
            })
            .await
    }

    #[cfg(all(feature = "linux", feature = "linux-hotplug"))]
    pub async fn poll_watcher_and_refresh_incremental_with_linux(
        &self,
        context: DiscoveryContext,
        backend: &LinuxBackend,
        watcher: LinuxHotplugWatcher,
    ) -> AsyncRuntimeResult<(Option<RuntimeWatchedRefreshReport>, LinuxHotplugWatcher)> {
        let backend = backend.clone();
        self.runtime()
            .with_runtime(move |runtime| {
                let mut watcher = watcher;
                let report = backend.with_probes(|probes| {
                    runtime.poll_watcher_and_refresh_incremental(&context, &probes, &mut watcher)
                })?;
                Ok((report, watcher))
            })
            .await
    }
}
