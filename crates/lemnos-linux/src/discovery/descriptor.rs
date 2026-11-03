//! Descriptor pieces that every Linux probe builds the same way, so the
//! probes share one copy of each instead of one per probe.

use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, CoreResult, DeviceAddress, DeviceControlSurface,
    DeviceDescriptor, DeviceDescriptorBuilder, DeviceKind,
};
use std::path::Path;

/// Starts a Linux descriptor: its ID, kind, display name, summary, address,
/// and the `backend = linux` label every Linux descriptor carries.
pub(super) fn start(
    id: String,
    kind: DeviceKind,
    display_name: String,
    summary: &str,
    address: DeviceAddress,
) -> CoreResult<DeviceDescriptorBuilder> {
    Ok(DeviceDescriptor::builder_for_kind(id, kind)?
        .display_name(display_name)
        .summary(summary)
        .address(address)
        .label("backend", "linux"))
}

/// A sysfs class device (hwmon, thermal zone, LED): its subsystem, its class
/// directory, and the control surface a driver reaches it through.
pub(super) fn class_device(
    builder: DeviceDescriptorBuilder,
    subsystem: &'static str,
    path: &Path,
) -> DeviceDescriptorBuilder {
    let class_path = path.display().to_string();
    builder
        .label("subsystem", subsystem)
        .control_surface(DeviceControlSurface::LinuxClass {
            root: class_path.clone(),
        })
        .property("linux.subsystem", subsystem)
        .property("linux.class_path", class_path)
}

/// A capability with a fixed identifier from a probe's own table.
pub(super) fn capability(id: &str, access: CapabilityAccess) -> CapabilityDescriptor {
    CapabilityDescriptor::new(id, access).expect("static Linux capability identifiers are valid")
}
