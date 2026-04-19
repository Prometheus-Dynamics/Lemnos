use crate::LinuxPaths;
use crate::metadata::{with_devnode, with_driver};
use crate::util::{
    existing_path_string, file_name, parse_usb_bus_name, parse_usb_device_name,
    parse_usb_interface_name, read_dir_sorted, read_hex_u8, read_hex_u16, read_link_name,
    read_trimmed, read_u32,
};
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceDescriptor,
    DeviceDescriptorBuilder, DeviceHealth, DeviceId, DeviceKind, DeviceLink, DeviceRelation,
    InterfaceKind,
};
use lemnos_discovery::{DiscoveryContext, DiscoveryError, DiscoveryProbe, ProbeDiscovery};
use std::collections::BTreeMap;
use std::path::Path;

const INTERFACES: [InterfaceKind; 1] = [InterfaceKind::Usb];
type UsbVendorProduct = (Option<u16>, Option<u16>);

struct UsbInterfaceDescriptorInput<'a> {
    parent_device_id: &'a DeviceId,
    bus: u16,
    ports: &'a [u8],
    configuration_number: u8,
    parsed_interface_number: u8,
    vendor_id: Option<u16>,
    product_id: Option<u16>,
    path: &'a Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDiscoveryProbe {
    paths: LinuxPaths,
}

impl UsbDiscoveryProbe {
    pub fn new(paths: LinuxPaths) -> Self {
        Self { paths }
    }
}

impl DiscoveryProbe for UsbDiscoveryProbe {
    fn name(&self) -> &'static str {
        "linux-usb"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &INTERFACES
    }

    fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
        let devices_root = self.paths.usb_devices_root();
        let mut discovery = ProbeDiscovery::default();

        if !devices_root.exists() {
            discovery.notes.push(format!(
                "USB sysfs root '{}' is not present",
                devices_root.display()
            ));
            return Ok(discovery);
        }

        let entries = read_dir_sorted(&devices_root).map_err(|error| {
            probe_failed(self.name(), &devices_root, "enumerate USB devices", error)
        })?;
        let mut bus_ids: BTreeMap<u16, DeviceId> = BTreeMap::new();
        let mut device_ids: BTreeMap<String, DeviceId> = BTreeMap::new();
        let mut device_vendor_products: BTreeMap<String, UsbVendorProduct> = BTreeMap::new();

        for entry in entries {
            let Some(name) = file_name(&entry) else {
                continue;
            };

            if let Some(bus) = parse_usb_bus_name(name) {
                if bus_ids.contains_key(&bus) {
                    continue;
                }
                match build_bus_descriptor(bus, &entry) {
                    Ok(descriptor) => {
                        bus_ids.insert(bus, descriptor.id.clone());
                        discovery.devices.push(descriptor);
                    }
                    Err(note) => discovery.notes.push(note),
                }
                continue;
            }

            if let Some((bus, ports)) = parse_usb_device_name(name) {
                let bus_id = if let Some(bus_id) = bus_ids.get(&bus) {
                    bus_id.clone()
                } else {
                    match build_bus_descriptor_from_number(bus) {
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

                match build_device_descriptor(&self.paths, &bus_id, bus, &ports, &entry) {
                    Ok((descriptor, vendor_product)) => {
                        let key = usb_device_key(bus, &ports);
                        device_vendor_products.insert(key.clone(), vendor_product);
                        device_ids.insert(key, descriptor.id.clone());
                        discovery.devices.push(descriptor);
                    }
                    Err(note) => discovery.notes.push(note),
                }
                continue;
            }

            if let Some((bus, ports, configuration_number, interface_number)) =
                parse_usb_interface_name(name)
            {
                let key = usb_device_key(bus, &ports);
                let Some(parent_device_id) = device_ids.get(&key).cloned() else {
                    discovery.notes.push(format!(
                        "skipping Linux USB interface '{}' because parent device '{}' was not discovered",
                        name, key
                    ));
                    continue;
                };
                let vendor_product = device_vendor_products
                    .get(&key)
                    .copied()
                    .unwrap_or((None, None));
                match build_interface_descriptor(UsbInterfaceDescriptorInput {
                    parent_device_id: &parent_device_id,
                    bus,
                    ports: &ports,
                    configuration_number,
                    parsed_interface_number: interface_number,
                    vendor_id: vendor_product.0,
                    product_id: vendor_product.1,
                    path: &entry,
                }) {
                    Ok(descriptor) => discovery.devices.push(descriptor),
                    Err(note) => discovery.notes.push(note),
                }
            }
        }

        Ok(discovery)
    }
}

fn build_bus_descriptor_from_number(bus: u16) -> Result<DeviceDescriptor, String> {
    DeviceDescriptor::builder_for_kind(format!("linux.usb.bus.{bus}"), DeviceKind::UsbBus)
        .map_err(|error| format!("failed to start USB bus descriptor for bus {bus}: {error}"))?
        .display_name(format!("usb{bus}"))
        .summary("Linux USB bus")
        .address(DeviceAddress::UsbBus { bus })
        .label("backend", "linux")
        .label("bus", bus.to_string())
        .property("bus", u64::from(bus))
        .build()
        .map_err(|error| format!("failed to build USB bus descriptor for bus {bus}: {error}"))
}

fn build_bus_descriptor(bus: u16, path: &Path) -> Result<DeviceDescriptor, String> {
    let mut builder =
        DeviceDescriptor::builder_for_kind(format!("linux.usb.bus.{bus}"), DeviceKind::UsbBus)
            .map_err(|error| format!("failed to start USB bus descriptor for bus {bus}: {error}"))?
            .display_name(format!("usb{bus}"))
            .summary("Linux USB bus")
            .address(DeviceAddress::UsbBus { bus })
            .label("backend", "linux")
            .label("bus", bus.to_string())
            .property("bus", u64::from(bus))
            .property("sysfs_path", path.display().to_string());

    if let Some(product) = read_trimmed(&path.join("product")).map_err(|error| {
        format!(
            "failed to read Linux USB bus product for '{}' at '{}': {error}",
            bus,
            path.display()
        )
    })? {
        builder = builder.property("product", product);
    }

    builder
        .build()
        .map_err(|error| format!("failed to build USB bus descriptor for bus {bus}: {error}"))
}

fn build_device_descriptor(
    paths: &LinuxPaths,
    bus_id: &DeviceId,
    bus: u16,
    ports: &[u8],
    path: &Path,
) -> Result<(DeviceDescriptor, UsbVendorProduct), String> {
    let kernel_name = file_name(path)
        .ok_or_else(|| {
            format!(
                "missing file name for Linux USB device '{}'",
                path.display()
            )
        })?
        .to_string();
    let vendor_id = read_hex_u16(&path.join("idVendor")).map_err(|error| {
        format!(
            "failed to read Linux USB vendor ID for '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })?;
    let product_id = read_hex_u16(&path.join("idProduct")).map_err(|error| {
        format!(
            "failed to read Linux USB product ID for '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })?;
    let device_number = read_u32(&path.join("devnum")).map_err(|error| {
        format!(
            "failed to read Linux USB device number for '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })?;
    let modalias = read_trimmed(&path.join("modalias")).map_err(|error| {
        format!(
            "failed to read Linux USB modalias for '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })?;
    let manufacturer = read_trimmed(&path.join("manufacturer")).map_err(|error| {
        format!(
            "failed to read Linux USB manufacturer for '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })?;
    let product = read_trimmed(&path.join("product")).map_err(|error| {
        format!(
            "failed to read Linux USB product for '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })?;
    let serial_number = read_trimmed(&path.join("serial")).map_err(|error| {
        format!(
            "failed to read Linux USB serial for '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })?;

    let mut builder = with_usb_request_capabilities(
        DeviceDescriptor::builder_for_kind(
            format!("linux.usb.bus{bus}.ports{}", ports_label(ports)),
            DeviceKind::UsbDevice,
        )
        .map_err(|error| {
            format!(
                "failed to start USB device descriptor for bus {bus} ports {}: {error}",
                ports_display(ports)
            )
        })?
        .display_name(format!("usb-{bus}-{}", ports_display(ports)))
        .summary("Linux USB device")
        .address(DeviceAddress::UsbDevice {
            bus,
            ports: ports.to_vec(),
            vendor_id,
            product_id,
        })
        .driver_hint("lemnos.usb.generic")
        .label("backend", "linux")
        .label("bus", bus.to_string())
        .property("bus", u64::from(bus))
        .property("ports", ports_display(ports))
        .property("kernel_name", kernel_name.clone())
        .property("sysfs_path", path.display().to_string())
        .link(DeviceLink::new(bus_id.clone(), DeviceRelation::Parent)),
    );

    if let Some(vendor_id) = vendor_id {
        builder = builder
            .vendor(format!("{vendor_id:04x}"))
            .property("vendor_id", u64::from(vendor_id));
    }
    if let Some(product_id) = product_id {
        builder = builder
            .model(format!("{product_id:04x}"))
            .property("product_id", u64::from(product_id));
    }
    if let Some(modalias) = modalias {
        builder = builder
            .modalias(modalias.clone())
            .property("modalias", modalias);
    }
    if let Some(manufacturer) = manufacturer {
        builder = builder.property("manufacturer", manufacturer);
    }
    if let Some(product) = product {
        builder = builder.property("product", product);
    }
    if let Some(serial_number) = serial_number {
        builder = builder.serial_number(serial_number);
    }
    if let Some(device_number) = device_number {
        builder = builder.property("device_number", u64::from(device_number));
        if let Ok(device_number) = u16::try_from(device_number) {
            if let Some(devnode) = existing_path_string(&paths.usb_bus_devnode(bus, device_number))
            {
                builder = with_devnode(builder, devnode);
            } else {
                builder = builder.health(DeviceHealth::Degraded);
            }
        }
    } else {
        builder = builder.health(DeviceHealth::Degraded);
    }

    let descriptor = builder.build().map_err(|error| {
        format!("failed to build USB device descriptor '{kernel_name}': {error}")
    })?;
    Ok((descriptor, (vendor_id, product_id)))
}

fn build_interface_descriptor(
    input: UsbInterfaceDescriptorInput<'_>,
) -> Result<DeviceDescriptor, String> {
    let UsbInterfaceDescriptorInput {
        parent_device_id,
        bus,
        ports,
        configuration_number,
        parsed_interface_number,
        vendor_id,
        product_id,
        path,
    } = input;
    let kernel_name = file_name(path)
        .ok_or_else(|| {
            format!(
                "missing file name for Linux USB interface '{}'",
                path.display()
            )
        })?
        .to_string();
    let interface_number = read_hex_u8(&path.join("bInterfaceNumber")).map_err(|error| {
        format!(
            "failed to read Linux USB interface number for '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })?;
    let alternate_setting = read_hex_u8(&path.join("bAlternateSetting")).map_err(|error| {
        format!(
            "failed to read Linux USB alternate setting for '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })?;
    let class_code = read_hex_u8(&path.join("bInterfaceClass")).map_err(|error| {
        format!(
            "failed to read Linux USB interface class for '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })?;
    let subclass_code = read_hex_u8(&path.join("bInterfaceSubClass")).map_err(|error| {
        format!(
            "failed to read Linux USB interface subclass for '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })?;
    let protocol_code = read_hex_u8(&path.join("bInterfaceProtocol")).map_err(|error| {
        format!(
            "failed to read Linux USB interface protocol for '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })?;
    let driver = read_link_name(&path.join("driver")).map_err(|error| {
        format!(
            "failed to read Linux USB interface driver link for '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })?;
    let modalias = read_trimmed(&path.join("modalias")).map_err(|error| {
        format!(
            "failed to read Linux USB interface modalias for '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })?;

    let interface_number = interface_number.unwrap_or(parsed_interface_number);
    let mut builder = with_usb_request_capabilities(
        DeviceDescriptor::builder_for_kind(
            format!(
                "linux.usb.bus{bus}.ports{}.if{interface_number}",
                ports_label(ports)
            ),
            DeviceKind::UsbInterface,
        )
        .map_err(|error| {
            format!(
                "failed to start USB interface descriptor for '{}' at '{}': {error}",
                kernel_name,
                path.display()
            )
        })?
        .display_name(format!(
            "usb-{bus}-{}:if{interface_number}",
            ports_display(ports)
        ))
        .summary("Linux USB interface")
        .address(DeviceAddress::UsbInterface {
            bus,
            ports: ports.to_vec(),
            interface_number,
            vendor_id,
            product_id,
        })
        .driver_hint("lemnos.usb.generic")
        .label("backend", "linux")
        .label("bus", bus.to_string())
        .property("bus", u64::from(bus))
        .property("ports", ports_display(ports))
        .property("kernel_name", kernel_name.clone())
        .property("sysfs_path", path.display().to_string())
        .property("configuration_number", u64::from(configuration_number))
        .property("interface_number", u64::from(interface_number))
        .link(DeviceLink::new(
            parent_device_id.clone(),
            DeviceRelation::Parent,
        )),
    );

    if let Some(alternate_setting) = alternate_setting {
        builder = builder.property("alternate_setting", u64::from(alternate_setting));
    }
    if let Some(class_code) = class_code {
        builder = builder.property("class_code", u64::from(class_code));
    }
    if let Some(subclass_code) = subclass_code {
        builder = builder.property("subclass_code", u64::from(subclass_code));
    }
    if let Some(protocol_code) = protocol_code {
        builder = builder.property("protocol_code", u64::from(protocol_code));
    }
    if let Some(driver) = driver {
        builder = builder.label("driver", driver.clone());
        builder = with_driver(builder, driver);
    }
    if let Some(modalias) = modalias {
        builder = builder
            .modalias(modalias.clone())
            .property("modalias", modalias);
    }

    builder.build().map_err(|error| {
        format!(
            "failed to build USB interface descriptor '{}' at '{}': {error}",
            kernel_name,
            path.display()
        )
    })
}

fn usb_capability(id: &str, access: CapabilityAccess) -> CapabilityDescriptor {
    CapabilityDescriptor::new(id, access).expect("static USB capability identifiers are valid")
}

fn with_usb_request_capabilities(builder: DeviceDescriptorBuilder) -> DeviceDescriptorBuilder {
    builder
        .capability(usb_capability(
            "usb.control_transfer",
            CapabilityAccess::READ_WRITE,
        ))
        .capability(usb_capability("usb.bulk_read", CapabilityAccess::READ))
        .capability(usb_capability("usb.bulk_write", CapabilityAccess::WRITE))
        .capability(usb_capability("usb.interrupt_read", CapabilityAccess::READ))
        .capability(usb_capability(
            "usb.interrupt_write",
            CapabilityAccess::WRITE,
        ))
        .capability(usb_capability(
            "usb.claim_interface",
            CapabilityAccess::CONFIGURE,
        ))
        .capability(usb_capability(
            "usb.release_interface",
            CapabilityAccess::CONFIGURE,
        ))
}

fn usb_device_key(bus: u16, ports: &[u8]) -> String {
    format!("{bus}-{}", ports_display(ports))
}

fn ports_label(ports: &[u8]) -> String {
    ports
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join("_")
}

fn ports_display(ports: &[u8]) -> String {
    ports
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

fn probe_failed(probe: &str, path: &Path, action: &str, error: std::io::Error) -> DiscoveryError {
    DiscoveryError::ProbeFailed {
        probe: probe.to_string(),
        message: format!("{action} at '{}': {error}", path.display()),
    }
}
