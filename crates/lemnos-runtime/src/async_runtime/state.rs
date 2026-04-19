use super::*;
use crate::async_runtime::sync::{read_lock, write_lock};

macro_rules! async_runtime_backend_setters {
    ($(($set:ident, $set_async:ident, $trait:ident)),+ $(,)?) => {
        $(
            pub fn $set<B>(&self, backend: B)
            where
                B: $trait + 'static,
            {
                write_lock(&self.inner).$set(backend);
            }

            pub async fn $set_async<B>(&self, backend: B) -> AsyncRuntimeResult<()>
            where
                B: $trait + Send + 'static,
            {
                self.run_blocking(move |runtime| runtime.$set(backend)).await?;
                Ok(())
            }
        )+
    };
}

impl Default for AsyncRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncRuntime {
    pub fn new() -> Self {
        Self::from_runtime(Runtime::default())
    }

    pub fn from_runtime(runtime: Runtime) -> Self {
        Self {
            inner: Arc::new(RwLock::new(runtime)),
            bind_locks: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn try_into_runtime(self) -> Result<Runtime, Self> {
        match Arc::try_unwrap(self.inner) {
            Ok(lock) => Ok(lock
                .into_inner()
                .unwrap_or_else(|poisoned| poisoned.into_inner())),
            Err(inner) => Err(Self {
                inner,
                bind_locks: self.bind_locks,
            }),
        }
    }

    pub fn shared_inventory(&self) -> Arc<InventorySnapshot> {
        read_lock(&self.inner).shared_inventory()
    }

    pub async fn shared_inventory_async(&self) -> AsyncRuntimeResult<Arc<InventorySnapshot>> {
        self.run_read_blocking(Runtime::shared_inventory).await
    }

    pub fn inventory(&self) -> InventorySnapshot {
        (*self.shared_inventory()).clone()
    }

    pub async fn inventory_async(&self) -> AsyncRuntimeResult<InventorySnapshot> {
        self.run_read_blocking(|runtime| runtime.inventory().clone())
            .await
    }

    pub fn inventory_len(&self) -> usize {
        read_lock(&self.inner).inventory_len()
    }

    pub async fn inventory_len_async(&self) -> AsyncRuntimeResult<usize> {
        self.run_read_blocking(Runtime::inventory_len).await
    }

    pub fn contains_device(&self, device_id: &DeviceId) -> bool {
        read_lock(&self.inner).contains_device(device_id)
    }

    pub async fn contains_device_async(&self, device_id: DeviceId) -> AsyncRuntimeResult<bool> {
        self.run_read_blocking(move |runtime| runtime.contains_device(&device_id))
            .await
    }

    pub fn config(&self) -> RuntimeConfig {
        *read_lock(&self.inner).config()
    }

    pub async fn config_async(&self) -> AsyncRuntimeResult<RuntimeConfig> {
        self.run_read_blocking(|runtime| *runtime.config()).await
    }

    /// Synchronously updates runtime policy under the runtime write lock.
    ///
    /// This is a small control-plane mutation intended for setup code. Use
    /// [`AsyncRuntime::set_config_async`] when you want to keep all control
    /// operations off the async caller thread.
    pub fn set_config(&self, config: RuntimeConfig) {
        write_lock(&self.inner).set_config(config);
    }

    pub async fn set_config_async(&self, config: RuntimeConfig) -> AsyncRuntimeResult<()> {
        self.run_blocking(move |runtime| runtime.set_config(config))
            .await?;
        Ok(())
    }

    /// Synchronously replaces the backend set under the runtime write lock.
    ///
    /// This is intended for initialization and tests. Use
    /// [`AsyncRuntime::set_backends_async`] to avoid doing the mutation on the
    /// async caller thread.
    pub fn set_backends(&self, backends: RuntimeBackends) {
        write_lock(&self.inner).set_backends(backends);
    }

    pub async fn set_backends_async(&self, backends: RuntimeBackends) -> AsyncRuntimeResult<()> {
        self.run_blocking(move |runtime| runtime.set_backends(backends))
            .await?;
        Ok(())
    }

    async_runtime_backend_setters! {
        (set_gpio_backend, set_gpio_backend_async, GpioBusBackend),
        (set_pwm_backend, set_pwm_backend_async, PwmBusBackend),
        (set_i2c_backend, set_i2c_backend_async, I2cBusBackend),
        (set_spi_backend, set_spi_backend_async, SpiBusBackend),
        (set_uart_backend, set_uart_backend_async, UartBusBackend),
        (set_usb_backend, set_usb_backend_async, UsbBusBackend),
    }

    pub fn is_running(&self) -> bool {
        read_lock(&self.inner).is_running()
    }

    pub async fn is_running_async(&self) -> AsyncRuntimeResult<bool> {
        self.run_read_blocking(Runtime::is_running).await
    }

    /// Synchronously flips the runtime into the running state.
    ///
    /// This is a cheap control-plane mutation. Use [`AsyncRuntime::start_async`]
    /// if you want the entire lifecycle API to stay async-shaped.
    pub fn start(&self) {
        write_lock(&self.inner).start();
    }

    pub async fn start_async(&self) -> AsyncRuntimeResult<()> {
        self.run_blocking(Runtime::start).await?;
        Ok(())
    }

    /// Synchronously shuts the runtime down and closes all bound devices.
    ///
    /// Because teardown may invoke device `close()` implementations, prefer
    /// [`AsyncRuntime::shutdown_async`] from async contexts.
    pub fn shutdown(&self) {
        write_lock(&self.inner).shutdown();
    }

    pub async fn shutdown_async(&self) -> AsyncRuntimeResult<()> {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let detached = {
                let mut runtime = write_lock(&inner);
                runtime.shutdown_detached()
            };
            crate::runtime::close_detached_bindings(detached);
        })
        .await?;
        Ok(())
    }

    pub fn shared_state(&self, device_id: &DeviceId) -> Option<Arc<DeviceStateSnapshot>> {
        read_lock(&self.inner).shared_state(device_id)
    }

    pub async fn shared_state_async(
        &self,
        device_id: DeviceId,
    ) -> AsyncRuntimeResult<Option<Arc<DeviceStateSnapshot>>> {
        self.run_read_blocking(move |runtime| runtime.shared_state(&device_id))
            .await
    }

    pub fn state(&self, device_id: &DeviceId) -> Option<DeviceStateSnapshot> {
        self.shared_state(device_id).map(|state| (*state).clone())
    }

    pub async fn state_async(
        &self,
        device_id: DeviceId,
    ) -> AsyncRuntimeResult<Option<DeviceStateSnapshot>> {
        self.shared_state_async(device_id)
            .await
            .map(|state| state.map(|state| (*state).clone()))
    }

    pub fn has_state(&self, device_id: &DeviceId) -> bool {
        read_lock(&self.inner).has_state(device_id)
    }

    pub async fn has_state_async(&self, device_id: DeviceId) -> AsyncRuntimeResult<bool> {
        self.run_read_blocking(move |runtime| runtime.has_state(&device_id))
            .await
    }

    pub fn is_bound(&self, device_id: &DeviceId) -> bool {
        read_lock(&self.inner).is_bound(device_id)
    }

    pub async fn is_bound_async(&self, device_id: DeviceId) -> AsyncRuntimeResult<bool> {
        self.run_read_blocking(move |runtime| runtime.is_bound(&device_id))
            .await
    }

    pub fn wants_binding(&self, device_id: &DeviceId) -> bool {
        read_lock(&self.inner).wants_binding(device_id)
    }

    pub async fn wants_binding_async(&self, device_id: DeviceId) -> AsyncRuntimeResult<bool> {
        self.run_read_blocking(move |runtime| runtime.wants_binding(&device_id))
            .await
    }

    pub fn failure(&self, device_id: &DeviceId) -> Option<RuntimeFailureRecord> {
        read_lock(&self.inner).failure(device_id).cloned()
    }

    pub async fn failure_async(
        &self,
        device_id: DeviceId,
    ) -> AsyncRuntimeResult<Option<RuntimeFailureRecord>> {
        self.run_read_blocking(move |runtime| runtime.failure(&device_id).cloned())
            .await
    }

    pub fn has_failure(&self, device_id: &DeviceId) -> bool {
        read_lock(&self.inner).has_failure(device_id)
    }

    pub async fn has_failure_async(&self, device_id: DeviceId) -> AsyncRuntimeResult<bool> {
        self.run_read_blocking(move |runtime| runtime.has_failure(&device_id))
            .await
    }

    pub fn events(&self) -> Vec<LemnosEvent> {
        read_lock(&self.inner).events().to_vec()
    }

    pub async fn events_async(&self) -> AsyncRuntimeResult<Vec<LemnosEvent>> {
        self.run_read_blocking(|runtime| runtime.events().to_vec())
            .await
    }

    pub fn take_events(&self) -> Vec<LemnosEvent> {
        write_lock(&self.inner).take_events()
    }

    pub async fn take_events_async(&self) -> AsyncRuntimeResult<Vec<LemnosEvent>> {
        self.run_blocking(Runtime::take_events).await
    }

    pub fn event_retention_stats(&self) -> RuntimeEventRetentionStats {
        read_lock(&self.inner).event_retention_stats()
    }

    pub async fn event_retention_stats_async(
        &self,
    ) -> AsyncRuntimeResult<RuntimeEventRetentionStats> {
        self.run_read_blocking(Runtime::event_retention_stats).await
    }

    pub fn subscribe(&self) -> AsyncRuntimeEventSubscription {
        let subscription = {
            let runtime = read_lock(&self.inner);
            runtime.subscribe_blocking()
        };
        AsyncRuntimeEventSubscription {
            runtime: Arc::clone(&self.inner),
            subscription: Arc::new(Mutex::new(subscription)),
        }
    }

    pub async fn subscribe_async(&self) -> AsyncRuntimeResult<AsyncRuntimeEventSubscription> {
        let runtime = Arc::clone(&self.inner);
        let subscription = self.run_read_blocking(Runtime::subscribe_blocking).await?;
        Ok(AsyncRuntimeEventSubscription {
            runtime,
            subscription: Arc::new(Mutex::new(subscription)),
        })
    }

    pub fn subscribe_from_start(&self) -> AsyncRuntimeEventSubscription {
        let subscription = {
            let runtime = read_lock(&self.inner);
            runtime.subscribe_from_start_blocking()
        };
        AsyncRuntimeEventSubscription {
            runtime: Arc::clone(&self.inner),
            subscription: Arc::new(Mutex::new(subscription)),
        }
    }

    pub async fn subscribe_from_start_async(
        &self,
    ) -> AsyncRuntimeResult<AsyncRuntimeEventSubscription> {
        let runtime = Arc::clone(&self.inner);
        let subscription = self
            .run_read_blocking(Runtime::subscribe_from_start_blocking)
            .await?;
        Ok(AsyncRuntimeEventSubscription {
            runtime,
            subscription: Arc::new(Mutex::new(subscription)),
        })
    }
}

impl Clone for AsyncRuntime {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            bind_locks: Arc::clone(&self.bind_locks),
        }
    }
}

impl AsyncRuntime {
    pub(crate) fn bind_lock(&self, device_id: &DeviceId) -> Arc<Mutex<()>> {
        let mut bind_locks = self
            .bind_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        bind_locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = bind_locks.get(device_id).and_then(Weak::upgrade) {
            return lock;
        }

        let lock = Arc::new(Mutex::new(()));
        bind_locks.insert(device_id.clone(), Arc::downgrade(&lock));
        lock
    }
}

impl<W> AsyncInventoryWatcher<W> {
    pub fn new(watcher: W) -> Self {
        Self {
            inner: Arc::new(Mutex::new(watcher)),
        }
    }

    pub fn try_into_inner(self) -> Result<W, Self> {
        match Arc::try_unwrap(self.inner) {
            Ok(mutex) => Ok(mutex
                .into_inner()
                .unwrap_or_else(|poisoned| poisoned.into_inner())),
            Err(inner) => Err(Self { inner }),
        }
    }

    pub(crate) fn inner(&self) -> Arc<Mutex<W>> {
        Arc::clone(&self.inner)
    }
}

impl<W> Clone for AsyncInventoryWatcher<W> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Runtime {
    pub fn into_async(self) -> AsyncRuntime {
        AsyncRuntime::from_runtime(self)
    }
}
