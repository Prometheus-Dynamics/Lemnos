use crate::LinuxPaths;
use crate::util::{file_name, read_dir_sorted, read_trimmed};
use lemnos_core::{
    DeviceAddress, DeviceControlSurface, DeviceDescriptor, DeviceKind, InterfaceKind,
};
use lemnos_discovery::{DiscoveryContext, DiscoveryError, DiscoveryProbe, ProbeDiscovery};
use std::path::Path;

const INTERFACES: [InterfaceKind; 1] = [InterfaceKind::Platform];

/// Reports each thermal zone under `/sys/class/thermal` as a
/// [`InterfaceKind::Platform`] device (`linux.subsystem = "thermal"`), for
/// the facade's thermal-zone driver. Live temperatures are state telemetry,
/// not descriptor properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThermalDiscoveryProbe {
    paths: LinuxPaths,
}

impl ThermalDiscoveryProbe {
    pub fn new(paths: LinuxPaths) -> Self {
        Self { paths }
    }
}

impl DiscoveryProbe for ThermalDiscoveryProbe {
    fn name(&self) -> &'static str {
        "linux-thermal"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &INTERFACES
    }

    fn discover(&self, context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
        let mut discovery = ProbeDiscovery::default();
        if !context.wants(InterfaceKind::Platform) {
            return Ok(discovery);
        }
        let root = self.paths.thermal_class_root();
        if !root.exists() {
            discovery.notes.push(format!(
                "Linux thermal sysfs root '{}' is not present",
                root.display()
            ));
            return Ok(discovery);
        }
        let entries = read_dir_sorted(&root).map_err(|error| DiscoveryError::ProbeFailed {
            probe: self.name().to_string(),
            message: format!("enumerate thermal zones at '{}': {error}", root.display()),
        })?;
        for entry in entries {
            let Some(name) = file_name(&entry) else {
                continue;
            };
            if !name.starts_with("thermal_zone") {
                continue;
            }
            match build_zone_descriptor(&entry, name) {
                Ok(descriptor) => discovery.devices.push(descriptor),
                Err(note) => discovery.notes.push(note),
            }
        }
        Ok(discovery)
    }
}

fn build_zone_descriptor(path: &Path, zone: &str) -> Result<DeviceDescriptor, String> {
    let zone_type = read_trimmed(&path.join("type")).map_err(|error| {
        format!(
            "failed to read thermal zone type at '{}': {error}",
            path.display()
        )
    })?;
    let mut builder = DeviceDescriptor::builder_for_kind(
        format!("linux.thermal.{zone}"),
        DeviceKind::Unspecified(InterfaceKind::Platform),
    )
    .map_err(|error| format!("failed to start thermal zone descriptor '{zone}': {error}"))?
    .display_name(zone_type.clone().unwrap_or_else(|| zone.to_string()))
    .summary("Linux thermal zone")
    .address(DeviceAddress::Custom {
        interface: InterfaceKind::Platform,
        scheme: "linux-thermal-zone".into(),
        value: zone.into(),
    })
    .label("backend", "linux")
    .label("subsystem", "thermal")
    .control_surface(DeviceControlSurface::LinuxClass {
        root: path.display().to_string(),
    })
    .label("device.class", "temperature")
    .property("device.class", "temperature")
    .property("linux.subsystem", "thermal")
    .property("linux.class_path", path.display().to_string());
    if let Some(zone_type) = zone_type {
        builder = builder.property("thermal.type", zone_type);
    }
    builder
        .build()
        .map_err(|error| format!("failed to build thermal zone descriptor '{zone}': {error}"))
}
