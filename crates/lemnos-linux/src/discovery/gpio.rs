use crate::LinuxPaths;
use crate::metadata::with_devnode;
use crate::util::{existing_path_string, file_name, read_dir_sorted, read_trimmed, read_u32};
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceDescriptor, DeviceHealth,
    DeviceKind, DeviceLink, DeviceRelation, InterfaceKind,
};
use lemnos_discovery::{DiscoveryContext, DiscoveryError, DiscoveryProbe, ProbeDiscovery};
use std::path::Path;

const INTERFACES: [InterfaceKind; 1] = [InterfaceKind::Gpio];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpioDiscoveryProbe {
    paths: LinuxPaths,
}

impl GpioDiscoveryProbe {
    pub fn new(paths: LinuxPaths) -> Self {
        Self { paths }
    }
}

impl DiscoveryProbe for GpioDiscoveryProbe {
    fn name(&self) -> &'static str {
        "linux-gpio"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &INTERFACES
    }

    fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
        let gpio_root = self.paths.gpio_class_root();
        let mut discovery = ProbeDiscovery::default();

        if !gpio_root.exists() {
            // No GPIO sysfs (`CONFIG_GPIO_SYSFS=n`): the character devices
            // still describe every chip and line.
            #[cfg(feature = "gpio-cdev")]
            {
                return Ok(self.discover_cdev(discovery));
            }
            #[cfg(not(feature = "gpio-cdev"))]
            {
                discovery.notes.push(format!(
                    "GPIO sysfs root '{}' is not present",
                    gpio_root.display()
                ));
                return Ok(discovery);
            }
        }

        let entries = read_dir_sorted(&gpio_root).map_err(|error| {
            probe_failed(self.name(), &gpio_root, "enumerate GPIO chips", error)
        })?;

        for chip_path in entries {
            let Some(chip_name) = file_name(&chip_path) else {
                continue;
            };
            if !chip_name.starts_with("gpiochip") {
                continue;
            }

            let chip_name = chip_name.to_string();
            let chip_data = load_chip_data(self.name(), &self.paths, &chip_path, &chip_name)?;

            let chip = match build_chip_descriptor(&chip_data) {
                Ok(device) => device,
                Err(note) => {
                    discovery.notes.push(note);
                    continue;
                }
            };
            let chip_id = chip.id.clone();
            discovery.devices.push(chip);

            for offset in 0..chip_data.line_count {
                match build_line_descriptor(&chip_data, &chip_id, offset, None) {
                    Ok(line) => discovery.devices.push(line),
                    Err(note) => discovery.notes.push(note),
                }
            }
        }

        Ok(discovery)
    }
}

impl GpioDiscoveryProbe {
    /// Chips and lines from `/dev/gpiochipN` (GPIO uAPI v2): chip name,
    /// label and line count, and each line's name and consumer.
    #[cfg(feature = "gpio-cdev")]
    fn discover_cdev(&self, mut discovery: ProbeDiscovery) -> ProbeDiscovery {
        use crate::hal::GpioChip;
        let chips = match GpioChip::paths_in(&self.paths.dev_root) {
            Ok(chips) => chips,
            Err(error) => {
                discovery.notes.push(format!(
                    "no GPIO sysfs and no GPIO character devices under '{}': {error}",
                    self.paths.dev_root.display()
                ));
                return discovery;
            }
        };
        for path in chips {
            let chip = match GpioChip::open(&path) {
                Ok(chip) => chip,
                Err(error) => {
                    discovery
                        .notes
                        .push(format!("cannot open '{}': {error}", path.display()));
                    continue;
                }
            };
            let info = chip.info().clone();
            let data = GpioChipData {
                chip_name: info.name.clone(),
                chip_path: None,
                devnode: Some(path.display().to_string()),
                label: Some(info.label.clone()).filter(|l| !l.is_empty()),
                base: None,
                line_count: info.lines,
            };
            let descriptor = match build_chip_descriptor(&data) {
                Ok(descriptor) => descriptor,
                Err(note) => {
                    discovery.notes.push(note);
                    continue;
                }
            };
            let chip_id = descriptor.id.clone();
            discovery.devices.push(descriptor);
            for offset in 0..data.line_count {
                let line = chip.line_info(offset).ok().map(|l| LineExtra {
                    name: l.name,
                    consumer: l.consumer,
                    used: l.used,
                });
                match build_line_descriptor(&data, &chip_id, offset, line.as_ref()) {
                    Ok(line) => discovery.devices.push(line),
                    Err(note) => discovery.notes.push(note),
                }
            }
        }
        discovery
    }
}

/// What the character device says about a line.
#[cfg_attr(not(feature = "gpio-cdev"), allow(dead_code))]
#[derive(Debug, Clone, Default)]
struct LineExtra {
    name: String,
    consumer: String,
    used: bool,
}

#[derive(Debug, Clone)]
struct GpioChipData {
    chip_name: String,
    /// The sysfs directory, when discovered through sysfs.
    chip_path: Option<String>,
    devnode: Option<String>,
    label: Option<String>,
    base: Option<u32>,
    line_count: u32,
}

fn load_chip_data(
    probe: &str,
    paths: &LinuxPaths,
    chip_path: &Path,
    chip_name: &str,
) -> Result<GpioChipData, DiscoveryError> {
    let label = read_trimmed(&chip_path.join("label"))
        .map_err(|error| probe_failed(probe, chip_path, "read GPIO label", error))?;
    let base = read_u32(&chip_path.join("base"))
        .map_err(|error| probe_failed(probe, chip_path, "read GPIO base", error))?;
    let line_count = read_u32(&chip_path.join("ngpio"))
        .map_err(|error| probe_failed(probe, chip_path, "read GPIO line count", error))?
        .unwrap_or(0);

    Ok(GpioChipData {
        chip_name: chip_name.to_string(),
        chip_path: Some(chip_path.display().to_string()),
        devnode: existing_path_string(&paths.gpio_devnode(chip_name)),
        label,
        base,
        line_count,
    })
}

fn build_chip_descriptor(data: &GpioChipData) -> Result<DeviceDescriptor, String> {
    let mut builder = DeviceDescriptor::builder_for_kind(
        format!("linux.gpio.chip.{}", data.chip_name),
        DeviceKind::GpioChip,
    )
    .map_err(|error| {
        format!(
            "failed to start GPIO chip descriptor '{}': {error}",
            data.chip_name
        )
    })?
    .display_name(data.label.clone().unwrap_or_else(|| data.chip_name.clone()))
    .summary("Linux GPIO controller")
    .address(DeviceAddress::GpioChip {
        chip_name: data.chip_name.clone(),
        base_line: data.base,
    })
    .label("backend", "linux")
    .label("chip_name", data.chip_name.clone())
    .property("line_count", u64::from(data.line_count));

    if let Some(chip_path) = &data.chip_path {
        builder = builder.property("sysfs_path", chip_path.clone());
    }

    if let Some(label) = &data.label {
        builder = builder
            .label("label", label.clone())
            .property("label", label.clone());
    }

    if let Some(base) = data.base {
        builder = builder.property("base", u64::from(base));
    }

    if let Some(devnode) = &data.devnode {
        builder = with_devnode(builder, devnode.clone());
    } else {
        builder = builder.health(DeviceHealth::Degraded);
    }

    builder.build().map_err(|error| {
        format!(
            "failed to build GPIO chip descriptor '{}': {error}",
            data.chip_name
        )
    })
}

fn build_line_descriptor(
    data: &GpioChipData,
    chip_id: &lemnos_core::DeviceId,
    offset: u32,
    line: Option<&LineExtra>,
) -> Result<DeviceDescriptor, String> {
    let mut builder = DeviceDescriptor::builder_for_kind(
        format!("linux.gpio.line.{}.{}", data.chip_name, offset),
        DeviceKind::GpioLine,
    )
    .map_err(|error| {
        format!(
            "failed to start GPIO line descriptor '{}:{}': {error}",
            data.chip_name, offset
        )
    })?
    .display_name(format!("{}:{offset}", data.chip_name))
    .summary("Linux GPIO line")
    .address(DeviceAddress::GpioLine {
        chip_name: data.chip_name.clone(),
        offset,
    })
    .driver_hint("lemnos.gpio.generic")
    .label("backend", "linux")
    .label("chip_name", data.chip_name.clone())
    .property("chip_name", data.chip_name.clone())
    .property("offset", u64::from(offset))
    .link(DeviceLink::new(chip_id.clone(), DeviceRelation::Parent))
    .capability(gpio_capability("gpio.read", CapabilityAccess::READ))
    .capability(gpio_capability("gpio.write", CapabilityAccess::WRITE))
    .capability(gpio_capability(
        "gpio.configure",
        CapabilityAccess::CONFIGURE,
    ))
    .capability(gpio_capability(
        "gpio.get_configuration",
        CapabilityAccess::READ,
    ));

    if let Some(base) = data.base.and_then(|base| base.checked_add(offset)) {
        builder = builder.property("global_line", u64::from(base));
    }

    if let Some(chip_path) = &data.chip_path {
        builder = builder.property("sysfs_path", chip_path.clone());
    }

    if let Some(line) = line {
        if !line.name.is_empty() {
            builder = builder
                .display_name(line.name.clone())
                .label("line_name", line.name.clone())
                .property("line_name", line.name.clone());
        }
        if !line.consumer.is_empty() {
            builder = builder.property("consumer", line.consumer.clone());
        }
        builder = builder.property("used", line.used);
    }

    if let Some(devnode) = &data.devnode {
        builder = with_devnode(builder, devnode.clone());
    } else {
        builder = builder.health(DeviceHealth::Degraded);
    }

    builder.build().map_err(|error| {
        format!(
            "failed to build GPIO line descriptor '{}:{}': {error}",
            data.chip_name, offset
        )
    })
}

fn gpio_capability(id: &str, access: CapabilityAccess) -> CapabilityDescriptor {
    CapabilityDescriptor::new(id, access).expect("static GPIO capability identifiers are valid")
}

fn probe_failed(probe: &str, path: &Path, action: &str, error: std::io::Error) -> DiscoveryError {
    DiscoveryError::ProbeFailed {
        probe: probe.to_string(),
        message: format!("{action} at '{}': {error}", path.display()),
    }
}
