use crate::LinuxPaths;
use crate::metadata::{with_devnode, with_driver};
use crate::util::{
    existing_path_string, file_name, parse_spi_device_name, read_dir_sorted, read_link_name,
    read_trimmed,
};
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceDescriptor, DeviceHealth,
    DeviceId, DeviceKind, DeviceLink, DeviceRelation, InterfaceKind,
};
use lemnos_discovery::{DiscoveryContext, DiscoveryError, DiscoveryProbe, ProbeDiscovery};
use std::collections::BTreeMap;
use std::path::Path;

const INTERFACES: [InterfaceKind; 1] = [InterfaceKind::Spi];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiDiscoveryProbe {
    paths: LinuxPaths,
}

impl SpiDiscoveryProbe {
    pub fn new(paths: LinuxPaths) -> Self {
        Self { paths }
    }
}

impl DiscoveryProbe for SpiDiscoveryProbe {
    fn name(&self) -> &'static str {
        "linux-spi"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &INTERFACES
    }

    fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
        let devices_root = self.paths.spi_devices_root();
        let mut discovery = ProbeDiscovery::default();

        if !devices_root.exists() {
            discovery.notes.push(format!(
                "SPI sysfs root '{}' is not present",
                devices_root.display()
            ));
            return Ok(discovery);
        }

        let entries = read_dir_sorted(&devices_root).map_err(|error| {
            probe_failed(self.name(), &devices_root, "enumerate SPI devices", error)
        })?;
        let mut bus_ids: BTreeMap<u32, DeviceId> = BTreeMap::new();

        for device_path in entries {
            let Some(device_name) = file_name(&device_path) else {
                continue;
            };
            let Some((bus, chip_select)) = parse_spi_device_name(device_name) else {
                continue;
            };

            let bus_id = if let Some(bus_id) = bus_ids.get(&bus) {
                bus_id.clone()
            } else {
                match build_bus_descriptor(bus) {
                    Ok(descriptor) => {
                        let bus_id = descriptor.id.clone();
                        bus_ids.insert(bus, bus_id.clone());
                        discovery.devices.push(descriptor);
                        bus_id
                    }
                    Err(note) => {
                        discovery.notes.push(note);
                        continue;
                    }
                }
            };

            match build_device_descriptor(&self.paths, &bus_id, bus, chip_select, &device_path) {
                Ok(descriptor) => discovery.devices.push(descriptor),
                Err(note) => discovery.notes.push(note),
            }
        }

        Ok(discovery)
    }
}

fn build_bus_descriptor(bus: u32) -> Result<DeviceDescriptor, String> {
    DeviceDescriptor::builder_for_kind(format!("linux.spi.bus.{bus}"), DeviceKind::SpiBus)
        .map_err(|error| format!("failed to start SPI bus descriptor for bus {bus}: {error}"))?
        .display_name(format!("spi-{bus}"))
        .summary("Linux SPI bus")
        .address(DeviceAddress::SpiBus { bus })
        .label("backend", "linux")
        .label("bus", bus.to_string())
        .property("bus", u64::from(bus))
        .build()
        .map_err(|error| format!("failed to build SPI bus descriptor for bus {bus}: {error}"))
}

fn build_device_descriptor(
    paths: &LinuxPaths,
    bus_id: &DeviceId,
    bus: u32,
    chip_select: u16,
    device_path: &Path,
) -> Result<DeviceDescriptor, String> {
    let kernel_name = file_name(device_path)
        .ok_or_else(|| {
            format!(
                "missing file name for Linux SPI device path '{}'",
                device_path.display()
            )
        })?
        .to_string();
    let modalias = read_trimmed(&device_path.join("modalias")).map_err(|error| {
        format!(
            "failed to read Linux SPI modalias for '{}' at '{}': {error}",
            kernel_name,
            device_path.display()
        )
    })?;
    let driver = read_link_name(&device_path.join("driver")).map_err(|error| {
        format!(
            "failed to read Linux SPI driver link for '{}' at '{}': {error}",
            kernel_name,
            device_path.display()
        )
    })?;
    let devnode = existing_path_string(&paths.spi_devnode(bus, chip_select));

    let mut builder = DeviceDescriptor::builder_for_kind(
        format!("linux.spi.bus{bus}.cs{chip_select}"),
        DeviceKind::SpiDevice,
    )
    .map_err(|error| {
        format!(
            "failed to start SPI device descriptor for bus {bus} chip-select {chip_select}: {error}"
        )
    })?
    .display_name(format!("spi-{bus}.{chip_select}"))
    .summary("Linux SPI device")
    .address(DeviceAddress::SpiDevice { bus, chip_select })
    .driver_hint("lemnos.spi.generic")
    .label("backend", "linux")
    .label("bus", bus.to_string())
    .property("bus", u64::from(bus))
    .property("chip_select", u64::from(chip_select))
    .property("kernel_name", kernel_name.clone())
    .property("sysfs_path", device_path.display().to_string())
    .link(DeviceLink::new(bus_id.clone(), DeviceRelation::Parent))
    .capability(spi_capability("spi.transfer", CapabilityAccess::READ_WRITE))
    .capability(spi_capability("spi.write", CapabilityAccess::WRITE))
    .capability(spi_capability("spi.configure", CapabilityAccess::CONFIGURE))
    .capability(spi_capability(
        "spi.get_configuration",
        CapabilityAccess::READ,
    ));

    if let Some(modalias) = modalias {
        builder = builder
            .modalias(modalias.clone())
            .property("modalias", modalias);
    }

    if let Some(driver) = driver {
        builder = builder.label("driver", driver.clone());
        builder = with_driver(builder, driver);
    }

    if let Some(devnode) = devnode {
        builder = with_devnode(builder, devnode);
    } else {
        builder = builder.health(DeviceHealth::Degraded);
    }

    builder
        .build()
        .map_err(|error| format!("failed to build SPI device descriptor '{kernel_name}': {error}"))
}

fn spi_capability(id: &str, access: CapabilityAccess) -> CapabilityDescriptor {
    CapabilityDescriptor::new(id, access).expect("static SPI capability identifiers are valid")
}

fn probe_failed(probe: &str, path: &Path, action: &str, error: std::io::Error) -> DiscoveryError {
    DiscoveryError::ProbeFailed {
        probe: probe.to_string(),
        message: format!("{action} at '{}': {error}", path.display()),
    }
}
