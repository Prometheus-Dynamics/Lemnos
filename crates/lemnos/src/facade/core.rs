use super::*;
use std::sync::Arc;

impl Default for Lemnos {
    fn default() -> Self {
        Self::new()
    }
}

impl Lemnos {
    pub fn new() -> Self {
        Self {
            runtime: Runtime::new(),
        }
    }

    pub fn builder() -> LemnosBuilder {
        LemnosBuilder::default()
    }

    pub fn inventory(&self) -> &InventorySnapshot {
        self.runtime.inventory()
    }

    pub fn shared_inventory(&self) -> Arc<InventorySnapshot> {
        self.runtime.shared_inventory()
    }

    pub fn inventory_len(&self) -> usize {
        self.runtime.inventory_len()
    }

    pub fn contains_device(&self, device_id: &DeviceId) -> bool {
        self.runtime.contains_device(device_id)
    }

    pub fn config(&self) -> &RuntimeConfig {
        self.runtime.config()
    }

    pub fn set_config(&mut self, config: RuntimeConfig) {
        self.runtime.set_config(config);
    }

    pub fn set_backends(&mut self, backends: RuntimeBackends) {
        self.runtime.set_backends(backends);
    }

    impl_shared_backend_methods!(sync; set_backends);

    pub fn is_running(&self) -> bool {
        self.runtime.is_running()
    }

    pub fn start(&mut self) {
        self.runtime.start();
    }

    pub fn shutdown(&mut self) {
        self.runtime.shutdown();
    }

    pub fn state(&self, device_id: &DeviceId) -> Option<&DeviceStateSnapshot> {
        self.runtime.state(device_id)
    }

    pub fn shared_state(&self, device_id: &DeviceId) -> Option<Arc<DeviceStateSnapshot>> {
        self.runtime.shared_state(device_id)
    }

    pub fn has_state(&self, device_id: &DeviceId) -> bool {
        self.runtime.has_state(device_id)
    }

    pub fn is_bound(&self, device_id: &DeviceId) -> bool {
        self.runtime.is_bound(device_id)
    }

    pub fn wants_binding(&self, device_id: &DeviceId) -> bool {
        self.runtime.wants_binding(device_id)
    }

    pub fn failure(&self, device_id: &DeviceId) -> Option<&RuntimeFailureRecord> {
        self.runtime.failure(device_id)
    }

    pub fn has_failure(&self, device_id: &DeviceId) -> bool {
        self.runtime.has_failure(device_id)
    }

    pub fn events(&self) -> &[LemnosEvent] {
        self.runtime.events()
    }

    pub fn subscribe(&self) -> RuntimeEventCursor {
        self.runtime.subscribe()
    }

    pub fn subscribe_from_start(&self) -> RuntimeEventCursor {
        self.runtime.subscribe_from_start()
    }

    pub fn subscribe_blocking(&self) -> RuntimeEventSubscription {
        self.runtime.subscribe_blocking()
    }

    pub fn subscribe_from_start_blocking(&self) -> RuntimeEventSubscription {
        self.runtime.subscribe_from_start_blocking()
    }

    pub fn poll_events<'a>(&'a self, cursor: &mut RuntimeEventCursor) -> &'a [LemnosEvent] {
        self.runtime.poll_events(cursor)
    }

    pub fn take_events(&mut self) -> Vec<LemnosEvent> {
        self.runtime.take_events()
    }

    pub fn event_retention_stats(&self) -> RuntimeEventRetentionStats {
        self.runtime.event_retention_stats()
    }

    pub fn register_driver<D>(&mut self, driver: D) -> RuntimeResult<()>
    where
        D: Driver + 'static,
    {
        self.runtime.register_driver(driver)
    }

    #[cfg(feature = "builtin-drivers")]
    pub fn register_builtin_drivers(&mut self) -> RuntimeResult<()> {
        BuiltInDriverBundle::register_into(&mut self.runtime)
    }

    pub fn prefer_driver_for_device(
        &mut self,
        device_id: impl Into<DeviceId>,
        driver_id: impl Into<DriverId>,
    ) -> RuntimeResult<()> {
        self.prefer_driver_id_for_device(device_id, driver_id)
    }

    pub fn prefer_driver_id_for_device(
        &mut self,
        device_id: impl Into<DeviceId>,
        driver_id: impl Into<DriverId>,
    ) -> RuntimeResult<()> {
        self.runtime
            .registry_mut()
            .prefer_driver_for_device(device_id, driver_id)?;
        Ok(())
    }

    pub fn clear_preferred_driver_for_device(&mut self, device_id: &DeviceId) -> Option<String> {
        self.clear_preferred_driver_id_for_device(device_id)
            .map(DriverId::into_string)
    }

    pub fn clear_preferred_driver_id_for_device(
        &mut self,
        device_id: &DeviceId,
    ) -> Option<DriverId> {
        self.runtime
            .registry_mut()
            .clear_preferred_driver_for_device(device_id)
    }

    pub fn preferred_driver_for_device(&self, device_id: &DeviceId) -> Option<&str> {
        self.preferred_driver_id_for_device(device_id)
            .map(DriverId::as_str)
    }

    pub fn preferred_driver_id_for_device(&self, device_id: &DeviceId) -> Option<&DriverId> {
        self.runtime
            .registry()
            .preferred_driver_for_device(device_id)
    }

    impl_runtime_backend_setters!(
        (set_gpio_backend, GpioBusBackend),
        (set_pwm_backend, PwmBusBackend),
        (set_i2c_backend, I2cBusBackend),
        (set_spi_backend, SpiBusBackend),
        (set_uart_backend, UartBusBackend),
        (set_usb_backend, UsbBusBackend),
    );

    impl_mock_backend_methods!(sync);

    pub fn bind(&mut self, device_id: &DeviceId) -> RuntimeResult<()> {
        self.runtime.bind(device_id)
    }

    pub fn unbind(&mut self, device_id: &DeviceId) -> bool {
        self.runtime.unbind(device_id)
    }

    pub fn refresh_state(
        &mut self,
        device_id: &DeviceId,
    ) -> RuntimeResult<Option<&DeviceStateSnapshot>> {
        self.runtime.refresh_state(device_id)
    }

    pub fn refresh_state_shared(
        &mut self,
        device_id: &DeviceId,
    ) -> RuntimeResult<Option<Arc<DeviceStateSnapshot>>> {
        self.runtime.refresh_state_shared(device_id)
    }
}
