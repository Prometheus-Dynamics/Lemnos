use lemnos_core::{DeviceDescriptor, InterfaceKind};

/// Runtime-facing backend dispatch stays trait-object-based so one runtime can
/// mix heterogeneous backend implementations chosen at startup. Concrete
/// transports should keep their hot-path work behind this boundary.
pub trait BusBackend: Send + Sync {
    fn name(&self) -> &str;
    fn supported_interfaces(&self) -> &'static [InterfaceKind];
    fn supports_device(&self, device: &DeviceDescriptor) -> bool;
}
