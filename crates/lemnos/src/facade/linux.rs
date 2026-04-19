use super::*;

impl Lemnos {
    impl_linux_backend_methods!(sync);

    pub fn refresh_default(
        &mut self,
        probes: &[&dyn DiscoveryProbe],
    ) -> RuntimeResult<RuntimeRefreshReport> {
        self.refresh(&DiscoveryContext::new(), probes)
    }

    pub fn refresh_incremental_default(
        &mut self,
        probes: &[&dyn DiscoveryProbe],
    ) -> RuntimeResult<RuntimeRefreshReport> {
        self.refresh_incremental(&DiscoveryContext::new(), probes)
    }

    pub fn refresh(
        &mut self,
        context: &DiscoveryContext,
        probes: &[&dyn DiscoveryProbe],
    ) -> RuntimeResult<RuntimeRefreshReport> {
        self.runtime.refresh(context, probes)
    }

    pub fn refresh_incremental(
        &mut self,
        context: &DiscoveryContext,
        probes: &[&dyn DiscoveryProbe],
    ) -> RuntimeResult<RuntimeRefreshReport> {
        self.runtime.refresh_incremental(context, probes)
    }

    pub fn poll_watcher_and_refresh(
        &mut self,
        context: &DiscoveryContext,
        probes: &[&dyn DiscoveryProbe],
        watcher: &mut dyn InventoryWatcher,
    ) -> RuntimeResult<Option<RuntimeWatchedRefreshReport>> {
        self.runtime
            .poll_watcher_and_refresh(context, probes, watcher)
    }

    pub fn poll_watcher_and_refresh_incremental(
        &mut self,
        context: &DiscoveryContext,
        probes: &[&dyn DiscoveryProbe],
        watcher: &mut dyn InventoryWatcher,
    ) -> RuntimeResult<Option<RuntimeWatchedRefreshReport>> {
        self.runtime
            .poll_watcher_and_refresh_incremental(context, probes, watcher)
    }

    pub fn refresh_with_probe<P>(
        &mut self,
        context: &DiscoveryContext,
        probe: &P,
    ) -> RuntimeResult<RuntimeRefreshReport>
    where
        P: DiscoveryProbe,
    {
        let probe: &dyn DiscoveryProbe = probe;
        self.runtime.refresh(context, &[probe])
    }

    pub fn refresh_with_probe_default<P>(
        &mut self,
        probe: &P,
    ) -> RuntimeResult<RuntimeRefreshReport>
    where
        P: DiscoveryProbe,
    {
        self.refresh_with_probe(&DiscoveryContext::new(), probe)
    }

    pub fn refresh_incremental_with_probe<P>(
        &mut self,
        context: &DiscoveryContext,
        probe: &P,
    ) -> RuntimeResult<RuntimeRefreshReport>
    where
        P: DiscoveryProbe,
    {
        let probe: &dyn DiscoveryProbe = probe;
        self.runtime.refresh_incremental(context, &[probe])
    }

    pub fn refresh_incremental_with_probe_default<P>(
        &mut self,
        probe: &P,
    ) -> RuntimeResult<RuntimeRefreshReport>
    where
        P: DiscoveryProbe,
    {
        self.refresh_incremental_with_probe(&DiscoveryContext::new(), probe)
    }

    #[cfg(feature = "mock")]
    pub fn refresh_with_mock(
        &mut self,
        context: &DiscoveryContext,
        hardware: &MockHardware,
    ) -> RuntimeResult<RuntimeRefreshReport> {
        self.refresh_with_probe(context, hardware)
    }

    #[cfg(feature = "mock")]
    pub fn refresh_with_mock_default(
        &mut self,
        hardware: &MockHardware,
    ) -> RuntimeResult<RuntimeRefreshReport> {
        self.refresh_with_mock(&DiscoveryContext::new(), hardware)
    }

    #[cfg(feature = "mock")]
    pub fn refresh_incremental_with_mock(
        &mut self,
        context: &DiscoveryContext,
        hardware: &MockHardware,
    ) -> RuntimeResult<RuntimeRefreshReport> {
        self.refresh_incremental_with_probe(context, hardware)
    }

    #[cfg(feature = "mock")]
    pub fn refresh_incremental_with_mock_default(
        &mut self,
        hardware: &MockHardware,
    ) -> RuntimeResult<RuntimeRefreshReport> {
        self.refresh_incremental_with_mock(&DiscoveryContext::new(), hardware)
    }

    #[cfg(feature = "linux")]
    pub fn refresh_with_linux(
        &mut self,
        context: &DiscoveryContext,
        backend: &LinuxBackend,
    ) -> RuntimeResult<RuntimeRefreshReport> {
        backend.with_probes(|probes| self.runtime.refresh(context, &probes))
    }

    #[cfg(feature = "linux")]
    pub fn refresh_with_linux_default(
        &mut self,
        backend: &LinuxBackend,
    ) -> RuntimeResult<RuntimeRefreshReport> {
        self.refresh_with_linux(&DiscoveryContext::new(), backend)
    }

    #[cfg(feature = "linux")]
    pub fn refresh_incremental_with_linux(
        &mut self,
        context: &DiscoveryContext,
        backend: &LinuxBackend,
    ) -> RuntimeResult<RuntimeRefreshReport> {
        backend.with_probes(|probes| self.runtime.refresh_incremental(context, &probes))
    }

    #[cfg(feature = "linux")]
    pub fn refresh_incremental_with_linux_default(
        &mut self,
        backend: &LinuxBackend,
    ) -> RuntimeResult<RuntimeRefreshReport> {
        self.refresh_incremental_with_linux(&DiscoveryContext::new(), backend)
    }

    #[cfg(all(feature = "linux", feature = "linux-hotplug"))]
    pub fn poll_watcher_and_refresh_with_linux(
        &mut self,
        context: &DiscoveryContext,
        backend: &LinuxBackend,
        watcher: &mut LinuxHotplugWatcher,
    ) -> RuntimeResult<Option<RuntimeWatchedRefreshReport>> {
        backend.with_probes(|probes| {
            self.runtime
                .poll_watcher_and_refresh(context, &probes, watcher)
        })
    }

    #[cfg(all(feature = "linux", feature = "linux-hotplug"))]
    pub fn poll_watcher_and_refresh_incremental_with_linux(
        &mut self,
        context: &DiscoveryContext,
        backend: &LinuxBackend,
        watcher: &mut LinuxHotplugWatcher,
    ) -> RuntimeResult<Option<RuntimeWatchedRefreshReport>> {
        backend.with_probes(|probes| {
            self.runtime
                .poll_watcher_and_refresh_incremental(context, &probes, watcher)
        })
    }
}
