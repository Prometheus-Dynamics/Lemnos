use crate::{DiscoveryError, DiscoveryResult};
use lemnos_core::{
    DeviceDescriptor, DeviceId, DeviceKind, InterfaceKind, InventoryEvent, SmallMap, TimestampMs,
};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InventorySnapshot {
    pub observed_at: Option<TimestampMs>,
    pub devices: Vec<DeviceDescriptor>,
}

/// Devices keyed by ID. Built by insertion: collecting sorts a `Vec` first,
/// which links a whole sort implementation per element type.
fn index_by_id(devices: &[DeviceDescriptor]) -> SmallMap<&DeviceId, &DeviceDescriptor> {
    let mut map = SmallMap::new();
    for device in devices {
        map.insert(&device.id, device);
    }
    map
}

impl InventorySnapshot {
    pub fn new(devices: Vec<DeviceDescriptor>) -> DiscoveryResult<Self> {
        Self::with_observed_at(devices, None)
    }

    pub fn with_observed_at(
        devices: Vec<DeviceDescriptor>,
        observed_at: Option<TimestampMs>,
    ) -> DiscoveryResult<Self> {
        validate_devices(&devices)?;
        Ok(Self {
            observed_at,
            devices,
        })
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn count_for(&self, interface: InterfaceKind) -> usize {
        self.devices
            .iter()
            .filter(|device| device.interface == interface)
            .count()
    }

    pub fn count_kind(&self, kind: DeviceKind) -> usize {
        self.devices
            .iter()
            .filter(|device| device.kind == kind)
            .count()
    }

    pub fn contains(&self, device_id: &DeviceId) -> bool {
        self.get(device_id).is_some()
    }

    pub fn get(&self, device_id: &DeviceId) -> Option<&DeviceDescriptor> {
        self.devices.iter().find(|device| &device.id == device_id)
    }

    pub fn by_interface(&self, interface: InterfaceKind) -> Vec<&DeviceDescriptor> {
        self.devices
            .iter()
            .filter(|device| device.interface == interface)
            .collect()
    }

    pub fn by_kind(&self, kind: DeviceKind) -> Vec<&DeviceDescriptor> {
        self.devices
            .iter()
            .filter(|device| device.kind == kind)
            .collect()
    }

    pub fn first_by_kind(&self, kind: DeviceKind) -> Option<&DeviceDescriptor> {
        self.devices.iter().find(|device| device.kind == kind)
    }

    pub fn first_id_by_kind(&self, kind: DeviceKind) -> Option<DeviceId> {
        self.first_by_kind(kind).map(|device| device.id.clone())
    }

    pub fn ids(&self) -> Vec<&DeviceId> {
        self.devices.iter().map(|device| &device.id).collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &DeviceDescriptor> {
        self.devices.iter()
    }

    /// Snapshot lookup helpers intentionally use linear scans for `0.1`.
    ///
    /// Expected inventories are control-plane sized, and keeping one canonical
    /// `Vec<DeviceDescriptor>` avoids maintaining a hidden secondary index that
    /// would add complexity and extra sync work to every snapshot mutation.
    pub fn diff(&self, next: &InventorySnapshot) -> InventoryDiff {
        let current = index_by_id(&self.devices);
        let updated = index_by_id(&next.devices);

        let mut added = Vec::new();
        let mut changed = Vec::new();
        let mut removed = Vec::new();

        for (device_id, device) in &updated {
            match current.get(device_id) {
                None => added.push(Arc::new((*device).clone())),
                Some(previous) if *previous != *device => changed.push(ChangedDevice {
                    previous: Arc::new((*previous).clone()),
                    current: Arc::new((*device).clone()),
                }),
                Some(_) => {}
            }
        }

        for device_id in current.keys() {
            if !updated.contains_key(device_id) {
                removed.push((*device_id).clone());
            }
        }

        InventoryDiff {
            added,
            changed,
            removed,
        }
    }
}

/// A device whose descriptor changed. Descriptors are shared (`Arc`) with
/// the inventory events built from the diff, so each is copied once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedDevice {
    pub previous: Arc<DeviceDescriptor>,
    pub current: Arc<DeviceDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InventoryDiff {
    pub added: Vec<Arc<DeviceDescriptor>>,
    pub changed: Vec<ChangedDevice>,
    pub removed: Vec<DeviceId>,
}

impl InventoryDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }

    pub fn events(&self) -> Vec<InventoryEvent> {
        let mut events =
            Vec::with_capacity(self.added.len() + self.changed.len() + self.removed.len());
        events.extend(self.added.iter().cloned().map(InventoryEvent::Added));
        events.extend(
            self.changed
                .iter()
                .cloned()
                .map(|change| InventoryEvent::Changed {
                    previous: change.previous,
                    current: change.current,
                }),
        );
        events.extend(self.removed.iter().cloned().map(InventoryEvent::Removed));
        events
    }
}

pub(crate) fn validate_devices(devices: &[DeviceDescriptor]) -> DiscoveryResult<()> {
    let mut seen = SmallMap::new();
    for device in devices {
        device
            .validate()
            .map_err(|source| DiscoveryError::InvalidDescriptor {
                probe: "snapshot".to_string(),
                device_id: device.id.clone(),
                source,
            })?;
        if seen.insert(device.id.clone(), ()).is_some() {
            return Err(DiscoveryError::DuplicateDeviceId {
                device_id: device.id.clone(),
            });
        }
    }
    Ok(())
}
