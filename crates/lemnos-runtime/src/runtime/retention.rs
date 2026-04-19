use lemnos_core::{
    CapabilityDescriptor, DeviceAddress, DeviceDescriptor, DeviceEvent, DeviceId, DeviceIssue,
    DeviceLink, DeviceRelation, DeviceStateSnapshot, InterfaceKind, LemnosEvent, MatchHints,
    OperationRecord, StateEvent, Value, ValueMap,
};
use std::collections::BTreeMap;
use std::mem::size_of;

pub(super) fn estimated_retained_event_bytes(event: &LemnosEvent) -> usize {
    match event {
        LemnosEvent::Inventory(event) => size_of::<DeviceEvent>() + inventory_event_bytes(event),
        LemnosEvent::State(event) => size_of::<StateEvent>() + state_event_bytes(event),
    }
}

fn inventory_event_bytes(event: &DeviceEvent) -> usize {
    match event {
        DeviceEvent::Added(descriptor) => {
            size_of::<DeviceDescriptor>() + descriptor_bytes(descriptor)
        }
        DeviceEvent::Changed { previous, current } => {
            size_of::<DeviceDescriptor>()
                + descriptor_bytes(previous)
                + size_of::<DeviceDescriptor>()
                + descriptor_bytes(current)
        }
        DeviceEvent::Removed(device_id) => string_id_bytes(device_id),
    }
}

fn state_event_bytes(event: &StateEvent) -> usize {
    match event {
        StateEvent::Snapshot(snapshot) => state_snapshot_bytes(snapshot.as_ref()),
        StateEvent::HealthChanged { device_id, .. } => string_id_bytes(device_id),
        StateEvent::LifecycleChanged { device_id, .. } => string_id_bytes(device_id),
        StateEvent::IssuesChanged {
            device_id, issues, ..
        } => string_id_bytes(device_id) + vec_bytes(issues, issue_bytes),
    }
}

fn descriptor_bytes(descriptor: &DeviceDescriptor) -> usize {
    string_id_bytes(&descriptor.id)
        + descriptor
            .local_id
            .as_ref()
            .map_or(0, |local_id| local_id.as_str().len())
        + descriptor
            .display_name
            .as_ref()
            .map_or(0, |display_name| display_name.len())
        + descriptor
            .summary
            .as_ref()
            .map_or(0, |summary| summary.len())
        + descriptor.address.as_ref().map_or(0, address_bytes)
        + string_map_bytes(&descriptor.labels)
        + value_map_bytes(&descriptor.properties)
        + vec_bytes(&descriptor.capabilities, capability_bytes)
        + vec_bytes(&descriptor.links, link_bytes)
        + match_hints_bytes(&descriptor.match_hints)
}

fn address_bytes(address: &DeviceAddress) -> usize {
    match address {
        DeviceAddress::GpioChip { chip_name, .. } => chip_name.len(),
        DeviceAddress::GpioLine { chip_name, .. } => chip_name.len(),
        DeviceAddress::PwmChip { chip_name } => chip_name.len(),
        DeviceAddress::PwmChannel { chip_name, .. } => chip_name.len(),
        DeviceAddress::I2cBus { .. } => 0,
        DeviceAddress::I2cDevice { .. } => 0,
        DeviceAddress::SpiBus { .. } => 0,
        DeviceAddress::SpiDevice { .. } => 0,
        DeviceAddress::UartPort { port } => port.len(),
        DeviceAddress::UartDevice { port, unit } => {
            port.len() + unit.as_ref().map_or(0, |unit| unit.len())
        }
        DeviceAddress::UsbBus { .. } => 0,
        DeviceAddress::UsbDevice { ports, .. } => ports.len(),
        DeviceAddress::UsbInterface { ports, .. } => ports.len(),
        DeviceAddress::Custom {
            interface,
            scheme,
            value,
        } => interface_bytes(interface) + scheme.len() + value.len(),
    }
}

fn capability_bytes(capability: &CapabilityDescriptor) -> usize {
    capability.id.as_str().len()
        + capability
            .summary
            .as_ref()
            .map_or(0, |summary| summary.len())
        + value_map_bytes(&capability.properties)
}

fn link_bytes(link: &DeviceLink) -> usize {
    string_id_bytes(&link.target)
        + relation_bytes(&link.relation)
        + value_map_bytes(&link.attributes)
}

fn relation_bytes(relation: &DeviceRelation) -> usize {
    match relation {
        DeviceRelation::Parent => "parent".len(),
        DeviceRelation::Controller => "controller".len(),
        DeviceRelation::Bus => "bus".len(),
        DeviceRelation::Transport => "transport".len(),
        DeviceRelation::Interface => "interface".len(),
        DeviceRelation::Channel => "channel".len(),
        DeviceRelation::Dependency => "dependency".len(),
        DeviceRelation::Peer => "peer".len(),
        DeviceRelation::Consumer => "consumer".len(),
        DeviceRelation::Provider => "provider".len(),
        DeviceRelation::Custom(value) => value.len(),
    }
}

fn match_hints_bytes(hints: &MatchHints) -> usize {
    hints.driver_hint.as_ref().map_or(0, |value| value.len())
        + hints.modalias.as_ref().map_or(0, |value| value.len())
        + vec_bytes(&hints.compatible, |value| value.len())
        + hints.vendor.as_ref().map_or(0, |value| value.len())
        + hints.model.as_ref().map_or(0, |value| value.len())
        + hints.revision.as_ref().map_or(0, |value| value.len())
        + hints.serial_number.as_ref().map_or(0, |value| value.len())
        + string_map_bytes(&hints.hardware_ids)
}

fn state_snapshot_bytes(snapshot: &DeviceStateSnapshot) -> usize {
    string_id_bytes(&snapshot.device_id)
        + vec_bytes(&snapshot.issues, issue_bytes)
        + value_map_bytes(&snapshot.realized_config)
        + value_map_bytes(&snapshot.telemetry)
        + snapshot
            .last_operation
            .as_ref()
            .map_or(0, operation_record_bytes)
}

fn issue_bytes(issue: &DeviceIssue) -> usize {
    issue.code.as_str().len() + issue.message.len() + value_map_bytes(&issue.attributes)
}

fn operation_record_bytes(record: &OperationRecord) -> usize {
    record.interaction.len()
        + record.summary.as_ref().map_or(0, |summary| summary.len())
        + record.output.as_ref().map_or(0, value_bytes)
}

fn value_map_bytes(values: &ValueMap) -> usize {
    values
        .iter()
        .map(|(key, value)| key.len() + value_bytes(value))
        .sum()
}

fn string_map_bytes(values: &BTreeMap<String, String>) -> usize {
    values
        .iter()
        .map(|(key, value)| key.len() + value.len())
        .sum()
}

fn value_bytes(value: &Value) -> usize {
    match value {
        Value::Null | Value::Bool(_) | Value::I64(_) | Value::U64(_) | Value::F64(_) => 0,
        Value::String(value) => value.len(),
        Value::Bytes(value) => value.len(),
        Value::List(values) => vec_bytes(values, value_bytes),
        Value::Map(values) => value_map_bytes(values),
    }
}

fn vec_bytes<T>(values: &[T], item_bytes: impl Fn(&T) -> usize) -> usize {
    std::mem::size_of_val(values) + values.iter().map(item_bytes).sum::<usize>()
}

fn string_id_bytes(id: &DeviceId) -> usize {
    id.as_str().len()
}

fn interface_bytes(interface: &InterfaceKind) -> usize {
    match interface {
        InterfaceKind::Gpio => "gpio".len(),
        InterfaceKind::Pwm => "pwm".len(),
        InterfaceKind::I2c => "i2c".len(),
        InterfaceKind::Spi => "spi".len(),
        InterfaceKind::Uart => "uart".len(),
        InterfaceKind::Usb => "usb".len(),
    }
}
