use crate::LinuxPaths;
use crate::metadata::with_linux_driver;
use crate::util::{file_name, read_dir_sorted, read_link_name, read_trimmed, read_u32};
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceControlSurface, DeviceDescriptor,
    DeviceHealth, DeviceKind, InterfaceKind, Value,
};
use lemnos_discovery::{DiscoveryContext, DiscoveryError, DiscoveryProbe, ProbeDiscovery};
use std::path::Path;

const INTERFACES: [InterfaceKind; 1] = [InterfaceKind::Gpio];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedDiscoveryProbe {
    paths: LinuxPaths,
}

impl LedDiscoveryProbe {
    pub fn new(paths: LinuxPaths) -> Self {
        Self { paths }
    }
}

impl DiscoveryProbe for LedDiscoveryProbe {
    fn name(&self) -> &'static str {
        "linux-leds"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &INTERFACES
    }

    fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
        let leds_root = self.paths.led_class_root();
        let mut discovery = ProbeDiscovery::default();

        if !leds_root.exists() {
            discovery.notes.push(format!(
                "Linux LED sysfs root '{}' is not present",
                leds_root.display()
            ));
            return Ok(discovery);
        }

        let entries = read_dir_sorted(&leds_root).map_err(|error| {
            probe_failed(
                self.name(),
                &leds_root,
                "enumerate Linux LED devices",
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

            match build_led_descriptor(&entry, name) {
                Ok(descriptor) => discovery.devices.push(descriptor),
                Err(note) => discovery.notes.push(note),
            }
        }

        Ok(discovery)
    }
}

fn build_led_descriptor(path: &Path, led_name: &str) -> Result<DeviceDescriptor, String> {
    let brightness = read_u32(&path.join("brightness")).map_err(|error| {
        format!(
            "failed to read Linux LED brightness for '{}' at '{}': {error}",
            led_name,
            path.display()
        )
    })?;
    let max_brightness = read_u32(&path.join("max_brightness")).map_err(|error| {
        format!(
            "failed to read Linux LED max brightness for '{}' at '{}': {error}",
            led_name,
            path.display()
        )
    })?;
    let trigger_text = read_trimmed(&path.join("trigger")).map_err(|error| {
        format!(
            "failed to read Linux LED triggers for '{}' at '{}': {error}",
            led_name,
            path.display()
        )
    })?;
    let driver = read_link_name(&path.join("device").join("driver")).map_err(|error| {
        format!(
            "failed to read Linux LED driver for '{}' at '{}': {error}",
            led_name,
            path.display()
        )
    })?;
    let device_name = read_link_name(&path.join("device")).map_err(|error| {
        format!(
            "failed to read Linux LED device link for '{}' at '{}': {error}",
            led_name,
            path.display()
        )
    })?;
    let (active_trigger, available_triggers) = parse_trigger_state(trigger_text.as_deref());

    let mut builder = DeviceDescriptor::builder_for_kind(
        format!("linux.gpio.led.{led_name}"),
        DeviceKind::Unspecified(InterfaceKind::Gpio),
    )
    .map_err(|error| format!("failed to start Linux LED descriptor '{led_name}': {error}"))?
    .display_name(led_name.to_string())
    .summary("Linux LED class device")
    .address(DeviceAddress::Custom {
        interface: InterfaceKind::Gpio,
        scheme: "linux-led-class".into(),
        value: led_name.into(),
    })
    .label("backend", "linux")
    .label("subsystem", "leds")
    .label("led_name", led_name.to_string())
    .control_surface(DeviceControlSurface::LinuxClass {
        root: path.display().to_string(),
    })
    .property("linux.subsystem", "leds")
    .property("linux.class_path", path.display().to_string())
    .property("led.name", led_name.to_string())
    .capability(led_capability("led.get_brightness", CapabilityAccess::READ))
    .capability(led_capability(
        "led.set_brightness",
        CapabilityAccess::WRITE,
    ))
    .capability(led_capability("led.get_trigger", CapabilityAccess::READ))
    .capability(led_capability(
        "led.set_trigger",
        CapabilityAccess::CONFIGURE,
    ));

    if let Some(brightness) = brightness {
        builder = builder.property("led.brightness", u64::from(brightness));
    } else {
        builder = builder.health(DeviceHealth::Degraded);
    }

    if let Some(max_brightness) = max_brightness {
        builder = builder.property("led.max_brightness", u64::from(max_brightness));
    } else {
        builder = builder.health(DeviceHealth::Degraded);
    }

    if let Some(active_trigger) = active_trigger {
        builder = builder.property("led.active_trigger", active_trigger);
    }

    if !available_triggers.is_empty() {
        builder = builder.property(
            "led.available_triggers",
            Value::from(
                available_triggers
                    .into_iter()
                    .map(Value::from)
                    .collect::<Vec<_>>(),
            ),
        );
    }

    if let Some(device_name) = device_name {
        builder = builder.property("linux.device", device_name);
    }

    if let Some(driver) = driver {
        builder = with_linux_driver(builder, driver);
    }

    builder
        .build()
        .map_err(|error| format!("failed to build Linux LED descriptor '{led_name}': {error}"))
}

fn parse_trigger_state(value: Option<&str>) -> (Option<String>, Vec<String>) {
    let Some(value) = value else {
        return (None, Vec::new());
    };

    let mut active = None;
    let mut available = Vec::new();

    for token in value.split_whitespace() {
        if let Some(name) = token
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        {
            active = Some(name.to_string());
            available.push(name.to_string());
        } else {
            available.push(token.to_string());
        }
    }

    (active, available)
}

fn led_capability(id: &str, access: CapabilityAccess) -> CapabilityDescriptor {
    CapabilityDescriptor::new(id, access).expect("static LED capability identifiers are valid")
}

fn probe_failed(probe: &str, path: &Path, action: &str, error: std::io::Error) -> DiscoveryError {
    DiscoveryError::ProbeFailed {
        probe: probe.to_string(),
        message: format!("{action} at '{}': {error}", path.display()),
    }
}
