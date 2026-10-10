//! What the bridge publishes for lemnosd's devices: resource records, status
//! entries and the deadbands that keep readings quiet. No I/O here, so the
//! contract in `docs/orion.md` is tested without Orion or hardware.

use std::collections::BTreeMap;
use std::time::Duration;

use lemnos_device::CalibrationStatus;
use lemnos_ipc::{ChannelDesc, ControlDesc, DeviceDesc, DeviceStatus, NO_VALUE, Quantity};
use orion_control_plane::{
    AvailabilityState, HealthState, ResourceRecord, StatusEntry, StatusSubject, TypedConfigValue,
};
use orion_core::{ProviderId, ResourceId, ResourceType};

/// The Orion provider id the bridge registers.
pub const PROVIDER_ID: &str = "lemnos";
/// The Orion resource type of every lemnosd device.
pub const RESOURCE_TYPE: &str = "lemnos.device";

/// The Orion resource id of a board's device: `lemnos.<board>.<device>`.
pub fn resource_id(board: &str, device: &str) -> ResourceId {
    ResourceId::new(format!("lemnos.{board}.{device}"))
}

/// How often readings may be published, and how long entries live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadence {
    /// At least this long between reading publishes of one device (1 s / rate).
    pub min_interval: Duration,
    /// Every status entry is republished at least this often.
    pub heartbeat: Duration,
    /// The TTL of every status entry.
    pub ttl: Duration,
}

impl Cadence {
    /// Readings at most `rate_hz` times a second per device.
    pub fn new(rate_hz: f64, heartbeat: Duration, ttl: Duration) -> Self {
        let min_interval = if rate_hz > 0.0 {
            Duration::from_secs_f64(1.0 / rate_hz)
        } else {
            Duration::from_secs(1)
        };
        Self {
            min_interval,
            heartbeat,
            ttl,
        }
    }

    fn ttl_ms(self) -> u64 {
        u64::try_from(self.ttl.as_millis()).unwrap_or(u64::MAX)
    }
}

/// The smallest change of a quantity worth publishing (in its unit). Zero
/// for quantities that are not continuous (a level, a mode, a colour).
pub fn deadband(quantity: Quantity) -> f64 {
    match quantity {
        Quantity::Acceleration => 0.05,
        Quantity::AngularRate => 0.01,
        Quantity::MagneticField => 1e-7,
        Quantity::Voltage | Quantity::Current => 0.005,
        Quantity::Power => 0.05,
        Quantity::Temperature => 0.2,
        Quantity::RotationalSpeed => 20.0,
        Quantity::Ratio => 0.005,
        Quantity::Position | Quantity::Frequency => 1.0,
        Quantity::Angle => 0.01,
        Quantity::Pressure => 100.0,
        _ => 0.0,
    }
}

/// The name of a device status as published under `status`.
pub fn status_name(status: DeviceStatus) -> &'static str {
    match status {
        DeviceStatus::Available => "available",
        DeviceStatus::Degraded => "degraded",
        DeviceStatus::Faulted => "faulted",
        DeviceStatus::Missing => "missing",
    }
}

/// A device status as the resource's availability and health.
/// `Faulted` is present but not usable (unavailable, failed); `Missing` is
/// not reachable (unavailable, unknown health).
pub fn availability(status: DeviceStatus) -> (AvailabilityState, HealthState) {
    match status {
        DeviceStatus::Available => (AvailabilityState::Available, HealthState::Healthy),
        DeviceStatus::Degraded => (AvailabilityState::Available, HealthState::Degraded),
        DeviceStatus::Faulted => (AvailabilityState::Unavailable, HealthState::Failed),
        DeviceStatus::Missing => (AvailabilityState::Unavailable, HealthState::Unknown),
    }
}

/// A channel's value scaled by its exponent, as the status lane carries it.
fn value(raw: i32, exponent: i8) -> f64 {
    let scale = 10f64.powi(i32::from(exponent.unsigned_abs()));
    if exponent < 0 {
        f64::from(raw) / scale
    } else {
        f64::from(raw) * scale
    }
}

/// One lemnosd device as the bridge mirrors it.
struct Device {
    desc: DeviceDesc,
    driver: Option<String>,
    resource: ResourceId,
    /// Last status published (`status`, `reason`).
    status: Option<(DeviceStatus, String)>,
    /// Last value published per channel (`None`: never).
    values: Vec<Option<f64>>,
    /// Last value published per control (`None`: never).
    controls: Vec<Option<f64>>,
    /// Last value published per calibration key (see `Mirror::calibration`).
    calibration: BTreeMap<String, TypedConfigValue>,
    read_us: Option<u64>,
    /// Board-level values of a light (its look and owner, the presets, the
    /// brightness), by key.
    extras: BTreeMap<String, TypedConfigValue>,
    last_publish: Option<Duration>,
    last_heartbeat: Option<Duration>,
}

/// The bridge's view of the board's devices, and what has been published.
pub struct Mirror {
    board: String,
    cadence: Cadence,
    devices: Vec<Device>,
}

impl Mirror {
    /// Mirrors `devices` of `board`. `drivers` maps device ids to driver names
    /// (from the board definition); unknown ones get no `lemnos.driver` label.
    pub fn new(
        board: &str,
        devices: Vec<DeviceDesc>,
        drivers: &BTreeMap<String, String>,
        cadence: Cadence,
    ) -> Self {
        let devices = devices
            .into_iter()
            .map(|desc| device_from(board, desc, drivers))
            .collect();
        Self {
            board: board.to_owned(),
            cadence,
            devices,
        }
    }

    /// The board's id.
    pub fn board(&self) -> &str {
        &self.board
    }

    /// The device with this id, if the board has one.
    pub fn device(&self, id: &str) -> Option<&DeviceDesc> {
        self.find(id).map(|d| &d.desc)
    }

    /// The resource's device id (the part after `lemnos.<board>.`).
    pub fn device_of_resource(&self, resource: &str) -> Option<String> {
        self.devices
            .iter()
            .find(|d| d.resource.as_str() == resource)
            .map(|d| d.desc.id.clone())
    }

    /// Every device's resource record.
    pub fn resources(&self) -> Vec<ResourceRecord> {
        self.devices.iter().map(|d| d.record(&self.board)).collect()
    }

    /// Status entries for every device: all the values known, for a
    /// (re)publish after the provider or its node came back.
    pub fn snapshot(&mut self, now: Duration) -> Vec<StatusEntry> {
        let ttl = self.cadence.ttl_ms();
        let mut out = Vec::new();
        for device in &mut self.devices {
            device.full(now, ttl, &mut out);
        }
        out
    }

    /// Sets a board-level value of `device` (see `Device::extras`): its entry
    /// when the value changed, nothing when it did not.
    pub fn extra(&mut self, device: &str, key: &str, value: TypedConfigValue) -> Vec<StatusEntry> {
        let ttl = self.cadence.ttl_ms();
        let Some(found) = self.find_mut(device) else {
            return Vec::new();
        };
        if found.extras.get(key) == Some(&value) {
            return Vec::new();
        }
        found.extras.insert(key.to_owned(), value.clone());
        vec![entry(&found.resource, key.to_owned(), value, ttl)]
    }

    /// Status entries for the devices whose heartbeat is due.
    pub fn heartbeat(&mut self, now: Duration) -> Vec<StatusEntry> {
        let ttl = self.cadence.ttl_ms();
        let heartbeat = self.cadence.heartbeat;
        let mut out = Vec::new();
        for device in &mut self.devices {
            let due = device
                .last_heartbeat
                .is_none_or(|at| now.saturating_sub(at) >= heartbeat);
            if due {
                device.full(now, ttl, &mut out);
            }
        }
        out
    }

    /// Entries for a reading: the status when it changed, `read_us`, and each
    /// channel that moved by its deadband. Readings come at most once per
    /// `min_interval`; a reading inside that window publishes nothing (the
    /// next one carries the change).
    pub fn reading(
        &mut self,
        device_id: &str,
        status: DeviceStatus,
        timestamp_us: u64,
        raw: &[i32],
        now: Duration,
    ) -> Vec<StatusEntry> {
        let ttl = self.cadence.ttl_ms();
        let min_interval = self.cadence.min_interval;
        let Some(device) = self.find_mut(device_id) else {
            return Vec::new();
        };
        if device
            .last_publish
            .is_some_and(|at| now.saturating_sub(at) < min_interval)
        {
            return Vec::new();
        }
        // A reading says the status but not why: keep the reason while the
        // status is the same, and clear it when the status changes.
        let reason = device
            .status
            .as_ref()
            .filter(|(current, _)| *current == status)
            .map(|(_, reason)| reason.clone())
            .unwrap_or_default();
        let mut out = Vec::new();
        device.status_change(status, &reason, ttl, &mut out);
        for index in 0..device.desc.channels.len() {
            let raw = raw.get(index).copied().unwrap_or(NO_VALUE);
            if raw == NO_VALUE {
                continue;
            }
            let channel = &device.desc.channels[index];
            let new = value(raw, channel.exponent);
            let band = deadband(channel.quantity);
            let moved = device.values[index].is_none_or(|old| (new - old).abs() >= band);
            if moved {
                device.values[index] = Some(new);
                out.push(entry(
                    &device.resource,
                    channel.name.clone(),
                    TypedConfigValue::F64(new),
                    ttl,
                ));
            }
        }
        if !out.is_empty() {
            device.read_us = Some(timestamp_us);
            out.push(entry(
                &device.resource,
                "read_us".to_owned(),
                TypedConfigValue::UInt(timestamp_us),
                ttl,
            ));
        }
        if !out.is_empty() {
            device.last_publish = Some(now);
        }
        out
    }

    /// Entries for a status change (`Event::Status`), not rate limited.
    pub fn status(&mut self, device: &str, status: DeviceStatus, reason: &str) -> Vec<StatusEntry> {
        let ttl = self.cadence.ttl_ms();
        let Some(device) = self.find_mut(device) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        device.status_change(status, reason, ttl, &mut out);
        out
    }

    /// Entries for a control's value (`Event::Control`, or a `get`), when it
    /// changed.
    pub fn control(&mut self, device: &str, control: &str, value: f64) -> Vec<StatusEntry> {
        let ttl = self.cadence.ttl_ms();
        let Some(device) = self.find_mut(device) else {
            return Vec::new();
        };
        let Some(index) = device.desc.controls.iter().position(|c| c.name == control) else {
            return Vec::new();
        };
        if device.controls[index] == Some(value) {
            return Vec::new();
        }
        device.controls[index] = Some(value);
        vec![entry(
            &device.resource,
            control_key(control),
            TypedConfigValue::F64(value),
            ttl,
        )]
    }

    /// The calibration's confidence keys (`calibration.<part>.confidence` and
    /// `.active`, `calibration.candidate`, `calibration.revision`), published
    /// when they change. Ratios are 0 to 1.
    pub fn calibration(&mut self, device: &str, status: &CalibrationStatus) -> Vec<StatusEntry> {
        let ttl = self.cadence.ttl_ms();
        let Some(device) = self.find_mut(device) else {
            return Vec::new();
        };
        let mut values: Vec<(String, TypedConfigValue)> = vec![
            (
                "calibration.revision".to_owned(),
                TypedConfigValue::UInt(u64::from(status.revision)),
            ),
            (
                "calibration.candidate".to_owned(),
                TypedConfigValue::Bool(status.candidate),
            ),
        ];
        for (name, index) in [
            ("accel", lemnos_device::PART_ACCEL),
            ("gyro", lemnos_device::PART_GYRO),
            ("mag", lemnos_device::PART_MAG),
        ] {
            let part = status.parts[index];
            values.push((
                format!("calibration.{name}.confidence"),
                TypedConfigValue::F64(f64::from(part.confidence) / 1000.0),
            ));
            values.push((
                format!("calibration.{name}.active"),
                TypedConfigValue::Bool(part.active),
            ));
        }
        let mut out = Vec::new();
        for (key, value) in values {
            let last = device.calibration.get(&key);
            if last != Some(&value) {
                device.calibration.insert(key.clone(), value.clone());
                out.push(entry(&device.resource, key, value, ttl));
            }
        }
        out
    }

    /// The controls to read at connection: their names, for `get`.
    pub fn control_names(&self, device: &str) -> Vec<String> {
        self.find(device)
            .map(|d| d.desc.controls.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default()
    }

    /// Forgets what was published, so the next readings publish everything
    /// again (a reconnection, or a new device list).
    pub fn reset(&mut self, devices: Vec<DeviceDesc>, drivers: &BTreeMap<String, String>) {
        let board = self.board.clone();
        self.devices = devices
            .into_iter()
            .map(|desc| device_from(&board, desc, drivers))
            .collect();
    }

    fn find(&self, id: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.desc.id == id)
    }

    fn find_mut(&mut self, id: &str) -> Option<&mut Device> {
        self.devices.iter_mut().find(|d| d.desc.id == id)
    }
}

fn device_from(board: &str, desc: DeviceDesc, drivers: &BTreeMap<String, String>) -> Device {
    let driver = drivers.get(&desc.id).cloned();
    Device::new(board, desc, driver)
}

fn control_key(control: &str) -> String {
    format!("control.{control}")
}

fn entry(resource: &ResourceId, key: String, value: TypedConfigValue, ttl: u64) -> StatusEntry {
    let mut entry = StatusEntry::new(StatusSubject::Resource(resource.clone()), key, value);
    entry.ttl_ms = ttl;
    entry
}

impl Device {
    fn new(board: &str, desc: DeviceDesc, driver: Option<String>) -> Self {
        let channels = desc.channels.len();
        let controls = desc.controls.len();
        Self {
            resource: resource_id(board, &desc.id),
            driver,
            status: None,
            values: vec![None; channels],
            controls: vec![None; controls],
            calibration: BTreeMap::new(),
            read_us: None,
            extras: BTreeMap::new(),
            last_publish: None,
            last_heartbeat: None,
            desc,
        }
    }

    /// The resource record: type, availability and health from the status,
    /// and the labels of `docs/orion.md`.
    fn record(&self, board: &str) -> ResourceRecord {
        let (availability, health) = availability(self.desc.status);
        let mut builder = ResourceRecord::builder(
            self.resource.clone(),
            ResourceType::new(RESOURCE_TYPE),
            ProviderId::new(PROVIDER_ID),
        )
        .availability(availability)
        .health(health)
        .label(format!("lemnos.class={}", self.desc.class))
        .label(format!("lemnos.model={}", self.desc.model))
        .label(format!("lemnos.board={board}"));
        if let Some(driver) = &self.driver {
            builder = builder.label(format!("lemnos.driver={driver}"));
        }
        for channel in &self.desc.channels {
            builder = builder.label(unit_label(channel));
        }
        for control in &self.desc.controls {
            builder = builder.label(control_label(control));
        }
        builder.build()
    }

    /// Records the status (and reason) if it changed, pushing its entries.
    fn status_change(
        &mut self,
        status: DeviceStatus,
        reason: &str,
        ttl: u64,
        out: &mut Vec<StatusEntry>,
    ) {
        let reason = if status == DeviceStatus::Available {
            String::new()
        } else {
            reason.to_owned()
        };
        if self.status.as_ref() == Some(&(status, reason.clone())) {
            return;
        }
        self.status = Some((status, reason.clone()));
        self.desc.status = status;
        out.push(entry(
            &self.resource,
            "status".to_owned(),
            TypedConfigValue::String(status_name(status).to_owned()),
            ttl,
        ));
        out.push(entry(
            &self.resource,
            "reason".to_owned(),
            TypedConfigValue::String(reason),
            ttl,
        ));
    }

    /// Every entry this device has published or can publish now.
    fn full(&mut self, now: Duration, ttl: u64, out: &mut Vec<StatusEntry>) {
        let status = self.desc.status;
        let reason = self
            .status
            .as_ref()
            .map(|(_, reason)| reason.clone())
            .unwrap_or_default();
        out.push(entry(
            &self.resource,
            "status".to_owned(),
            TypedConfigValue::String(status_name(status).to_owned()),
            ttl,
        ));
        out.push(entry(
            &self.resource,
            "reason".to_owned(),
            TypedConfigValue::String(reason.clone()),
            ttl,
        ));
        self.status = Some((status, reason));
        if let Some(read_us) = self.read_us {
            out.push(entry(
                &self.resource,
                "read_us".to_owned(),
                TypedConfigValue::UInt(read_us),
                ttl,
            ));
        }
        for (channel, value) in self.desc.channels.iter().zip(&self.values) {
            if let Some(value) = value {
                out.push(entry(
                    &self.resource,
                    channel.name.clone(),
                    TypedConfigValue::F64(*value),
                    ttl,
                ));
            }
        }
        for (control, value) in self.desc.controls.iter().zip(&self.controls) {
            if let Some(value) = value {
                out.push(entry(
                    &self.resource,
                    control_key(&control.name),
                    TypedConfigValue::F64(*value),
                    ttl,
                ));
            }
        }
        for (key, value) in &self.extras {
            out.push(entry(&self.resource, key.clone(), value.clone(), ttl));
        }
        self.last_heartbeat = Some(now);
        self.last_publish = Some(now);
    }
}

fn unit_label(channel: &ChannelDesc) -> String {
    format!(
        "lemnos.unit.{}={}",
        channel.name,
        channel.quantity.unit().symbol()
    )
}

fn control_label(control: &ControlDesc) -> String {
    let min = value(control.min, control.exponent);
    let max = value(control.max, control.exponent);
    let symbol = control.quantity.unit().symbol();
    if symbol.is_empty() {
        format!("lemnos.control.{}={min}..{max}", control.name)
    } else {
        format!("lemnos.control.{}={min}..{max} {symbol}", control.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_ipc::{Axis, DeviceClass};

    const RATE: f64 = 2.0;

    fn cadence() -> Cadence {
        Cadence::new(RATE, Duration::from_secs(30), Duration::from_secs(90))
    }

    fn device() -> DeviceDesc {
        DeviceDesc {
            id: "power".into(),
            label: "Power".into(),
            class: DeviceClass::PowerMonitor,
            model: "ina238".into(),
            status: DeviceStatus::Available,
            reason: String::new(),
            channels: vec![
                ChannelDesc {
                    name: "bus_voltage".into(),
                    quantity: Quantity::Voltage,
                    axis: Axis::None,
                    exponent: -3,
                },
                ChannelDesc {
                    name: "current".into(),
                    quantity: Quantity::Current,
                    axis: Axis::None,
                    exponent: -3,
                },
            ],
            controls: vec![ControlDesc {
                name: "duty".into(),
                quantity: Quantity::Ratio,
                exponent: -3,
                min: 0,
                max: 1000,
            }],
            pixels: 0,
        }
    }

    fn mirror() -> Mirror {
        Mirror::new("test", vec![device()], &BTreeMap::new(), cadence())
    }

    fn keys(entries: &[StatusEntry]) -> Vec<String> {
        entries.iter().map(|e| e.key.clone()).collect()
    }

    fn value(entries: &[StatusEntry], key: &str) -> Option<TypedConfigValue> {
        entries
            .iter()
            .find(|e| e.key == key)
            .map(|e| e.value.clone())
    }

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    #[test]
    fn first_reading_publishes_status_channels_and_read_us() {
        let mut m = mirror();
        let out = m.reading(
            "power",
            DeviceStatus::Available,
            1_000,
            &[12_000, 250],
            secs(1.0),
        );
        assert_eq!(
            keys(&out),
            ["status", "reason", "bus_voltage", "current", "read_us"]
        );
        assert_eq!(
            value(&out, "bus_voltage"),
            Some(TypedConfigValue::F64(12.0))
        );
        assert_eq!(value(&out, "current"), Some(TypedConfigValue::F64(0.25)));
        assert!(out.iter().all(|e| e.ttl_ms == 90_000));
    }

    #[test]
    fn a_change_inside_the_deadband_publishes_nothing() {
        let mut m = mirror();
        m.reading(
            "power",
            DeviceStatus::Available,
            1,
            &[12_000, 250],
            secs(1.0),
        );
        // 0.001 V moved; the voltage deadband is 0.005 V.
        let out = m.reading(
            "power",
            DeviceStatus::Available,
            2,
            &[12_001, 250],
            secs(2.0),
        );
        assert!(out.is_empty(), "published: {:?}", keys(&out));
    }

    #[test]
    fn a_change_past_the_deadband_publishes_only_that_channel() {
        let mut m = mirror();
        m.reading(
            "power",
            DeviceStatus::Available,
            1,
            &[12_000, 250],
            secs(1.0),
        );
        let out = m.reading(
            "power",
            DeviceStatus::Available,
            2,
            &[12_010, 250],
            secs(2.0),
        );
        assert_eq!(keys(&out), ["bus_voltage", "read_us"]);
    }

    #[test]
    fn readings_are_rate_limited_to_the_cadence() {
        let mut m = mirror();
        m.reading(
            "power",
            DeviceStatus::Available,
            1,
            &[12_000, 250],
            secs(1.0),
        );
        // 0.2 s later (rate 2 Hz means one per 0.5 s): nothing, even though the
        // value moved far past the deadband.
        let early = m.reading(
            "power",
            DeviceStatus::Available,
            2,
            &[13_000, 250],
            secs(1.2),
        );
        assert!(early.is_empty());
        // The next reading after the window carries the change.
        let later = m.reading(
            "power",
            DeviceStatus::Available,
            3,
            &[13_000, 250],
            secs(1.6),
        );
        assert_eq!(
            value(&later, "bus_voltage"),
            Some(TypedConfigValue::F64(13.0))
        );
    }

    #[test]
    fn status_publishes_once_and_its_reason_follows_the_event() {
        let mut m = mirror();
        let out = m.status("power", DeviceStatus::Faulted, "read: bus error");
        assert_eq!(
            value(&out, "status"),
            Some(TypedConfigValue::String("faulted".into()))
        );
        assert_eq!(
            value(&out, "reason"),
            Some(TypedConfigValue::String("read: bus error".into()))
        );
        // The same status again is not a change.
        assert!(
            m.status("power", DeviceStatus::Faulted, "read: bus error")
                .is_empty()
        );
        // A reading with the same status keeps the reason.
        let read = m.reading("power", DeviceStatus::Faulted, 5, &[1, 1], secs(10.0));
        assert!(
            value(&read, "reason").is_none(),
            "reason rewritten: {:?}",
            keys(&read)
        );
    }

    #[test]
    fn available_clears_the_reason() {
        let mut m = mirror();
        m.status("power", DeviceStatus::Faulted, "init: nope");
        let out = m.status("power", DeviceStatus::Available, "");
        assert_eq!(
            value(&out, "reason"),
            Some(TypedConfigValue::String(String::new()))
        );
    }

    #[test]
    fn heartbeat_is_due_after_its_interval_and_republishes_everything_known() {
        let mut m = mirror();
        m.reading(
            "power",
            DeviceStatus::Available,
            7,
            &[12_000, 250],
            secs(1.0),
        );
        m.control("power", "duty", 0.5);
        // No heartbeat has gone out yet, so the first call is due.
        let first = m.heartbeat(secs(1.0));
        assert!(keys(&first).contains(&"bus_voltage".to_owned()));
        assert!(keys(&first).contains(&"control.duty".to_owned()));
        assert!(keys(&first).contains(&"read_us".to_owned()));
        // Within the heartbeat interval: nothing.
        assert!(m.heartbeat(secs(10.0)).is_empty());
        // After it: everything again.
        let due = m.heartbeat(secs(31.5));
        assert_eq!(
            value(&due, "bus_voltage"),
            Some(TypedConfigValue::F64(12.0))
        );
    }

    #[test]
    fn controls_publish_only_on_change() {
        let mut m = mirror();
        assert_eq!(m.control("power", "duty", 0.5).len(), 1);
        assert!(m.control("power", "duty", 0.5).is_empty());
        let out = m.control("power", "duty", 0.75);
        assert_eq!(keys(&out), ["control.duty"]);
        assert!(m.control("power", "missing", 1.0).is_empty());
    }

    #[test]
    fn resource_labels_follow_the_contract() {
        let m = mirror();
        let resources = m.resources();
        assert_eq!(resources.len(), 1);
        let r = &resources[0];
        assert_eq!(r.resource_id.as_str(), "lemnos.test.power");
        assert_eq!(r.resource_type.as_str(), RESOURCE_TYPE);
        for label in [
            "lemnos.class=power-monitor",
            "lemnos.model=ina238",
            "lemnos.board=test",
            "lemnos.unit.bus_voltage=V",
            "lemnos.unit.current=A",
            "lemnos.control.duty=0..1",
        ] {
            assert!(
                r.labels.iter().any(|l| l == label),
                "missing {label}: {:?}",
                r.labels
            );
        }
    }

    #[test]
    fn device_of_resource_maps_back() {
        let m = mirror();
        assert_eq!(
            m.device_of_resource("lemnos.test.power").as_deref(),
            Some("power")
        );
        assert_eq!(m.device_of_resource("lemnos.test.other"), None);
    }

    #[test]
    fn deadband_table_is_zero_for_discrete_quantities() {
        assert_eq!(deadband(Quantity::Mode), 0.0);
        assert!(deadband(Quantity::Temperature) > 0.0);
    }
}
