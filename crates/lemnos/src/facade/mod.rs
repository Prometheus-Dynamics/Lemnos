#[cfg(feature = "builtin-drivers")]
use crate::builtin::BuiltInDriverBundle;
use crate::driver::Driver;
use lemnos_bus::{
    GpioBusBackend, I2cBusBackend, PwmBusBackend, SpiBusBackend, UartBusBackend, UsbBusBackend,
};
use lemnos_core::{
    CustomInteractionRequest, DeviceId, DeviceRequest, DeviceResponse, DeviceStateSnapshot,
    GpioLevel, GpioLineConfiguration, GpioRequest, I2cRequest, InteractionId, InteractionRequest,
    LemnosEvent, PwmConfiguration, PwmRequest, SpiRequest, StandardRequest, UartRequest,
    UsbRequest, Value,
};
use lemnos_discovery::{DiscoveryContext, DiscoveryProbe, InventorySnapshot, InventoryWatcher};
#[cfg(all(feature = "linux", feature = "linux-hotplug"))]
use lemnos_linux::{LinuxBackend, LinuxHotplugWatcher, LinuxPaths, LinuxTransportConfig};
#[cfg(all(feature = "linux", not(feature = "linux-hotplug")))]
use lemnos_linux::{LinuxBackend, LinuxPaths, LinuxTransportConfig};
#[cfg(feature = "mock")]
use lemnos_mock::MockHardware;
use lemnos_runtime::{
    DriverId, Runtime, RuntimeBackends, RuntimeConfig, RuntimeEventCursor,
    RuntimeEventRetentionStats, RuntimeEventSubscription, RuntimeFailureRecord,
    RuntimeRefreshReport, RuntimeResult, RuntimeWatchedRefreshReport,
};

macro_rules! impl_runtime_backend_setters {
    ($(($name:ident, $trait:ident)),+ $(,)?) => {
        $(
            pub fn $name<B>(&mut self, backend: B)
            where
                B: $trait + 'static,
            {
                self.runtime.$name(backend);
            }
        )+
    };
}

#[doc(hidden)]
pub trait FacadeSharedBackend:
    GpioBusBackend
    + PwmBusBackend
    + I2cBusBackend
    + SpiBusBackend
    + UartBusBackend
    + UsbBusBackend
    + 'static
{
}

impl<T> FacadeSharedBackend for T where
    T: GpioBusBackend
        + PwmBusBackend
        + I2cBusBackend
        + SpiBusBackend
        + UartBusBackend
        + UsbBusBackend
        + 'static
{
}

fn build_custom_request<I>(
    device_id: DeviceId,
    interaction_id: I,
    input: Option<Value>,
) -> RuntimeResult<DeviceRequest>
where
    I: TryInto<InteractionId, Error = lemnos_core::CoreError>,
{
    let mut request = CustomInteractionRequest::new(interaction_id).map_err(|source| {
        lemnos_runtime::RuntimeError::InvalidRequest {
            device_id: device_id.clone(),
            source: Box::new(source),
        }
    })?;
    if let Some(input) = input {
        request = request.with_input(input);
    }
    Ok(DeviceRequest::new(
        device_id,
        InteractionRequest::Custom(request),
    ))
}

#[cfg(feature = "tokio")]
macro_rules! impl_async_runtime_backend_setters {
    ($(($name:ident, $trait:ident)),+ $(,)?) => {
        $(
            pub fn $name<B>(&self, backend: B)
            where
                B: $trait + 'static,
            {
                self.runtime.$name(backend);
            }
        )+
    };
}

macro_rules! impl_shared_backend_methods {
    (sync; $set_backends:ident) => {
        pub fn set_shared_backend<B>(&mut self, backend: B)
        where
            B: FacadeSharedBackend,
        {
            self.$set_backends(RuntimeBackends::default().with_shared_backend(backend));
        }

        pub fn set_shared_backend_ref<B>(&mut self, backend: &B)
        where
            B: FacadeSharedBackend + Clone,
        {
            self.set_shared_backend(backend.clone());
        }
    };
    (async; $set_backends:ident) => {
        pub fn set_shared_backend<B>(&self, backend: B)
        where
            B: FacadeSharedBackend,
        {
            self.$set_backends(RuntimeBackends::default().with_shared_backend(backend));
        }

        pub fn set_shared_backend_ref<B>(&self, backend: &B)
        where
            B: FacadeSharedBackend + Clone,
        {
            self.set_shared_backend(backend.clone());
        }
    };
    (builder; $with_backends:ident) => {
        pub fn with_shared_backend<B>(self, backend: B) -> Self
        where
            B: FacadeSharedBackend,
        {
            self.$with_backends(RuntimeBackends::default().with_shared_backend(backend))
        }

        pub fn with_shared_backend_ref<B>(self, backend: &B) -> Self
        where
            B: FacadeSharedBackend + Clone,
        {
            self.with_shared_backend(backend.clone())
        }
    };
}

macro_rules! impl_mock_backend_methods {
    (sync) => {
        #[cfg(feature = "mock")]
        pub fn set_mock_hardware(&mut self, hardware: MockHardware) {
            self.set_shared_backend(hardware);
        }

        #[cfg(feature = "mock")]
        pub fn set_mock_hardware_ref(&mut self, hardware: &MockHardware) {
            self.set_mock_hardware(hardware.clone());
        }
    };
    (async) => {
        #[cfg(feature = "mock")]
        pub fn set_mock_hardware(&self, hardware: MockHardware) {
            self.set_shared_backend(hardware);
        }

        #[cfg(feature = "mock")]
        pub fn set_mock_hardware_ref(&self, hardware: &MockHardware) {
            self.set_mock_hardware(hardware.clone());
        }
    };
    (builder) => {
        #[cfg(feature = "mock")]
        pub fn with_mock_hardware(self, hardware: MockHardware) -> Self {
            self.with_shared_backend(hardware)
        }

        #[cfg(feature = "mock")]
        pub fn with_mock_hardware_ref(self, hardware: &MockHardware) -> Self {
            self.with_mock_hardware(hardware.clone())
        }
    };
}

macro_rules! impl_linux_backend_methods {
    (sync) => {
        #[cfg(feature = "linux")]
        pub fn set_linux_backend(&mut self, backend: LinuxBackend) {
            self.set_shared_backend(backend);
        }

        #[cfg(feature = "linux")]
        pub fn set_linux_backend_ref(&mut self, backend: &LinuxBackend) {
            self.set_linux_backend(backend.clone());
        }

        #[cfg(feature = "linux")]
        pub fn set_linux_paths(&mut self, paths: LinuxPaths) {
            self.set_linux_backend(LinuxBackend::with_paths(paths));
        }

        #[cfg(feature = "linux")]
        pub fn set_linux_transport_config(&mut self, transport_config: LinuxTransportConfig) {
            self.set_linux_backend(LinuxBackend::with_config(transport_config));
        }

        #[cfg(feature = "linux")]
        pub fn set_linux_paths_and_config(
            &mut self,
            paths: LinuxPaths,
            transport_config: LinuxTransportConfig,
        ) {
            self.set_linux_backend(LinuxBackend::with_paths_and_config(paths, transport_config));
        }
    };
    (async) => {
        #[cfg(feature = "linux")]
        pub fn set_linux_backend(&self, backend: LinuxBackend) {
            self.set_shared_backend(backend);
        }

        #[cfg(feature = "linux")]
        pub fn set_linux_backend_ref(&self, backend: &LinuxBackend) {
            self.set_linux_backend(backend.clone());
        }

        #[cfg(feature = "linux")]
        pub fn set_linux_paths(&self, paths: LinuxPaths) {
            self.set_linux_backend(LinuxBackend::with_paths(paths));
        }

        #[cfg(feature = "linux")]
        pub fn set_linux_transport_config(&self, transport_config: LinuxTransportConfig) {
            self.set_linux_backend(LinuxBackend::with_config(transport_config));
        }

        #[cfg(feature = "linux")]
        pub fn set_linux_paths_and_config(
            &self,
            paths: LinuxPaths,
            transport_config: LinuxTransportConfig,
        ) {
            self.set_linux_backend(LinuxBackend::with_paths_and_config(paths, transport_config));
        }
    };
    (builder) => {
        #[cfg(feature = "linux")]
        pub fn with_linux_backend(self, backend: LinuxBackend) -> Self {
            self.with_shared_backend(backend)
        }

        #[cfg(feature = "linux")]
        pub fn with_linux_backend_ref(self, backend: &LinuxBackend) -> Self {
            self.with_linux_backend(backend.clone())
        }

        #[cfg(feature = "linux")]
        pub fn with_linux_paths(self, paths: LinuxPaths) -> Self {
            self.with_linux_backend(LinuxBackend::with_paths(paths))
        }

        #[cfg(feature = "linux")]
        pub fn with_linux_transport_config(self, transport_config: LinuxTransportConfig) -> Self {
            self.with_linux_backend(LinuxBackend::with_config(transport_config))
        }

        #[cfg(feature = "linux")]
        pub fn with_linux_paths_and_config(
            self,
            paths: LinuxPaths,
            transport_config: LinuxTransportConfig,
        ) -> Self {
            self.with_linux_backend(LinuxBackend::with_paths_and_config(paths, transport_config))
        }
    };
}

macro_rules! impl_standard_request_helpers {
    (sync; $(($name:ident, $request_ty:ident, $variant:ident)),+ $(,)?) => {
        $(
            pub fn $name(
                &mut self,
                device_id: impl Into<DeviceId>,
                request: $request_ty,
            ) -> RuntimeResult<DeviceResponse> {
                self.request_standard(device_id, StandardRequest::$variant(request))
            }
        )+
    };
    (async; $(($name:ident, $request_ty:ident, $variant:ident)),+ $(,)?) => {
        $(
            pub async fn $name(
                &self,
                device_id: impl Into<DeviceId>,
                request: $request_ty,
            ) -> AsyncRuntimeResult<DeviceResponse> {
                self.request_standard(device_id, StandardRequest::$variant(request)).await
            }
        )+
    };
}

macro_rules! impl_simple_standard_request_helpers {
    (sync; $(($name:ident, $variant:ident, $request:expr)),+ $(,)?) => {
        $(
            pub fn $name(
                &mut self,
                device_id: impl Into<DeviceId>,
            ) -> RuntimeResult<DeviceResponse> {
                self.request_standard(device_id, StandardRequest::$variant($request))
            }
        )+
    };
    (async; $(($name:ident, $variant:ident, $request:expr)),+ $(,)?) => {
        $(
            pub async fn $name(
                &self,
                device_id: impl Into<DeviceId>,
            ) -> AsyncRuntimeResult<DeviceResponse> {
                self.request_standard(device_id, StandardRequest::$variant($request)).await
            }
        )+
    };
}

macro_rules! impl_parameterized_standard_request_helpers {
    (sync; $(($name:ident, ($($arg:ident: $arg_ty:ty),* $(,)?), $variant:ident, $request:expr)),+ $(,)?) => {
        $(
            pub fn $name(
                &mut self,
                device_id: impl Into<DeviceId>,
                $($arg: $arg_ty),*
            ) -> RuntimeResult<DeviceResponse> {
                self.request_standard(device_id, StandardRequest::$variant($request))
            }
        )+
    };
    (async; $(($name:ident, ($($arg:ident: $arg_ty:ty),* $(,)?), $variant:ident, $request:expr)),+ $(,)?) => {
        $(
            pub async fn $name(
                &self,
                device_id: impl Into<DeviceId>,
                $($arg: $arg_ty),*
            ) -> AsyncRuntimeResult<DeviceResponse> {
                self.request_standard(device_id, StandardRequest::$variant($request)).await
            }
        )+
    };
}

macro_rules! impl_builder_backend_methods {
    ($(($name:ident, $setter:ident, $trait:ident)),+ $(,)?) => {
        $(
            pub fn $name<B>(mut self, backend: B) -> Self
            where
                B: $trait + 'static,
            {
                self.runtime.$setter(backend);
                self
            }
        )+
    };
}

/// Synchronous end-user facade over the Lemnos runtime and its configured
/// backends.
///
/// This type keeps the direct control-plane model of the underlying runtime:
/// refresh, bind, request, and watcher polling all execute on the caller
/// thread.
pub struct Lemnos {
    runtime: Runtime,
}

/// Builder for [`Lemnos`] runtime instances.
///
/// Use this to assemble backends, runtime policy, and optional built-in driver
/// registration before handing the configured facade to application code.
#[derive(Default)]
pub struct LemnosBuilder {
    runtime: Runtime,
}

#[cfg(feature = "tokio")]
mod async_facade;
#[cfg(feature = "tokio")]
mod async_linux;
#[cfg(feature = "tokio")]
pub use async_facade::AsyncLemnos;
mod builder;
mod core;
mod linux;
mod requests;
