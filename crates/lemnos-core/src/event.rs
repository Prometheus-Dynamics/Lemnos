use crate::{
    DeviceDescriptor, DeviceHealth, DeviceId, DeviceIssue, DeviceLifecycleState,
    DeviceStateSnapshot, TimestampMs,
};
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InventoryEvent {
    /// Descriptors are shared (`Arc`), so a diff, the retained event log and
    /// the subscribers hold one copy of each.
    Added(Arc<DeviceDescriptor>),
    Changed {
        previous: Arc<DeviceDescriptor>,
        current: Arc<DeviceDescriptor>,
    },
    Removed(DeviceId),
}

pub type DeviceEvent = InventoryEvent;

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateEvent {
    Snapshot(Arc<DeviceStateSnapshot>),
    HealthChanged {
        device_id: DeviceId,
        previous: DeviceHealth,
        current: DeviceHealth,
        observed_at: Option<TimestampMs>,
    },
    LifecycleChanged {
        device_id: DeviceId,
        previous: DeviceLifecycleState,
        current: DeviceLifecycleState,
        observed_at: Option<TimestampMs>,
    },
    IssuesChanged {
        device_id: DeviceId,
        issues: Vec<DeviceIssue>,
        observed_at: Option<TimestampMs>,
    },
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LemnosEvent {
    Inventory(Box<InventoryEvent>),
    State(Box<StateEvent>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeviceDescriptor, DeviceId, InterfaceKind};

    #[test]
    fn inventory_events_preserve_descriptor_data() {
        let descriptor = DeviceDescriptor::new("gpio0", InterfaceKind::Gpio).expect("descriptor");
        let event = InventoryEvent::Added(Arc::new(descriptor.clone()));
        assert_eq!(event, InventoryEvent::Added(Arc::new(descriptor)));
    }

    #[test]
    fn state_event_wraps_health_changes() {
        let event = StateEvent::HealthChanged {
            device_id: DeviceId::new("gpio0").expect("device id"),
            previous: DeviceHealth::Healthy,
            current: DeviceHealth::Degraded,
            observed_at: Some(TimestampMs::new(5)),
        };

        match event {
            StateEvent::HealthChanged { current, .. } => {
                assert_eq!(current, DeviceHealth::Degraded);
            }
            _ => panic!("unexpected event variant"),
        }
    }
}
