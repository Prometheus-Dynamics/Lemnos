use super::{DeviceAddress, DeviceControlSurface, DeviceKind, DeviceLink, MatchHints};
use crate::{
    CapabilityDescriptor, CoreError, CoreResult, DeviceHealth, DeviceId, InterfaceKind,
    LocalDeviceId, Value, ValueMap,
};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceDescriptor {
    pub id: DeviceId,
    pub local_id: Option<LocalDeviceId>,
    pub interface: InterfaceKind,
    pub kind: DeviceKind,
    pub display_name: Option<String>,
    pub summary: Option<String>,
    /// Operational identity for transports and matching code.
    ///
    /// When present, this is the canonical source of truth for opening or
    /// selecting the underlying device. Implementations should not require
    /// duplicate entries in `properties` for the same addressing data.
    pub address: Option<DeviceAddress>,
    pub labels: BTreeMap<String, String>,
    /// Descriptive metadata that augments the descriptor.
    ///
    /// Properties are intended for discovery notes, UI, diagnostics, and
    /// secondary matching hints rather than mandatory transport identity.
    pub properties: ValueMap,
    /// Optional typed control-plane surface used by host drivers.
    ///
    /// This should be preferred over required descriptor properties when a
    /// driver needs a structured control root or host-facing access path.
    pub control_surface: Option<DeviceControlSurface>,
    pub capabilities: Vec<CapabilityDescriptor>,
    pub links: Vec<DeviceLink>,
    pub health: DeviceHealth,
    pub match_hints: MatchHints,
}

impl DeviceDescriptor {
    pub fn new(id: impl Into<String>, interface: InterfaceKind) -> CoreResult<Self> {
        Ok(Self {
            id: DeviceId::new(id)?,
            local_id: None,
            interface,
            kind: DeviceKind::Unspecified(interface),
            display_name: None,
            summary: None,
            address: None,
            labels: BTreeMap::new(),
            properties: ValueMap::new(),
            control_surface: None,
            capabilities: Vec::new(),
            links: Vec::new(),
            health: DeviceHealth::Healthy,
            match_hints: MatchHints::default(),
        })
    }

    pub fn for_kind(id: impl Into<String>, kind: DeviceKind) -> CoreResult<Self> {
        let interface = kind.interface();
        let mut descriptor = Self::new(id, interface)?;
        descriptor.kind = kind;
        Ok(descriptor)
    }

    pub fn builder(
        id: impl Into<String>,
        interface: InterfaceKind,
    ) -> CoreResult<DeviceDescriptorBuilder> {
        Ok(DeviceDescriptorBuilder {
            descriptor: Self::new(id, interface)?,
        })
    }

    pub fn builder_for_kind(
        id: impl Into<String>,
        kind: DeviceKind,
    ) -> CoreResult<DeviceDescriptorBuilder> {
        Ok(DeviceDescriptorBuilder {
            descriptor: Self::for_kind(id, kind)?,
        })
    }

    pub fn validate(&self) -> CoreResult<()> {
        if self.kind.interface() != self.interface {
            return Err(CoreError::KindInterfaceMismatch {
                kind: self.kind,
                interface: self.interface,
            });
        }

        if let Some(address) = &self.address
            && address.interface() != self.interface
        {
            return Err(CoreError::AddressInterfaceMismatch {
                address: address.clone(),
                interface: self.interface,
            });
        }

        Ok(())
    }

    pub fn add_label(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.labels.insert(key.into(), value.into());
    }

    pub fn set_property(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.properties.insert(key.into(), value.into());
    }

    pub fn add_capability(&mut self, capability: CapabilityDescriptor) {
        self.capabilities.push(capability);
    }

    pub fn set_control_surface(&mut self, control_surface: DeviceControlSurface) {
        self.control_surface = Some(control_surface);
    }

    pub fn add_link(&mut self, link: DeviceLink) {
        self.links.push(link);
    }
}

pub struct DeviceDescriptorBuilder {
    descriptor: DeviceDescriptor,
}

impl DeviceDescriptorBuilder {
    pub fn local_id(mut self, value: impl Into<String>) -> CoreResult<Self> {
        self.descriptor.local_id = Some(LocalDeviceId::new(value)?);
        Ok(self)
    }

    pub fn kind(mut self, kind: DeviceKind) -> Self {
        self.descriptor.interface = kind.interface();
        self.descriptor.kind = kind;
        self
    }

    pub fn display_name(mut self, value: impl Into<String>) -> Self {
        self.descriptor.display_name = Some(value.into());
        self
    }

    pub fn summary(mut self, value: impl Into<String>) -> Self {
        self.descriptor.summary = Some(value.into());
        self
    }

    pub fn address(mut self, value: DeviceAddress) -> Self {
        self.descriptor.address = Some(value);
        self
    }

    pub fn health(mut self, value: DeviceHealth) -> Self {
        self.descriptor.health = value;
        self
    }

    pub fn driver_hint(mut self, value: impl Into<String>) -> Self {
        self.descriptor.match_hints.driver_hint = Some(value.into());
        self
    }

    pub fn modalias(mut self, value: impl Into<String>) -> Self {
        self.descriptor.match_hints.modalias = Some(value.into());
        self
    }

    pub fn compatible(mut self, value: impl Into<String>) -> Self {
        self.descriptor.match_hints.compatible.push(value.into());
        self
    }

    pub fn vendor(mut self, value: impl Into<String>) -> Self {
        self.descriptor.match_hints.vendor = Some(value.into());
        self
    }

    pub fn model(mut self, value: impl Into<String>) -> Self {
        self.descriptor.match_hints.model = Some(value.into());
        self
    }

    pub fn revision(mut self, value: impl Into<String>) -> Self {
        self.descriptor.match_hints.revision = Some(value.into());
        self
    }

    pub fn serial_number(mut self, value: impl Into<String>) -> Self {
        self.descriptor.match_hints.serial_number = Some(value.into());
        self
    }

    pub fn hardware_id(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.descriptor
            .match_hints
            .hardware_ids
            .insert(key.into(), value.into());
        self
    }

    pub fn label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.descriptor.add_label(key, value);
        self
    }

    pub fn property(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.descriptor.set_property(key, value);
        self
    }

    pub fn control_surface(mut self, value: DeviceControlSurface) -> Self {
        self.descriptor.set_control_surface(value);
        self
    }

    pub fn capability(mut self, value: CapabilityDescriptor) -> Self {
        self.descriptor.add_capability(value);
        self
    }

    pub fn link(mut self, value: DeviceLink) -> Self {
        self.descriptor.add_link(value);
        self
    }

    pub fn build(self) -> CoreResult<DeviceDescriptor> {
        self.descriptor.validate()?;
        Ok(self.descriptor)
    }
}
