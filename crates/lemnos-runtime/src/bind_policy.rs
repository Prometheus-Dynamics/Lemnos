use lemnos_core::{DeviceDescriptor, DeviceKind, InterfaceKind};
use std::collections::BTreeSet;

/// Which newly discovered devices the runtime binds on its own during refresh.
///
/// The default policy binds nothing: devices stay unbound until a caller
/// binds them explicitly or, with
/// [`RuntimeConfig::auto_bind_on_request`](crate::RuntimeConfig::auto_bind_on_request),
/// on their first request. Unbound devices still report a status through
/// [`Runtime::device_status`](crate::Runtime::device_status) without opening
/// any handles.
///
/// A non-empty policy is evaluated against devices that a refresh adds or
/// changes. A device is bound when a registered driver supports it and it
/// matches any configured rule (interface, kind, or resolved driver id).
/// Devices whose bind fails are not retried until they change again.
///
/// ```
/// use lemnos_runtime::RuntimeBindPolicy;
///
/// // Keep fan telemetry live without opening every GPIO line.
/// let policy = RuntimeBindPolicy::new().with_driver("linux.hwmon-fan");
/// assert!(!policy.is_empty());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeBindPolicy {
    all: bool,
    interfaces: BTreeSet<InterfaceKind>,
    kinds: BTreeSet<DeviceKind>,
    driver_ids: BTreeSet<String>,
}

impl RuntimeBindPolicy {
    /// A policy that binds nothing on refresh.
    pub fn new() -> Self {
        Self::default()
    }

    /// A policy that binds every discovered device a registered driver supports.
    pub fn all() -> Self {
        Self {
            all: true,
            ..Self::default()
        }
    }

    pub fn with_interface(mut self, interface: InterfaceKind) -> Self {
        self.interfaces.insert(interface);
        self
    }

    pub fn with_kind(mut self, kind: DeviceKind) -> Self {
        self.kinds.insert(kind);
        self
    }

    /// Binds devices whose best-matching driver has this id.
    pub fn with_driver(mut self, driver_id: impl Into<String>) -> Self {
        self.driver_ids.insert(driver_id.into());
        self
    }

    pub fn is_empty(&self) -> bool {
        !self.all
            && self.interfaces.is_empty()
            && self.kinds.is_empty()
            && self.driver_ids.is_empty()
    }

    pub fn binds_all(&self) -> bool {
        self.all
    }

    pub fn interfaces(&self) -> &BTreeSet<InterfaceKind> {
        &self.interfaces
    }

    pub fn kinds(&self) -> &BTreeSet<DeviceKind> {
        &self.kinds
    }

    pub fn driver_ids(&self) -> &BTreeSet<String> {
        &self.driver_ids
    }

    /// Whether `device`, resolved to `driver_id`, should be bound on refresh.
    pub fn matches(&self, device: &DeviceDescriptor, driver_id: &str) -> bool {
        self.all
            || self.interfaces.contains(&device.interface)
            || self.kinds.contains(&device.kind)
            || self.driver_ids.contains(driver_id)
    }
}
