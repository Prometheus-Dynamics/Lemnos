//! The service's view of the devices, and what has changed since Orion last heard.
//!
//! Every device becomes one Orion resource: its id is `lemnos.<device>`, its type `lemnos.device`,
//! its availability and health come from its state, and its labels come from its driver, tags
//! and item units. Its status entries are `status`, `health` and `reason`, then one entry per
//! value item. Nothing here knows what a device is.
//!
//! Changes are coalesced: a value that moves twice before a publish goes out once, with its
//! latest value, and a value equal to the one held is not a change. There is no rate.

use std::time::Duration;

use lemnos_client::{DeviceHealth, DeviceState};
use orion_control_plane::{
    AvailabilityState, HealthState, ResourceRecord, StatusEntry, StatusSubject, TypedConfigValue,
};
use orion_core::{ProviderId, ResourceId, ResourceType};

use crate::values::Reading;
use crate::{PROVIDER_ID, RESOURCE_TYPE};

/// Orion keeps a status entry this long unless it is published again. The node's ceiling is
/// 300 s; unchanged values are published again well inside it.
pub const REFRESH: Duration = Duration::from_secs(60);

/// What a device carries, as the source reads it from lemnosd.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceSpec {
    /// lemnosd's index for the device, which its events name.
    pub index: u32,
    pub name: String,
    pub driver: String,
    pub tags: Vec<String>,
    pub state: DeviceState,
    pub health: DeviceHealth,
    pub reason: String,
    /// The value items published for it, in order.
    pub items: Vec<ItemSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemSpec {
    pub name: String,
    pub unit: String,
}

/// What is to go to Orion. `records` is the whole device list when it changed.
#[derive(Debug, Default, PartialEq)]
pub struct Changes {
    pub records: Option<Vec<ResourceRecord>>,
    pub status: Vec<StatusEntry>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.records.is_none() && self.status.is_empty()
    }
}

#[derive(Debug)]
struct Item {
    name: String,
    unit: String,
    value: Option<Reading>,
    dirty: bool,
}

#[derive(Debug)]
struct Device {
    index: u32,
    driver: String,
    tags: Vec<String>,
    state: DeviceState,
    health: DeviceHealth,
    reason: String,
    resource: ResourceId,
    items: Vec<Item>,
    status_dirty: bool,
}

#[derive(Debug, Default)]
pub struct Mirror {
    devices: Vec<Device>,
    records_dirty: bool,
}

impl Mirror {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the device list. Everything is due to go out again.
    pub fn set_devices(&mut self, specs: Vec<DeviceSpec>) {
        self.devices = specs
            .into_iter()
            .map(|spec| Device {
                index: spec.index,
                resource: ResourceId::new(format!("lemnos.{}", spec.name)),
                driver: spec.driver,
                tags: spec.tags,
                state: spec.state,
                health: spec.health,
                reason: spec.reason,
                items: spec
                    .items
                    .into_iter()
                    .map(|item| Item {
                        name: item.name,
                        unit: item.unit,
                        value: None,
                        dirty: false,
                    })
                    .collect(),
                status_dirty: true,
            })
            .collect();
        self.records_dirty = true;
    }

    /// A device's state, by lemnosd's index. An unknown index is ignored.
    pub fn set_state(
        &mut self,
        index: u32,
        state: DeviceState,
        health: DeviceHealth,
        reason: &str,
    ) {
        let Some(position) = self.devices.iter().position(|d| d.index == index) else {
            return;
        };
        let device = &mut self.devices[position];
        let mut moved = false;
        if device.state != state || device.health != health {
            device.state = state;
            device.health = health;
            self.records_dirty = true;
            moved = true;
        }
        if device.reason != reason {
            device.reason = reason.to_owned();
            moved = true;
        }
        if moved {
            device.status_dirty = true;
        }
    }

    /// A value read for item `item` of the device at `device` (a position in the list the
    /// last [`Mirror::set_devices`] gave). A value equal to the held one is not a change.
    pub fn set_reading(&mut self, device: usize, item: usize, reading: Reading) {
        let Some(item) = self
            .devices
            .get_mut(device)
            .and_then(|d| d.items.get_mut(item))
        else {
            return;
        };
        if item.value != Some(reading) {
            item.value = Some(reading);
            item.dirty = true;
        }
    }

    /// Everything the mirror holds is due again. Used on connect, and on the refresh tick.
    pub fn mark_all(&mut self) {
        self.records_dirty = true;
        for device in &mut self.devices {
            device.status_dirty = true;
            for item in &mut device.items {
                item.dirty = item.value.is_some();
            }
        }
    }

    /// Takes what is due, and clears it.
    pub fn take(&mut self) -> Changes {
        let mut changes = Changes::default();
        if std::mem::take(&mut self.records_dirty) {
            changes.records = Some(self.devices.iter().map(Device::record).collect());
        }
        for device in &mut self.devices {
            device.emit(&mut changes.status);
        }
        changes
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }
}

impl Device {
    fn record(&self) -> ResourceRecord {
        let availability = match self.state {
            DeviceState::Running => AvailabilityState::Available,
            DeviceState::BackingOff | DeviceState::Off => AvailabilityState::Unavailable,
        };
        let mut builder = ResourceRecord::builder(
            self.resource.clone(),
            ResourceType::new(RESOURCE_TYPE),
            ProviderId::new(PROVIDER_ID),
        )
        .availability(availability)
        .health(health_state(self.health))
        .label(format!("lemnos.driver={}", self.driver));
        for tag in &self.tags {
            builder = builder.label(format!("lemnos.tag={tag}"));
        }
        for item in &self.items {
            if !item.unit.is_empty() {
                builder = builder.label(format!("lemnos.unit.{}={}", item.name, item.unit));
            }
        }
        builder.build()
    }

    fn entry(&self, key: &str, value: TypedConfigValue) -> StatusEntry {
        StatusEntry::new(StatusSubject::Resource(self.resource.clone()), key, value)
    }

    fn emit(&mut self, out: &mut Vec<StatusEntry>) {
        if std::mem::take(&mut self.status_dirty) {
            out.push(self.entry(
                "status",
                TypedConfigValue::String(state_name(self.state).to_owned()),
            ));
            out.push(self.entry(
                "health",
                TypedConfigValue::String(health_name(self.health).to_owned()),
            ));
            out.push(self.entry("reason", TypedConfigValue::String(self.reason.clone())));
        }
        for index in 0..self.items.len() {
            if !std::mem::take(&mut self.items[index].dirty) {
                continue;
            }
            let item = &self.items[index];
            let Some(reading) = item.value else {
                continue;
            };
            let value = match reading {
                Reading::Number(number) => TypedConfigValue::F64(number),
                Reading::Flag(flag) => TypedConfigValue::Bool(flag),
            };
            out.push(self.entry(&item.name, value));
        }
    }
}

/// The state as `lemnos-ctl` names it.
pub const fn state_name(state: DeviceState) -> &'static str {
    match state {
        DeviceState::Running => "running",
        DeviceState::BackingOff => "retrying",
        DeviceState::Off => "off",
    }
}

/// The health as `lemnos-ctl` names it.
pub const fn health_name(health: DeviceHealth) -> &'static str {
    match health {
        DeviceHealth::Ok => "ok",
        DeviceHealth::Degraded => "degraded",
        DeviceHealth::Failed => "failed",
    }
}

const fn health_state(health: DeviceHealth) -> HealthState {
    match health {
        DeviceHealth::Ok => HealthState::Healthy,
        DeviceHealth::Degraded => HealthState::Degraded,
        DeviceHealth::Failed => HealthState::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(index: u32, name: &str, items: &[(&str, &str)]) -> DeviceSpec {
        DeviceSpec {
            index,
            name: name.to_owned(),
            driver: "fake-counter".to_owned(),
            tags: vec!["imu".to_owned()],
            state: DeviceState::Running,
            health: DeviceHealth::Ok,
            reason: String::new(),
            items: items
                .iter()
                .map(|(name, unit)| ItemSpec {
                    name: (*name).to_owned(),
                    unit: (*unit).to_owned(),
                })
                .collect(),
        }
    }

    fn entries(changes: &Changes) -> Vec<(String, TypedConfigValue)> {
        changes
            .status
            .iter()
            .map(|entry| (entry.key.clone(), entry.value.clone()))
            .collect()
    }

    fn text(value: &str) -> TypedConfigValue {
        TypedConfigValue::String(value.to_owned())
    }

    #[test]
    fn a_device_is_one_resource_with_its_labels_and_state() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "thing", &[("acceleration.x", "m/s²")])]);
        let records = mirror.take().records.expect("a new device list goes out");
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.resource_id.as_str(), "lemnos.thing");
        assert_eq!(record.resource_type.as_str(), RESOURCE_TYPE);
        assert_eq!(record.provider_id.as_str(), PROVIDER_ID);
        assert_eq!(record.availability, AvailabilityState::Available);
        assert_eq!(record.health, HealthState::Healthy);
        assert!(
            record
                .labels
                .contains(&"lemnos.driver=fake-counter".to_owned())
        );
        assert!(record.labels.contains(&"lemnos.tag=imu".to_owned()));
        assert!(
            record
                .labels
                .contains(&"lemnos.unit.acceleration.x=m/s²".to_owned())
        );
    }

    #[test]
    fn the_device_state_and_its_status_are_published_once() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "thing", &[])]);
        let first = mirror.take();
        assert_eq!(
            entries(&first),
            [
                ("status".to_owned(), text("running")),
                ("health".to_owned(), text("ok")),
                ("reason".to_owned(), text("")),
            ]
        );
        assert!(
            mirror.take().is_empty(),
            "nothing changed, nothing goes out"
        );
    }

    #[test]
    fn a_value_goes_out_when_it_changes_and_not_when_it_repeats() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "thing", &[("count", "")])]);
        let _ = mirror.take();
        mirror.set_reading(0, 0, Reading::Number(1.5));
        let changes = mirror.take();
        assert!(changes.records.is_none());
        assert_eq!(
            entries(&changes),
            [("count".to_owned(), TypedConfigValue::F64(1.5))]
        );
        mirror.set_reading(0, 0, Reading::Number(1.5));
        assert!(mirror.take().is_empty());
    }

    #[test]
    fn two_values_before_a_publish_go_out_as_the_latest_once() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "thing", &[("ok", "")])]);
        let _ = mirror.take();
        mirror.set_reading(0, 0, Reading::Flag(true));
        mirror.set_reading(0, 0, Reading::Flag(false));
        assert_eq!(
            entries(&mirror.take()),
            [("ok".to_owned(), TypedConfigValue::Bool(false))]
        );
    }

    #[test]
    fn a_change_of_state_moves_the_record_and_a_change_of_reason_only_the_status() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(7, "thing", &[])]);
        let _ = mirror.take();
        mirror.set_state(
            7,
            DeviceState::BackingOff,
            DeviceHealth::Failed,
            "read: timeout",
        );
        let changes = mirror.take();
        let records = changes
            .records
            .as_ref()
            .expect("the state moved the record");
        assert_eq!(records[0].availability, AvailabilityState::Unavailable);
        assert_eq!(records[0].health, HealthState::Failed);
        assert!(entries(&changes).contains(&("status".to_owned(), text("retrying"))));
        assert!(entries(&changes).contains(&("reason".to_owned(), text("read: timeout"))));

        mirror.set_state(
            7,
            DeviceState::BackingOff,
            DeviceHealth::Failed,
            "read: again",
        );
        let reason_only = mirror.take();
        assert!(reason_only.records.is_none());
        assert_eq!(
            entries(&reason_only),
            [
                ("status".to_owned(), text("retrying")),
                ("health".to_owned(), text("failed")),
                ("reason".to_owned(), text("read: again")),
            ]
        );
    }

    #[test]
    fn an_unknown_device_or_item_is_ignored() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "thing", &[("count", "")])]);
        let _ = mirror.take();
        mirror.set_state(99, DeviceState::Off, DeviceHealth::Ok, "");
        mirror.set_reading(5, 0, Reading::Number(1.0));
        mirror.set_reading(0, 5, Reading::Number(1.0));
        assert!(mirror.take().is_empty());
    }

    #[test]
    fn mark_all_sends_everything_held_again() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "thing", &[("count", ""), ("unread", "")])]);
        let _ = mirror.take();
        mirror.set_reading(0, 0, Reading::Number(2.0));
        let _ = mirror.take();
        mirror.mark_all();
        let changes = mirror.take();
        assert_eq!(changes.records.as_ref().map(Vec::len), Some(1));
        assert_eq!(
            entries(&changes),
            [
                ("status".to_owned(), text("running")),
                ("health".to_owned(), text("ok")),
                ("reason".to_owned(), text("")),
                ("count".to_owned(), TypedConfigValue::F64(2.0)),
            ],
            "an item never read has nothing to send"
        );
    }

    #[test]
    fn an_empty_device_list_takes_the_resources_away() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "thing", &[])]);
        let _ = mirror.take();
        mirror.set_devices(Vec::new());
        let changes = mirror.take();
        assert_eq!(changes.records, Some(Vec::new()));
        assert!(changes.status.is_empty());
    }
}
