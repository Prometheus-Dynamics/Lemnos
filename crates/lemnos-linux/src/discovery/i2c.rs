use crate::LinuxPaths;
use crate::metadata::{with_devnode, with_driver};
use crate::util::{
    existing_path_string, file_name, parse_i2c_device_name, parse_prefixed_u32, read_dir_sorted,
    read_link_name, read_trimmed,
};
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceDescriptor, DeviceHealth,
    DeviceId, DeviceKind, DeviceLink, DeviceRelation, InterfaceKind,
};
use lemnos_discovery::{DiscoveryContext, DiscoveryError, DiscoveryProbe, ProbeDiscovery};
use std::collections::BTreeMap;
use std::path::Path;

const INTERFACES: [InterfaceKind; 1] = [InterfaceKind::I2c];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I2cDiscoveryProbe {
    paths: LinuxPaths,
}

impl I2cDiscoveryProbe {
    pub fn new(paths: LinuxPaths) -> Self {
        Self { paths }
    }
}

impl DiscoveryProbe for I2cDiscoveryProbe {
    fn name(&self) -> &'static str {
        "linux-i2c"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &INTERFACES
    }

    fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
        let class_root = self.paths.i2c_class_root();
        let devices_root = self.paths.i2c_devices_root();
        let mut discovery = ProbeDiscovery::default();

        if !class_root.exists() && !devices_root.exists() {
            discovery.notes.push(format!(
                "I2C sysfs roots '{}' and '{}' are not present",
                class_root.display(),
                devices_root.display()
            ));
            return Ok(discovery);
        }

        let mut bus_ids = BTreeMap::new();
        let class_entries = read_dir_sorted(&class_root).map_err(|error| {
            probe_failed(self.name(), &class_root, "enumerate I2C buses", error)
        })?;
        for bus_path in class_entries {
            let Some(bus_name) = file_name(&bus_path) else {
                continue;
            };
            let Some(bus) = parse_prefixed_u32(bus_name, "i2c-") else {
                continue;
            };
            match build_bus_descriptor(&self.paths, bus, Some(&bus_path)) {
                Ok(descriptor) => {
                    bus_ids.insert(bus, descriptor.id.clone());
                    discovery.devices.push(descriptor);
                }
                Err(note) => discovery.notes.push(note),
            }
        }

        let device_entries = read_dir_sorted(&devices_root).map_err(|error| {
            probe_failed(
                self.name(),
                &devices_root,
                "enumerate I2C bus devices",
                error,
            )
        })?;
        for device_path in device_entries {
            let Some(device_name) = file_name(&device_path) else {
                continue;
            };
            let Some((bus, address)) = parse_i2c_device_name(device_name) else {
                continue;
            };

            let bus_id = if let Some(bus_id) = bus_ids.get(&bus) {
                bus_id.clone()
            } else {
                match build_bus_descriptor(&self.paths, bus, None) {
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

            match build_device_descriptor(&self.paths, &bus_id, bus, address, &device_path) {
                Ok(descriptor) => discovery.devices.push(descriptor),
                Err(note) => discovery.notes.push(note),
            }
        }

        Ok(discovery)
    }
}

fn build_bus_descriptor(
    paths: &LinuxPaths,
    bus: u32,
    class_path: Option<&Path>,
) -> Result<DeviceDescriptor, String> {
    let bus_name = format!("i2c-{bus}");
    let class_path_string = class_path.map(|path| path.display().to_string());
    let adapter_name = if let Some(path) = class_path.map(|path| path.join("name")) {
        read_trimmed(&path).map_err(|error| {
            format!(
                "failed to read Linux I2C bus name for bus {bus} at '{}': {error}",
                path.display()
            )
        })?
    } else {
        None
    };
    let devnode = existing_path_string(&paths.i2c_devnode(bus));

    let mut builder =
        DeviceDescriptor::builder_for_kind(format!("linux.i2c.bus.{bus}"), DeviceKind::I2cBus)
            .map_err(|error| format!("failed to start I2C bus descriptor for bus {bus}: {error}"))?
            .display_name(bus_name.clone())
            .summary("Linux I2C bus")
            .address(DeviceAddress::I2cBus { bus })
            .label("backend", "linux")
            .label("bus", bus.to_string())
            .property("bus", u64::from(bus));

    if let Some(adapter_name) = adapter_name {
        builder = builder
            .label("adapter_name", adapter_name.clone())
            .property("adapter_name", adapter_name);
    }

    if let Some(class_path) = class_path_string {
        builder = builder.property("sysfs_path", class_path);
    }

    if let Some(devnode) = devnode {
        builder = with_devnode(builder, devnode);
    } else {
        builder = builder.health(DeviceHealth::Degraded);
    }

    builder
        .build()
        .map_err(|error| format!("failed to build I2C bus descriptor for bus {bus}: {error}"))
}

fn build_device_descriptor(
    paths: &LinuxPaths,
    bus_id: &DeviceId,
    bus: u32,
    address: u16,
    device_path: &Path,
) -> Result<DeviceDescriptor, String> {
    let kernel_name = file_name(device_path)
        .ok_or_else(|| {
            format!(
                "missing file name for Linux I2C device path '{}'",
                device_path.display()
            )
        })?
        .to_string();
    let device_name = read_trimmed(&device_path.join("name")).map_err(|error| {
        format!(
            "failed to read Linux I2C device name for '{}' at '{}': {error}",
            kernel_name,
            device_path.display()
        )
    })?;
    let modalias = read_trimmed(&device_path.join("modalias")).map_err(|error| {
        format!(
            "failed to read Linux I2C modalias for '{}' at '{}': {error}",
            kernel_name,
            device_path.display()
        )
    })?;
    let driver = read_link_name(&device_path.join("driver")).map_err(|error| {
        format!(
            "failed to read Linux I2C driver link for '{}' at '{}': {error}",
            kernel_name,
            device_path.display()
        )
    })?;
    let devnode = existing_path_string(&paths.i2c_devnode(bus));

    let mut builder = DeviceDescriptor::builder_for_kind(
        format!("linux.i2c.bus{bus}.0x{address:04x}"),
        DeviceKind::I2cDevice,
    )
    .map_err(|error| {
        format!(
            "failed to start I2C device descriptor for bus {bus} address 0x{address:04x}: {error}"
        )
    })?
    .display_name(format!("i2c-{bus}-0x{address:04x}"))
    .summary("Linux I2C device")
    .address(DeviceAddress::I2cDevice { bus, address })
    .driver_hint("lemnos.i2c.generic")
    .label("backend", "linux")
    .label("bus", bus.to_string())
    .property("bus", u64::from(bus))
    .property("address", u64::from(address))
    .property("kernel_name", kernel_name.clone())
    .property("sysfs_path", device_path.display().to_string())
    .link(DeviceLink::new(bus_id.clone(), DeviceRelation::Parent))
    .capability(i2c_capability("i2c.read", CapabilityAccess::READ))
    .capability(i2c_capability("i2c.write", CapabilityAccess::WRITE))
    .capability(i2c_capability(
        "i2c.write_read",
        CapabilityAccess::READ_WRITE,
    ))
    .capability(i2c_capability("i2c.transaction", CapabilityAccess::FULL));

    if let Some(device_name) = device_name {
        builder = builder
            .label("device_name", device_name.clone())
            .property("device_name", device_name);
    }

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
        .map_err(|error| format!("failed to build I2C device descriptor '{kernel_name}': {error}"))
}

fn i2c_capability(id: &str, access: CapabilityAccess) -> CapabilityDescriptor {
    CapabilityDescriptor::new(id, access).expect("static I2C capability identifiers are valid")
}

fn probe_failed(probe: &str, path: &Path, action: &str, error: std::io::Error) -> DiscoveryError {
    DiscoveryError::ProbeFailed {
        probe: probe.to_string(),
        message: format!("{action} at '{}': {error}", path.display()),
    }
}
