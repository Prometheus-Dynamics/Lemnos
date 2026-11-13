//! The service's view of the devices, and what has changed since Orion last heard.
//!
//! The source thread writes it; the publisher takes what is due. Nothing here knows what a
//! device is, or what Orion is: a device is its entry, its value items and its controls, and
//! a change is a state or a value.
//!
//! Changes are coalesced: a value that moves twice before a publish goes out once, with its
//! latest value, and a value equal to the one held is not a change. There is no rate.

use lemnos_client::{DeviceHealth, DeviceState};

use crate::values::{Plain, Reading};

/// What a device carries, as the source reads it from lemnosd.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceSpec {
    /// lemnosd's index for the device, which its events name.
    pub index: u32,
    pub name: String,
    pub driver: String,
    pub tags: Vec<String>,
    pub status: Status,
    /// The value items published for it, in order.
    pub items: Vec<ItemSpec>,
    /// Its control and action items, which Orion's `set` action drives.
    pub controls: Vec<ControlSpec>,
}

/// A value item, as published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemSpec {
    pub name: String,
    pub unit: String,
    pub plain: Plain,
}

/// A control or action item: its name, and the shapes of the values it takes as a person
/// writes them (`u32`, `f32 f32`), empty for none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlSpec {
    pub name: String,
    pub takes: String,
}

/// How a device stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub state: DeviceState,
    pub health: DeviceHealth,
    pub reason: String,
}

/// What is due to go to Orion. Kept and reused by the publisher, so a steady flow of values
/// allocates nothing once its lists have grown.
#[derive(Debug, Default, PartialEq)]
pub struct Changes {
    /// The device list these changes belong to. A publisher holding another list must
    /// describe the devices again first.
    pub generation: u64,
    /// Device positions whose status moved, with the status now.
    pub status: Vec<(usize, Status)>,
    /// `(device, item, value)`, by positions in the list.
    pub readings: Vec<(usize, usize, Reading)>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.status.is_empty() && self.readings.is_empty()
    }

    fn clear(&mut self) {
        self.status.clear();
        self.readings.clear();
    }
}

#[derive(Debug)]
struct Item {
    value: Option<Reading>,
    dirty: bool,
}

#[derive(Debug)]
struct Device {
    spec: DeviceSpec,
    items: Vec<Item>,
    status_dirty: bool,
}

#[derive(Debug, Default)]
pub struct Mirror {
    generation: u64,
    devices: Vec<Device>,
}

impl Mirror {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the device list. It is a new list, so everything about it is due.
    pub fn set_devices(&mut self, specs: Vec<DeviceSpec>) {
        self.generation += 1;
        self.devices = specs
            .into_iter()
            .map(|spec| Device {
                items: spec
                    .items
                    .iter()
                    .map(|_| Item {
                        value: None,
                        dirty: false,
                    })
                    .collect(),
                spec,
                status_dirty: true,
            })
            .collect();
    }

    /// The device list now, and its generation.
    pub fn devices(&self) -> (u64, Vec<DeviceSpec>) {
        (
            self.generation,
            self.devices.iter().map(|d| d.spec.clone()).collect(),
        )
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// A device's state, by lemnosd's index. An unknown index is ignored, and so is a state
    /// that is the one held.
    pub fn set_status(
        &mut self,
        index: u32,
        state: DeviceState,
        health: DeviceHealth,
        reason: &str,
    ) {
        let Some(device) = self.devices.iter_mut().find(|d| d.spec.index == index) else {
            return;
        };
        let status = &mut device.spec.status;
        if status.state == state && status.health == health && status.reason == reason {
            return;
        }
        status.state = state;
        status.health = health;
        reason.clone_into(&mut status.reason);
        device.status_dirty = true;
    }

    /// A value read for item `item` of the device at `device` (positions in the list the
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

    /// Everything the mirror holds is due again: a new Orion session has none of it.
    pub fn mark_all(&mut self) {
        for device in &mut self.devices {
            device.status_dirty = true;
            for item in &mut device.items {
                item.dirty = item.value.is_some();
            }
        }
    }

    /// Takes what is due into `into`, which is cleared first, and clears it here.
    pub fn take(&mut self, into: &mut Changes) {
        into.clear();
        into.generation = self.generation;
        for (position, device) in self.devices.iter_mut().enumerate() {
            if std::mem::take(&mut device.status_dirty) {
                into.status.push((position, device.spec.status.clone()));
            }
            for (index, item) in device.items.iter_mut().enumerate() {
                if std::mem::take(&mut item.dirty)
                    && let Some(value) = item.value
                {
                    into.readings.push((position, index, value));
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(index: u32, name: &str, items: &[&str]) -> DeviceSpec {
        DeviceSpec {
            index,
            name: name.to_owned(),
            driver: "fake-counter".to_owned(),
            tags: vec!["imu".to_owned()],
            status: Status {
                state: DeviceState::Running,
                health: DeviceHealth::Ok,
                reason: String::new(),
            },
            items: items
                .iter()
                .map(|name| ItemSpec {
                    name: (*name).to_owned(),
                    unit: String::new(),
                    plain: Plain::Number,
                })
                .collect(),
            controls: Vec::new(),
        }
    }

    fn take(mirror: &mut Mirror) -> Changes {
        let mut changes = Changes::default();
        mirror.take(&mut changes);
        changes
    }

    fn running() -> Status {
        Status {
            state: DeviceState::Running,
            health: DeviceHealth::Ok,
            reason: String::new(),
        }
    }

    #[test]
    fn a_new_list_is_a_new_generation_with_every_status_due() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "a", &[]), spec(1, "b", &[])]);
        let first = take(&mut mirror);
        assert_eq!(first.generation, 1);
        assert_eq!(first.status, [(0, running()), (1, running())]);
        assert!(
            take(&mut mirror).is_empty(),
            "nothing changed, nothing goes out"
        );
        mirror.set_devices(Vec::new());
        assert_eq!(take(&mut mirror).generation, 2);
        assert_eq!(mirror.devices(), (2, Vec::new()));
    }

    #[test]
    fn a_value_goes_out_when_it_changes_and_not_when_it_repeats() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "thing", &["count"])]);
        let _ = take(&mut mirror);
        mirror.set_reading(0, 0, Reading::Number(1.5));
        assert_eq!(take(&mut mirror).readings, [(0, 0, Reading::Number(1.5))]);
        mirror.set_reading(0, 0, Reading::Number(1.5));
        assert!(take(&mut mirror).is_empty());
    }

    #[test]
    fn two_values_before_a_publish_go_out_as_the_latest_once() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "thing", &["ok"])]);
        let _ = take(&mut mirror);
        mirror.set_reading(0, 0, Reading::Flag(true));
        mirror.set_reading(0, 0, Reading::Flag(false));
        assert_eq!(take(&mut mirror).readings, [(0, 0, Reading::Flag(false))]);
    }

    #[test]
    fn a_status_goes_out_when_any_part_of_it_moves() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(7, "thing", &[])]);
        let _ = take(&mut mirror);
        mirror.set_status(7, DeviceState::Running, DeviceHealth::Ok, "");
        assert!(
            take(&mut mirror).is_empty(),
            "the same status is not a change"
        );
        mirror.set_status(
            7,
            DeviceState::BackingOff,
            DeviceHealth::Failed,
            "read: timeout",
        );
        let moved = Status {
            state: DeviceState::BackingOff,
            health: DeviceHealth::Failed,
            reason: "read: timeout".to_owned(),
        };
        assert_eq!(take(&mut mirror).status, [(0, moved)]);
        mirror.set_status(
            7,
            DeviceState::BackingOff,
            DeviceHealth::Failed,
            "read: again",
        );
        assert_eq!(take(&mut mirror).status[0].1.reason, "read: again");
    }

    #[test]
    fn an_unknown_device_or_item_is_ignored() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "thing", &["count"])]);
        let _ = take(&mut mirror);
        mirror.set_status(99, DeviceState::Off, DeviceHealth::Ok, "");
        mirror.set_reading(5, 0, Reading::Number(1.0));
        mirror.set_reading(0, 5, Reading::Number(1.0));
        assert!(take(&mut mirror).is_empty());
    }

    #[test]
    fn mark_all_sends_everything_held_again() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "thing", &["count", "unread"])]);
        let _ = take(&mut mirror);
        mirror.set_reading(0, 0, Reading::Number(2.0));
        let _ = take(&mut mirror);
        mirror.mark_all();
        let changes = take(&mut mirror);
        assert_eq!(changes.status, [(0, running())]);
        assert_eq!(
            changes.readings,
            [(0, 0, Reading::Number(2.0))],
            "an item never read has nothing to send"
        );
    }
}
