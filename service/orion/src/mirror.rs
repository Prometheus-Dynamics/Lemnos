//! The service's view of the devices, and what has changed since Orion last heard.
//!
//! The source thread writes it; the publisher takes what is due. Nothing here knows what a
//! device is, or what Orion is: a device is its entry, its value items and its controls, and
//! a change is a device that came or went, a state, or a value.
//!
//! Each device has a slot: a number the mirror gives it when it first appears and never reuses.
//! A new device list is compared with the one held. A device that is still there, with the same
//! driver, tags, items and controls, keeps its slot, and only its state and values can change. A
//! device that is gone is removed; one that is new is added; one whose description changed is
//! removed and added again. Nothing else in the list moves, so the publisher can say each change
//! on its own.
//!
//! Changes are coalesced: a value that moves twice before a publish goes out once, with its
//! latest value and the stamp it was read at, and a value equal to the one held is not a change.
//! There is no rate.
//!
//! The board's identity facts (`hello.*`, see [`crate::hello`]) are held here too: the identity
//! device's facts as the source last read them, and `offers` from the devices that run. Their
//! document is rebuilt when either changes.

use lemnos_client::{DeviceHealth, DeviceState};

use crate::hello;
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

/// The devices with their slots, and the facts, as [`Mirror::snapshot`] gives them.
pub type Snapshot = (Vec<(u64, DeviceSpec)>, Vec<(String, String)>);

/// What is due to go to Orion. Kept and reused by the publisher, so a steady flow of values
/// allocates nothing once its lists have grown.
#[derive(Debug, Default, PartialEq)]
pub struct Changes {
    /// Slots of devices that left, in the order they left.
    pub removed: Vec<u64>,
    /// Devices that came, with their slot and what they carry.
    pub added: Vec<(u64, DeviceSpec)>,
    /// Slots whose status moved, with the status now.
    pub status: Vec<(u64, Status)>,
    /// `(slot, item, value, stamp)`: the item's index in the device's items, the value, and the
    /// time the driver read it (its `CLOCK_MONOTONIC` nanoseconds, passed on unchanged).
    pub readings: Vec<(u64, usize, Reading, u64)>,
    /// The board's facts now, when they changed: each `hello.<name>` and its value, in order.
    pub hello: Option<Vec<(String, String)>>,
    /// The device set's generation, when it changed (a device came or went).
    pub set: Option<u64>,
    /// `(slot, generation)` of each device whose description changed, its generation now.
    pub described: Vec<(u64, u64)>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty()
            && self.added.is_empty()
            && self.status.is_empty()
            && self.readings.is_empty()
            && self.hello.is_none()
            && self.set.is_none()
            && self.described.is_empty()
    }

    fn clear(&mut self) {
        self.removed.clear();
        self.added.clear();
        self.status.clear();
        self.readings.clear();
        self.hello = None;
        self.set = None;
        self.described.clear();
    }
}

#[derive(Debug)]
struct Item {
    value: Option<Reading>,
    stamp: u64,
    dirty: bool,
}

#[derive(Debug)]
struct Device {
    slot: u64,
    spec: DeviceSpec,
    items: Vec<Item>,
    status_dirty: bool,
    /// Counts each change of what the device's description says (its state and reason, which
    /// `describe` carries). Starts at 1 when the device comes.
    generation: u64,
    generation_dirty: bool,
}

#[derive(Debug, Default)]
pub struct Mirror {
    /// The next slot to give. Slots are never reused.
    next_slot: u64,
    devices: Vec<Device>,
    /// Slots that left since the last take, and slots that came: announced by the next take.
    removed: Vec<u64>,
    added: Vec<u64>,
    /// The identity device's facts, as last read. `None` when it could not be read.
    identity: Option<Vec<(String, String)>>,
    /// The `hello.*` facts now, as [`hello::document`] leaves them.
    hello: Vec<(String, String)>,
    hello_dirty: bool,
    /// Counts each change of the device set: a device that came or went.
    set_generation: u64,
    set_dirty: bool,
}

/// Whether a change of status changes what `describe` says: its state or its reason. Health is
/// not in the description, so a change of health alone is not one.
fn described_moved(held: &Status, new: &Status) -> bool {
    held.state != new.state || held.reason != new.reason
}

/// Whether two specs describe the same device, apart from its index and its status: what its
/// resource, topics and actions are made from.
fn same_shape(held: &DeviceSpec, new: &DeviceSpec) -> bool {
    held.name == new.name
        && held.driver == new.driver
        && held.tags == new.tags
        && held.items == new.items
        && held.controls == new.controls
}

impl Mirror {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the device list `specs`, with the identity device's facts as read for it. Devices
    /// that are still there keep their slots; the others are removed or added.
    pub fn set_devices(&mut self, specs: Vec<DeviceSpec>, identity: Option<Vec<(String, String)>>) {
        let mut held = std::mem::take(&mut self.devices);
        let mut kept = Vec::with_capacity(specs.len());
        for spec in specs {
            let same = held
                .iter()
                .position(|d| d.spec.name == spec.name && same_shape(&d.spec, &spec));
            match same {
                Some(at) => {
                    let mut device = held.swap_remove(at);
                    if device.spec.status != spec.status {
                        device.status_dirty = true;
                    }
                    if described_moved(&device.spec.status, &spec.status) {
                        device.generation += 1;
                        device.generation_dirty = true;
                    }
                    device.spec.index = spec.index;
                    device.spec.status = spec.status;
                    kept.push(device);
                }
                None => {
                    let slot = self.next_slot;
                    self.next_slot += 1;
                    self.added.push(slot);
                    kept.push(Device {
                        slot,
                        generation: 1,
                        generation_dirty: true,
                        items: spec
                            .items
                            .iter()
                            .map(|_| Item {
                                value: None,
                                stamp: 0,
                                dirty: false,
                            })
                            .collect(),
                        spec,
                        status_dirty: true,
                    });
                }
            }
        }
        self.removed.extend(held.iter().map(|d| d.slot));
        if !self.added.is_empty() || !self.removed.is_empty() {
            self.set_generation += 1;
            self.set_dirty = true;
        }
        self.devices = kept;
        self.identity = identity;
        self.rebuild_hello();
    }

    /// The identity device's facts as the source has just read them, or `None` when it could not
    /// be read: then none of its facts is published.
    pub fn set_identity(&mut self, identity: Option<Vec<(String, String)>>) {
        if self.identity != identity {
            self.identity = identity;
            self.rebuild_hello();
        }
    }

    /// The devices now, with their slots, and the facts to publish. The changes so far are the
    /// ones this list already holds, so they are dropped: a session published from this list
    /// starts from it.
    pub fn snapshot(&mut self) -> Snapshot {
        self.removed.clear();
        self.added.clear();
        // A session starts with the device set's generation (it is given to `Session::open`), and
        // with each device's generation published once: the next take carries them all.
        for device in &mut self.devices {
            device.generation_dirty = true;
        }
        (
            self.devices
                .iter()
                .map(|d| (d.slot, d.spec.clone()))
                .collect(),
            self.hello.clone(),
        )
    }

    /// Rebuilds the `hello.*` document from the identity device's facts and the running devices'
    /// contract ids. Marks it due when it has changed.
    fn rebuild_hello(&mut self) {
        let offers = hello::offers(
            self.devices
                .iter()
                .filter(|d| d.spec.status.state == DeviceState::Running)
                .flat_map(|d| d.spec.tags.iter().map(String::as_str)),
        );
        let document = hello::document(self.identity.as_deref(), &offers);
        if document != self.hello {
            self.hello = document;
            self.hello_dirty = true;
        }
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
        let moved = status.state != state;
        if described_moved(
            status,
            &Status {
                state,
                health,
                reason: reason.to_owned(),
            },
        ) {
            device.generation += 1;
            device.generation_dirty = true;
        }
        status.state = state;
        status.health = health;
        reason.clone_into(&mut status.reason);
        device.status_dirty = true;
        // Only a running device offers its contracts, so a change of state can change offers.
        if moved {
            self.rebuild_hello();
        }
    }

    /// A value read at `stamp` for item `item` of the device with lemnosd's index `index`. A
    /// value equal to the held one is not a change.
    pub fn set_reading(&mut self, index: u32, item: usize, reading: Reading, stamp: u64) {
        let Some(item) = self
            .devices
            .iter_mut()
            .find(|d| d.spec.index == index)
            .and_then(|d| d.items.get_mut(item))
        else {
            return;
        };
        if item.value != Some(reading) {
            item.value = Some(reading);
            item.stamp = stamp;
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
        into.removed.append(&mut self.removed);
        for slot in self.added.drain(..) {
            // A device that came and went before this take is not announced at all: its removal
            // is what the publisher is told, and it has nothing to undo.
            if let Some(device) = self.devices.iter().find(|d| d.slot == slot) {
                into.added.push((slot, device.spec.clone()));
            }
        }
        if std::mem::take(&mut self.hello_dirty) {
            into.hello = Some(self.hello.clone());
        }
        if std::mem::take(&mut self.set_dirty) {
            into.set = Some(self.set_generation);
        }
        for device in &mut self.devices {
            if std::mem::take(&mut device.generation_dirty) {
                into.described.push((device.slot, device.generation));
            }
            if std::mem::take(&mut device.status_dirty) {
                into.status.push((device.slot, device.spec.status.clone()));
            }
            for (index, item) in device.items.iter_mut().enumerate() {
                if std::mem::take(&mut item.dirty)
                    && let Some(value) = item.value
                {
                    into.readings.push((device.slot, index, value, item.stamp));
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    /// The device set's generation now: what a session starts from.
    pub fn set_generation(&self) -> u64 {
        self.set_generation
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
            status: running(),
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

    fn fact(name: &str, value: &str) -> (String, String) {
        (name.to_owned(), value.to_owned())
    }

    #[test]
    fn a_new_device_is_added_once_with_its_status_due() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "a", &[]), spec(1, "b", &[])], None);
        let first = take(&mut mirror);
        assert_eq!(
            first
                .added
                .iter()
                .map(|(slot, s)| (*slot, s.name.as_str()))
                .collect::<Vec<_>>(),
            [(0, "a"), (1, "b")]
        );
        assert_eq!(first.status, [(0, running()), (1, running())]);
        assert!(
            take(&mut mirror).is_empty(),
            "nothing changed, nothing goes out"
        );
    }

    #[test]
    fn a_device_that_stays_keeps_its_slot_and_nothing_else_moves() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "a", &[]), spec(1, "b", &[])], None);
        let _ = take(&mut mirror);
        // The list comes again with `a` gone: only `a` is removed, `b` keeps slot 1 and is not
        // described again, though its lemnosd index changed.
        mirror.set_devices(vec![spec(5, "b", &[])], None);
        let changes = take(&mut mirror);
        assert_eq!(changes.removed, [0]);
        assert!(changes.added.is_empty());
        assert!(changes.status.is_empty(), "b's status did not move");
        // A new device after that gets a slot never given before.
        mirror.set_devices(vec![spec(5, "b", &[]), spec(6, "c", &[])], None);
        let changes = take(&mut mirror);
        assert_eq!(changes.added.len(), 1);
        assert_eq!(changes.added[0].0, 2);
        assert!(changes.removed.is_empty());
    }

    #[test]
    fn a_device_whose_description_changed_is_removed_and_added() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "a", &["count"])], None);
        let _ = take(&mut mirror);
        mirror.set_devices(vec![spec(0, "a", &["count", "more"])], None);
        let changes = take(&mut mirror);
        assert_eq!(changes.removed, [0]);
        assert_eq!(changes.added.len(), 1);
        assert_eq!(changes.added[0].0, 1);
    }

    #[test]
    fn a_device_added_and_removed_before_a_take_is_not_announced() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "a", &[])], None);
        mirror.set_devices(Vec::new(), None);
        let changes = take(&mut mirror);
        assert_eq!(changes.removed, [0]);
        assert!(changes.added.is_empty());
    }

    #[test]
    fn a_value_goes_out_when_it_changes_with_the_stamp_it_was_read_at() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "thing", &["count"])], None);
        let _ = take(&mut mirror);
        mirror.set_reading(0, 0, Reading::Number(1.5), 42);
        assert_eq!(
            take(&mut mirror).readings,
            [(0, 0, Reading::Number(1.5), 42)]
        );
        mirror.set_reading(0, 0, Reading::Number(1.5), 43);
        assert!(
            take(&mut mirror).is_empty(),
            "the same value is not a change"
        );
    }

    #[test]
    fn two_values_before_a_publish_go_out_as_the_latest_once() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "thing", &["ok"])], None);
        let _ = take(&mut mirror);
        mirror.set_reading(0, 0, Reading::Flag(true), 1);
        mirror.set_reading(0, 0, Reading::Flag(false), 2);
        assert_eq!(
            take(&mut mirror).readings,
            [(0, 0, Reading::Flag(false), 2)]
        );
    }

    #[test]
    fn a_status_goes_out_when_any_part_of_it_moves() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(7, "thing", &[])], None);
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
        mirror.set_devices(vec![spec(0, "thing", &["count"])], None);
        let _ = take(&mut mirror);
        mirror.set_status(99, DeviceState::Off, DeviceHealth::Ok, "");
        mirror.set_reading(5, 0, Reading::Number(1.0), 1);
        mirror.set_reading(0, 5, Reading::Number(1.0), 1);
        assert!(take(&mut mirror).is_empty());
    }

    #[test]
    fn mark_all_sends_everything_held_again() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "thing", &["count", "unread"])], None);
        let _ = take(&mut mirror);
        mirror.set_reading(0, 0, Reading::Number(2.0), 9);
        let _ = take(&mut mirror);
        mirror.mark_all();
        let changes = take(&mut mirror);
        assert_eq!(changes.status, [(0, running())]);
        assert_eq!(changes.readings, [(0, 0, Reading::Number(2.0), 9)]);
    }

    #[test]
    fn snapshot_is_the_list_held_and_drops_the_changes_it_already_has() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "a", &[])], None);
        let (devices, _) = mirror.snapshot();
        assert_eq!(devices.len(), 1);
        assert!(take(&mut mirror).added.is_empty());
    }

    #[test]
    fn the_device_set_generation_moves_once_per_change_of_the_set_and_not_when_steady() {
        let mut mirror = Mirror::new();
        assert_eq!(mirror.set_generation(), 0);
        mirror.set_devices(vec![spec(0, "a", &[]), spec(1, "b", &[])], None);
        assert_eq!(mirror.set_generation(), 1, "two devices came in one change");
        assert_eq!(take(&mut mirror).set, Some(1));
        // The same list again: nothing moved.
        mirror.set_devices(vec![spec(0, "a", &[]), spec(1, "b", &[])], None);
        assert_eq!(mirror.set_generation(), 1);
        assert!(take(&mut mirror).set.is_none());
        mirror.set_devices(vec![spec(0, "a", &[])], None);
        assert_eq!(mirror.set_generation(), 2, "one left");
        assert_eq!(take(&mut mirror).set, Some(2));
    }

    #[test]
    fn a_devices_description_generation_moves_on_its_state_or_reason_and_not_its_health() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "a", &[])], None);
        let changes = take(&mut mirror);
        assert_eq!(
            changes.described,
            [(0, 1)],
            "a new device's first generation"
        );
        mirror.set_status(0, DeviceState::BackingOff, DeviceHealth::Ok, "bus");
        assert_eq!(take(&mut mirror).described, [(0, 2)]);
        mirror.set_status(0, DeviceState::BackingOff, DeviceHealth::Ok, "bus gone");
        assert_eq!(
            take(&mut mirror).described,
            [(0, 3)],
            "the reason is in the description"
        );
        mirror.set_status(0, DeviceState::BackingOff, DeviceHealth::Failed, "bus gone");
        assert!(
            take(&mut mirror).described.is_empty(),
            "health is not in the description"
        );
        mirror.set_status(0, DeviceState::Running, DeviceHealth::Failed, "bus gone");
        assert_eq!(take(&mut mirror).described, [(0, 4)]);
        // Steady: the same status again is not a change.
        mirror.set_status(0, DeviceState::Running, DeviceHealth::Failed, "bus gone");
        assert!(take(&mut mirror).described.is_empty());
    }

    #[test]
    fn offers_are_the_contracts_of_the_running_devices_and_change_with_them() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "a", &[]), spec(1, "b", &[])], None);
        let _ = take(&mut mirror);
        assert_eq!(mirror.snapshot().1, [fact("offers", "imu")]);
        mirror.set_status(0, DeviceState::Off, DeviceHealth::Ok, "");
        assert!(take(&mut mirror).hello.is_none());
        mirror.set_status(1, DeviceState::Off, DeviceHealth::Ok, "");
        assert_eq!(take(&mut mirror).hello, Some(Vec::new()));
        mirror.set_status(0, DeviceState::Running, DeviceHealth::Ok, "");
        assert_eq!(take(&mut mirror).hello, Some(vec![fact("offers", "imu")]));
        mirror.set_status(0, DeviceState::Running, DeviceHealth::Failed, "read");
        assert!(take(&mut mirror).hello.is_none());
    }

    #[test]
    fn the_identity_facts_come_with_offers_and_go_when_they_cannot_be_read() {
        let mut mirror = Mirror::new();
        mirror.set_devices(vec![spec(0, "a", &[])], Some(vec![fact("family", "alpha")]));
        let first = take(&mut mirror);
        assert_eq!(
            first.hello,
            Some(vec![fact("family", "alpha"), fact("offers", "imu")])
        );
        mirror.set_identity(Some(vec![fact("family", "alpha")]));
        assert!(take(&mut mirror).hello.is_none());
        mirror.set_identity(Some(vec![fact("family", "beta")]));
        assert_eq!(
            take(&mut mirror).hello,
            Some(vec![fact("family", "beta"), fact("offers", "imu")])
        );
        mirror.set_identity(None);
        assert_eq!(take(&mut mirror).hello, Some(vec![fact("offers", "imu")]));
    }
}
