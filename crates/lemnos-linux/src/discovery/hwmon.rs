use crate::LinuxPaths;
use crate::metadata::with_linux_driver;
use crate::util::{file_name, read_dir_sorted, read_link_name, read_trimmed, read_u32};
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceControlSurface, DeviceDescriptor,
    DeviceHealth, DeviceKind, InterfaceKind,
};
use lemnos_discovery::{DiscoveryContext, DiscoveryError, DiscoveryProbe, ProbeDiscovery};
use std::path::Path;

const INTERFACES: [InterfaceKind; 1] = [InterfaceKind::Pwm];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwmonDiscoveryProbe {
    paths: LinuxPaths,
}

impl HwmonDiscoveryProbe {
    pub fn new(paths: LinuxPaths) -> Self {
        Self { paths }
    }
}

impl DiscoveryProbe for HwmonDiscoveryProbe {
    fn name(&self) -> &'static str {
        "linux-hwmon"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &INTERFACES
    }

    fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
        let hwmon_root = self.paths.hwmon_class_root();
        let mut discovery = ProbeDiscovery::default();

        if !hwmon_root.exists() {
            discovery.notes.push(format!(
                "Linux hwmon sysfs root '{}' is not present",
                hwmon_root.display()
            ));
            return Ok(discovery);
        }

        let entries = read_dir_sorted(&hwmon_root).map_err(|error| {
            probe_failed(
                self.name(),
                &hwmon_root,
                "enumerate Linux hwmon devices",
                error,
            )
        })?;

        for entry in entries {
            if !entry.is_dir() {
                continue;
            }
            let Some(name) = file_name(&entry) else {
                continue;
            };

            match build_hwmon_descriptor(&entry, name) {
                Ok(Some(descriptor)) => discovery.devices.push(descriptor),
                Ok(None) => {}
                Err(note) => discovery.notes.push(note),
            }
        }

        Ok(discovery)
    }
}

fn build_hwmon_descriptor(
    path: &Path,
    hwmon_name: &str,
) -> Result<Option<DeviceDescriptor>, String> {
    if !path.join("pwm1").exists() {
        return Ok(None);
    }

    let name = read_trimmed(&path.join("name")).map_err(|error| {
        format!(
            "failed to read Linux hwmon name for '{}' at '{}': {error}",
            hwmon_name,
            path.display()
        )
    })?;
    let pwm = read_u32(&path.join("pwm1")).map_err(|error| {
        format!(
            "failed to read Linux hwmon pwm1 for '{}' at '{}': {error}",
            hwmon_name,
            path.display()
        )
    })?;
    let pwm_mode = read_u32(&path.join("pwm1_enable")).map_err(|error| {
        format!(
            "failed to read Linux hwmon pwm1_enable for '{}' at '{}': {error}",
            hwmon_name,
            path.display()
        )
    })?;
    let rpm = read_u32(&path.join("fan1_input")).map_err(|error| {
        format!(
            "failed to read Linux hwmon fan1_input for '{}' at '{}': {error}",
            hwmon_name,
            path.display()
        )
    })?;
    let driver = read_link_name(&path.join("device").join("driver")).map_err(|error| {
        format!(
            "failed to read Linux hwmon driver for '{}' at '{}': {error}",
            hwmon_name,
            path.display()
        )
    })?;
    let device_name = read_link_name(&path.join("device")).map_err(|error| {
        format!(
            "failed to read Linux hwmon device link for '{}' at '{}': {error}",
            hwmon_name,
            path.display()
        )
    })?;

    let mut builder = DeviceDescriptor::builder_for_kind(
        format!("linux.pwm.hwmon-fan.{hwmon_name}"),
        DeviceKind::Unspecified(InterfaceKind::Pwm),
    )
    .map_err(|error| format!("failed to start Linux hwmon fan descriptor '{hwmon_name}': {error}"))?
    .display_name(name.clone().unwrap_or_else(|| hwmon_name.to_string()))
    .summary("Linux hwmon fan control device")
    .address(DeviceAddress::Custom {
        interface: InterfaceKind::Pwm,
        scheme: "linux-hwmon-fan".into(),
        value: hwmon_name.into(),
    })
    .label("backend", "linux")
    .label("subsystem", "hwmon")
    .label("hwmon_name", hwmon_name.to_string())
    .control_surface(DeviceControlSurface::LinuxClass {
        root: path.display().to_string(),
    })
    .property("linux.subsystem", "hwmon")
    .property("linux.class_path", path.display().to_string())
    .property("fan.hwmon_name", hwmon_name.to_string())
    .capability(fan_capability("fan.get_state", CapabilityAccess::READ))
    .capability(fan_capability("fan.set_pwm", CapabilityAccess::CONFIGURE))
    .capability(fan_capability("fan.get_pwm_mode", CapabilityAccess::READ))
    .capability(fan_capability(
        "fan.set_pwm_mode",
        CapabilityAccess::CONFIGURE,
    ));

    if let Some(name) = name {
        builder = builder.property("hwmon.name", name);
    }
    if let Some(pwm) = pwm {
        builder = builder.property("fan.pwm", u64::from(pwm));
    } else {
        builder = builder.health(DeviceHealth::Degraded);
    }
    if let Some(pwm_mode) = pwm_mode {
        builder = builder.property("fan.pwm_mode", u64::from(pwm_mode));
    } else {
        builder = builder.health(DeviceHealth::Degraded);
    }
    if let Some(rpm) = rpm {
        builder = builder.property("fan.rpm", u64::from(rpm));
    }
    if let Some(device_name) = device_name {
        builder = builder.property("linux.device", device_name);
    }
    if let Some(driver) = driver {
        builder = with_linux_driver(builder, driver);
    }

    builder.build().map(Some).map_err(|error| {
        format!("failed to build Linux hwmon fan descriptor '{hwmon_name}': {error}")
    })
}

fn fan_capability(id: &str, access: CapabilityAccess) -> CapabilityDescriptor {
    CapabilityDescriptor::new(id, access).expect("static fan capability identifiers are valid")
}

fn probe_failed(probe: &str, path: &Path, action: &str, error: std::io::Error) -> DiscoveryError {
    DiscoveryError::ProbeFailed {
        probe: probe.to_string(),
        message: format!("{action} at '{}': {error}", path.display()),
    }
}
