use super::*;
use lemnos_bus::{
    GpioBusBackend, I2cBusBackend, PwmBusBackend, SpiBusBackend, UartBusBackend, UsbBusBackend,
};

impl Runtime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(config: RuntimeConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: RuntimeConfig) {
        runtime_debug!(config = ?config, "runtime config updated");
        self.config = config;
        self.enforce_event_retention();
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn start(&mut self) {
        self.running = true;
        runtime_info!("runtime started");
    }

    pub fn shutdown(&mut self) {
        runtime_info!(
            bound_devices = self.bindings.len(),
            cached_states = self.states.len(),
            retained_events = self.events.len(),
            tracked_failures = self.failures.len(),
            "runtime shutting down"
        );
        #[cfg(not(feature = "tracing"))]
        let _ = (
            self.bindings.len(),
            self.states.len(),
            self.events.len(),
            self.failures.len(),
        );
        self.close_all_bound_devices();
        let _ = self.shutdown_detached();
    }

    pub fn inventory(&self) -> &InventorySnapshot {
        self.inventory.as_ref()
    }

    pub fn shared_inventory(&self) -> Arc<InventorySnapshot> {
        Arc::clone(&self.inventory)
    }

    pub fn inventory_len(&self) -> usize {
        self.inventory.len()
    }

    pub fn contains_device(&self, device_id: &DeviceId) -> bool {
        self.inventory.contains(device_id)
    }

    pub fn registry(&self) -> &DriverRegistry {
        &self.registry
    }

    pub fn registry_mut(&mut self) -> &mut DriverRegistry {
        &mut self.registry
    }

    pub fn register_driver<D>(&mut self, driver: D) -> RuntimeResult<()>
    where
        D: Driver + 'static,
    {
        self.registry.register(driver)?;
        Ok(())
    }

    pub fn set_backends(&mut self, backends: RuntimeBackends) {
        self.backends = backends;
    }

    pub fn set_gpio_backend<B>(&mut self, backend: B)
    where
        B: GpioBusBackend + 'static,
    {
        self.backends.set_gpio(backend);
    }

    pub fn set_pwm_backend<B>(&mut self, backend: B)
    where
        B: PwmBusBackend + 'static,
    {
        self.backends.set_pwm(backend);
    }

    pub fn set_i2c_backend<B>(&mut self, backend: B)
    where
        B: I2cBusBackend + 'static,
    {
        self.backends.set_i2c(backend);
    }

    pub fn set_spi_backend<B>(&mut self, backend: B)
    where
        B: SpiBusBackend + 'static,
    {
        self.backends.set_spi(backend);
    }

    pub fn set_uart_backend<B>(&mut self, backend: B)
    where
        B: UartBusBackend + 'static,
    {
        self.backends.set_uart(backend);
    }

    pub fn set_usb_backend<B>(&mut self, backend: B)
    where
        B: UsbBusBackend + 'static,
    {
        self.backends.set_usb(backend);
    }

    pub fn state(&self, device_id: &DeviceId) -> Option<&DeviceStateSnapshot> {
        self.states.get(device_id).map(std::sync::Arc::as_ref)
    }

    pub fn shared_state(&self, device_id: &DeviceId) -> Option<Arc<DeviceStateSnapshot>> {
        self.states.get(device_id).map(Arc::clone)
    }

    pub fn has_state(&self, device_id: &DeviceId) -> bool {
        self.states.contains_key(device_id)
    }

    pub fn is_bound(&self, device_id: &DeviceId) -> bool {
        self.bindings.contains_key(device_id)
    }

    pub fn wants_binding(&self, device_id: &DeviceId) -> bool {
        self.desired_bindings.contains(device_id)
    }

    pub fn failure(&self, device_id: &DeviceId) -> Option<&RuntimeFailureRecord> {
        self.failures.get(device_id)
    }

    pub fn has_failure(&self, device_id: &DeviceId) -> bool {
        self.failures.contains_key(device_id)
    }

    pub fn failures(&self) -> &BTreeMap<DeviceId, RuntimeFailureRecord> {
        &self.failures
    }

    #[cfg(feature = "tokio")]
    pub(crate) fn clear_failure(&mut self, device_id: &DeviceId) {
        self.failures.remove(device_id);
    }

    #[cfg(feature = "tokio")]
    pub(crate) fn mark_desired_binding(&mut self, device_id: DeviceId) {
        self.desired_bindings.insert(device_id);
    }

    pub(crate) fn ensure_running(&self) -> RuntimeResult<()> {
        if self.running {
            Ok(())
        } else {
            Err(RuntimeError::NotRunning)
        }
    }
}
