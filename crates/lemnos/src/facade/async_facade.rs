use super::*;
use lemnos_runtime::{
    AsyncRuntime, AsyncRuntimeEventSubscription, AsyncRuntimeResult, SharedDiscoveryProbe,
};
use std::sync::Arc;

macro_rules! impl_async_backend_setters_async {
    ($(($name:ident, $trait:ident, $runtime_name:ident)),+ $(,)?) => {
        $(
            pub async fn $name<B>(&self, backend: B) -> AsyncRuntimeResult<()>
            where
                B: $trait + Send + 'static,
            {
                self.runtime.$runtime_name(backend).await
            }
        )+
    };
}

/// Tokio-backed async facade over [`Lemnos`]'s synchronous runtime.
///
/// The async surface is additive for `0.1`: transport and driver code remain
/// synchronous internally, while this wrapper offloads blocking work onto
/// Tokio's blocking pool. Mutable runtime work still commits through one
/// runtime instance, but read-side queries and retained-event polling can
/// overlap with unrelated mutations, and refresh probe execution happens
/// outside the runtime write lock before the final commit step. This API is
/// for async integration convenience rather than parallel request throughput.
///
/// Methods without an `_async` suffix are synchronous escape hatches inherited
/// from the underlying runtime. They may block the current thread on
/// `std::sync` locks, so prefer the `_async` variants when calling from Tokio.
#[derive(Clone)]
pub struct AsyncLemnos {
    runtime: AsyncRuntime,
}

impl Default for AsyncLemnos {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncLemnos {
    pub fn new() -> Self {
        Self::from_runtime(Runtime::default())
    }

    pub fn from_runtime(runtime: Runtime) -> Self {
        Self {
            runtime: runtime.into_async(),
        }
    }

    pub fn runtime(&self) -> &AsyncRuntime {
        &self.runtime
    }

    pub fn inventory(&self) -> InventorySnapshot {
        self.runtime.inventory()
    }

    pub fn shared_inventory(&self) -> Arc<InventorySnapshot> {
        self.runtime.shared_inventory()
    }

    pub async fn shared_inventory_async(&self) -> AsyncRuntimeResult<Arc<InventorySnapshot>> {
        self.runtime.shared_inventory_async().await
    }

    pub fn inventory_len(&self) -> usize {
        self.runtime.inventory_len()
    }

    pub async fn inventory_async(&self) -> AsyncRuntimeResult<InventorySnapshot> {
        self.runtime.inventory_async().await
    }

    pub async fn inventory_len_async(&self) -> AsyncRuntimeResult<usize> {
        self.runtime.inventory_len_async().await
    }

    pub fn contains_device(&self, device_id: &DeviceId) -> bool {
        self.runtime.contains_device(device_id)
    }

    pub async fn contains_device_async(&self, device_id: DeviceId) -> AsyncRuntimeResult<bool> {
        self.runtime.contains_device_async(device_id).await
    }

    pub fn config(&self) -> RuntimeConfig {
        self.runtime.config()
    }

    pub async fn config_async(&self) -> AsyncRuntimeResult<RuntimeConfig> {
        self.runtime.config_async().await
    }

    pub fn set_config(&self, config: RuntimeConfig) {
        self.runtime.set_config(config);
    }

    pub async fn set_config_async(&self, config: RuntimeConfig) -> AsyncRuntimeResult<()> {
        self.runtime.set_config_async(config).await
    }

    pub fn set_backends(&self, backends: RuntimeBackends) {
        self.runtime.set_backends(backends);
    }

    pub async fn set_backends_async(&self, backends: RuntimeBackends) -> AsyncRuntimeResult<()> {
        self.runtime.set_backends_async(backends).await
    }

    impl_shared_backend_methods!(async; set_backends);

    impl_async_runtime_backend_setters!(
        (set_gpio_backend, GpioBusBackend),
        (set_pwm_backend, PwmBusBackend),
        (set_i2c_backend, I2cBusBackend),
        (set_spi_backend, SpiBusBackend),
        (set_uart_backend, UartBusBackend),
        (set_usb_backend, UsbBusBackend),
    );

    impl_mock_backend_methods!(async);

    impl_async_backend_setters_async! {
        (set_gpio_backend_async, GpioBusBackend, set_gpio_backend_async),
        (set_pwm_backend_async, PwmBusBackend, set_pwm_backend_async),
        (set_i2c_backend_async, I2cBusBackend, set_i2c_backend_async),
        (set_spi_backend_async, SpiBusBackend, set_spi_backend_async),
        (set_uart_backend_async, UartBusBackend, set_uart_backend_async),
        (set_usb_backend_async, UsbBusBackend, set_usb_backend_async),
    }

    pub fn is_running(&self) -> bool {
        self.runtime.is_running()
    }

    pub async fn is_running_async(&self) -> AsyncRuntimeResult<bool> {
        self.runtime.is_running_async().await
    }

    pub fn start(&self) {
        self.runtime.start();
    }

    pub async fn start_async(&self) -> AsyncRuntimeResult<()> {
        self.runtime.start_async().await
    }

    pub fn shutdown(&self) {
        self.runtime.shutdown();
    }

    pub async fn shutdown_async(&self) -> AsyncRuntimeResult<()> {
        self.runtime.shutdown_async().await
    }

    pub fn state(&self, device_id: &DeviceId) -> Option<DeviceStateSnapshot> {
        self.runtime.state(device_id)
    }

    pub fn shared_state(&self, device_id: &DeviceId) -> Option<Arc<DeviceStateSnapshot>> {
        self.runtime.shared_state(device_id)
    }

    pub async fn shared_state_async(
        &self,
        device_id: DeviceId,
    ) -> AsyncRuntimeResult<Option<Arc<DeviceStateSnapshot>>> {
        self.runtime.shared_state_async(device_id).await
    }

    pub async fn state_async(
        &self,
        device_id: DeviceId,
    ) -> AsyncRuntimeResult<Option<DeviceStateSnapshot>> {
        self.runtime.state_async(device_id).await
    }

    pub fn has_state(&self, device_id: &DeviceId) -> bool {
        self.runtime.has_state(device_id)
    }

    pub async fn has_state_async(&self, device_id: DeviceId) -> AsyncRuntimeResult<bool> {
        self.runtime.has_state_async(device_id).await
    }

    pub fn is_bound(&self, device_id: &DeviceId) -> bool {
        self.runtime.is_bound(device_id)
    }

    pub async fn is_bound_async(&self, device_id: DeviceId) -> AsyncRuntimeResult<bool> {
        self.runtime.is_bound_async(device_id).await
    }

    pub fn wants_binding(&self, device_id: &DeviceId) -> bool {
        self.runtime.wants_binding(device_id)
    }

    pub async fn wants_binding_async(&self, device_id: DeviceId) -> AsyncRuntimeResult<bool> {
        self.runtime.wants_binding_async(device_id).await
    }

    pub fn failure(&self, device_id: &DeviceId) -> Option<RuntimeFailureRecord> {
        self.runtime.failure(device_id)
    }

    pub async fn failure_async(
        &self,
        device_id: DeviceId,
    ) -> AsyncRuntimeResult<Option<RuntimeFailureRecord>> {
        self.runtime.failure_async(device_id).await
    }

    pub fn has_failure(&self, device_id: &DeviceId) -> bool {
        self.runtime.has_failure(device_id)
    }

    pub async fn has_failure_async(&self, device_id: DeviceId) -> AsyncRuntimeResult<bool> {
        self.runtime.has_failure_async(device_id).await
    }

    pub fn events(&self) -> Vec<LemnosEvent> {
        self.runtime.events()
    }

    pub async fn events_async(&self) -> AsyncRuntimeResult<Vec<LemnosEvent>> {
        self.runtime.events_async().await
    }

    pub fn take_events(&self) -> Vec<LemnosEvent> {
        self.runtime.take_events()
    }

    pub async fn take_events_async(&self) -> AsyncRuntimeResult<Vec<LemnosEvent>> {
        self.runtime.take_events_async().await
    }

    pub fn event_retention_stats(&self) -> RuntimeEventRetentionStats {
        self.runtime.event_retention_stats()
    }

    pub async fn event_retention_stats_async(
        &self,
    ) -> AsyncRuntimeResult<RuntimeEventRetentionStats> {
        self.runtime.event_retention_stats_async().await
    }

    pub fn subscribe(&self) -> AsyncRuntimeEventSubscription {
        self.runtime.subscribe()
    }

    pub async fn subscribe_async(&self) -> AsyncRuntimeResult<AsyncRuntimeEventSubscription> {
        self.runtime.subscribe_async().await
    }

    pub fn subscribe_from_start(&self) -> AsyncRuntimeEventSubscription {
        self.runtime.subscribe_from_start()
    }

    pub async fn subscribe_from_start_async(
        &self,
    ) -> AsyncRuntimeResult<AsyncRuntimeEventSubscription> {
        self.runtime.subscribe_from_start_async().await
    }

    pub async fn register_driver<D>(&self, driver: D) -> AsyncRuntimeResult<()>
    where
        D: Driver + 'static,
    {
        self.runtime
            .with_runtime(move |runtime| runtime.register_driver(driver))
            .await
    }

    #[cfg(feature = "builtin-drivers")]
    pub async fn register_builtin_drivers(&self) -> AsyncRuntimeResult<()> {
        self.runtime
            .with_runtime(BuiltInDriverBundle::register_into)
            .await
    }

    pub async fn prefer_driver_for_device(
        &self,
        device_id: impl Into<DeviceId>,
        driver_id: impl Into<DriverId>,
    ) -> AsyncRuntimeResult<()> {
        self.prefer_driver_id_for_device(device_id, driver_id).await
    }

    pub async fn prefer_driver_id_for_device(
        &self,
        device_id: impl Into<DeviceId>,
        driver_id: impl Into<DriverId>,
    ) -> AsyncRuntimeResult<()> {
        let device_id = device_id.into();
        let driver_id = driver_id.into();
        self.runtime
            .with_runtime(move |runtime| {
                runtime
                    .registry_mut()
                    .prefer_driver_for_device(device_id, driver_id)?;
                Ok(())
            })
            .await
    }

    pub async fn clear_preferred_driver_for_device(
        &self,
        device_id: DeviceId,
    ) -> AsyncRuntimeResult<Option<String>> {
        self.clear_preferred_driver_id_for_device(device_id)
            .await
            .map(|driver_id| driver_id.map(DriverId::into_string))
    }

    pub async fn clear_preferred_driver_id_for_device(
        &self,
        device_id: DeviceId,
    ) -> AsyncRuntimeResult<Option<DriverId>> {
        self.runtime
            .with_runtime(move |runtime| {
                Ok(runtime
                    .registry_mut()
                    .clear_preferred_driver_for_device(&device_id))
            })
            .await
    }

    pub async fn preferred_driver_for_device(
        &self,
        device_id: DeviceId,
    ) -> AsyncRuntimeResult<Option<String>> {
        self.preferred_driver_id_for_device(device_id)
            .await
            .map(|driver_id| driver_id.map(DriverId::into_string))
    }

    pub async fn preferred_driver_id_for_device(
        &self,
        device_id: DeviceId,
    ) -> AsyncRuntimeResult<Option<DriverId>> {
        self.runtime
            .with_runtime(move |runtime| {
                Ok(runtime
                    .registry()
                    .preferred_driver_for_device(&device_id)
                    .cloned())
            })
            .await
    }

    pub async fn bind(&self, device_id: impl Into<DeviceId>) -> AsyncRuntimeResult<()> {
        self.runtime.bind(device_id.into()).await
    }

    pub async fn unbind(&self, device_id: impl Into<DeviceId>) -> AsyncRuntimeResult<bool> {
        self.runtime.unbind(device_id.into()).await
    }

    pub async fn refresh_state(
        &self,
        device_id: impl Into<DeviceId>,
    ) -> AsyncRuntimeResult<Option<DeviceStateSnapshot>> {
        self.runtime.refresh_state(device_id.into()).await
    }

    pub async fn refresh_state_shared(
        &self,
        device_id: impl Into<DeviceId>,
    ) -> AsyncRuntimeResult<Option<Arc<DeviceStateSnapshot>>> {
        self.runtime.refresh_state_shared(device_id.into()).await
    }

    pub async fn request(&self, request: DeviceRequest) -> AsyncRuntimeResult<DeviceResponse> {
        self.runtime.request(request).await
    }

    pub async fn request_standard(
        &self,
        device_id: impl Into<DeviceId>,
        request: StandardRequest,
    ) -> AsyncRuntimeResult<DeviceResponse> {
        self.request(DeviceRequest::new(
            device_id.into(),
            InteractionRequest::Standard(request),
        ))
        .await
    }

    impl_standard_request_helpers!(async;
        (request_gpio, GpioRequest, Gpio),
        (request_pwm, PwmRequest, Pwm),
        (request_i2c, I2cRequest, I2c),
        (request_spi, SpiRequest, Spi),
        (request_uart, UartRequest, Uart),
        (request_usb, UsbRequest, Usb),
    );

    impl_simple_standard_request_helpers!(async;
        (read_gpio, Gpio, GpioRequest::Read),
        (gpio_configuration, Gpio, GpioRequest::GetConfiguration),
        (pwm_configuration, Pwm, PwmRequest::GetConfiguration),
        (spi_configuration, Spi, SpiRequest::GetConfiguration),
        (uart_configuration, Uart, UartRequest::GetConfiguration),
    );

    impl_parameterized_standard_request_helpers!(async;
        (write_gpio, (level: GpioLevel), Gpio, GpioRequest::Write { level }),
        (
            configure_gpio,
            (configuration: GpioLineConfiguration),
            Gpio,
            GpioRequest::Configure(configuration)
        ),
        (enable_pwm, (enabled: bool), Pwm, PwmRequest::Enable { enabled }),
        (
            configure_pwm,
            (configuration: PwmConfiguration),
            Pwm,
            PwmRequest::Configure(configuration)
        ),
        (
            set_pwm_period,
            (period_ns: u64),
            Pwm,
            PwmRequest::SetPeriod { period_ns }
        ),
        (
            set_pwm_duty_cycle,
            (duty_cycle_ns: u64),
            Pwm,
            PwmRequest::SetDutyCycle { duty_cycle_ns }
        ),
        (
            claim_usb_interface,
            (interface_number: u8, alternate_setting: Option<u8>),
            Usb,
            UsbRequest::ClaimInterface {
                interface_number,
                alternate_setting,
            }
        ),
        (
            release_usb_interface,
            (interface_number: u8),
            Usb,
            UsbRequest::ReleaseInterface { interface_number }
        ),
    );

    pub async fn request_custom(
        &self,
        device_id: impl Into<DeviceId>,
        interaction_id: impl TryInto<InteractionId, Error = lemnos_core::CoreError>,
    ) -> AsyncRuntimeResult<DeviceResponse> {
        self.request_custom_with_input(device_id, interaction_id, None::<Value>)
            .await
    }

    pub async fn request_custom_value(
        &self,
        device_id: impl Into<DeviceId>,
        interaction_id: impl TryInto<InteractionId, Error = lemnos_core::CoreError>,
        input: impl Into<Value>,
    ) -> AsyncRuntimeResult<DeviceResponse> {
        self.request_custom_with_input(device_id, interaction_id, Some(input.into()))
            .await
    }

    pub async fn request_custom_with_input(
        &self,
        device_id: impl Into<DeviceId>,
        interaction_id: impl TryInto<InteractionId, Error = lemnos_core::CoreError>,
        input: impl Into<Option<Value>>,
    ) -> AsyncRuntimeResult<DeviceResponse> {
        let device_id = device_id.into();
        self.request(super::build_custom_request(
            device_id,
            interaction_id,
            input.into(),
        )?)
        .await
    }

    pub async fn refresh(
        &self,
        context: DiscoveryContext,
        probes: Vec<SharedDiscoveryProbe>,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        self.runtime.refresh(context, probes).await
    }

    pub async fn refresh_default(
        &self,
        probes: Vec<SharedDiscoveryProbe>,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        self.refresh(DiscoveryContext::new(), probes).await
    }

    pub async fn refresh_incremental(
        &self,
        context: DiscoveryContext,
        probes: Vec<SharedDiscoveryProbe>,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        self.runtime.refresh_incremental(context, probes).await
    }

    pub async fn refresh_incremental_default(
        &self,
        probes: Vec<SharedDiscoveryProbe>,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport> {
        self.refresh_incremental(DiscoveryContext::new(), probes)
            .await
    }

    pub async fn refresh_with_probe<P>(
        &self,
        context: DiscoveryContext,
        probe: P,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport>
    where
        P: DiscoveryProbe + 'static,
    {
        self.refresh(context, vec![Arc::new(probe)]).await
    }

    pub async fn refresh_with_probe_default<P>(
        &self,
        probe: P,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport>
    where
        P: DiscoveryProbe + 'static,
    {
        self.refresh_with_probe(DiscoveryContext::new(), probe)
            .await
    }

    pub async fn refresh_incremental_with_probe<P>(
        &self,
        context: DiscoveryContext,
        probe: P,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport>
    where
        P: DiscoveryProbe + 'static,
    {
        self.refresh_incremental(context, vec![Arc::new(probe)])
            .await
    }

    pub async fn refresh_incremental_with_probe_default<P>(
        &self,
        probe: P,
    ) -> AsyncRuntimeResult<RuntimeRefreshReport>
    where
        P: DiscoveryProbe + 'static,
    {
        self.refresh_incremental_with_probe(DiscoveryContext::new(), probe)
            .await
    }
}

impl Lemnos {
    pub fn into_async(self) -> AsyncLemnos {
        AsyncLemnos {
            runtime: self.runtime.into_async(),
        }
    }
}
