use crate::LinuxPaths;
use crate::metadata::{with_devnode, with_driver};
use crate::util::{existing_path_string, file_name, read_dir_sorted, read_link_name, read_trimmed};
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceDescriptor, DeviceHealth,
    DeviceKind, InterfaceKind,
};
use lemnos_discovery::{DiscoveryContext, DiscoveryError, DiscoveryProbe, ProbeDiscovery};
use std::path::Path;

const INTERFACES: [InterfaceKind; 1] = [InterfaceKind::Uart];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UartDiscoveryProbe {
    paths: LinuxPaths,
}

impl UartDiscoveryProbe {
    pub fn new(paths: LinuxPaths) -> Self {
        Self { paths }
    }
}

impl DiscoveryProbe for UartDiscoveryProbe {
    fn name(&self) -> &'static str {
        "linux-uart"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &INTERFACES
    }

    fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
        let tty_root = self.paths.tty_class_root();
        let mut discovery = ProbeDiscovery::default();

        if !tty_root.exists() {
            discovery.notes.push(format!(
                "TTY sysfs root '{}' is not present",
                tty_root.display()
            ));
            return Ok(discovery);
        }

        let entries = read_dir_sorted(&tty_root).map_err(|error| {
            probe_failed(self.name(), &tty_root, "enumerate TTY devices", error)
        })?;

        for tty_path in entries {
            let Some(port_name) = file_name(&tty_path) else {
                continue;
            };

            match build_port_descriptor(&self.paths, port_name, &tty_path) {
                Ok(Some(device)) => discovery.devices.push(device),
                Ok(None) => {}
                Err(note) => discovery.notes.push(note),
            }
        }

        Ok(discovery)
    }
}

fn build_port_descriptor(
    paths: &LinuxPaths,
    port_name: &str,
    tty_path: &Path,
) -> Result<Option<DeviceDescriptor>, String> {
    let devnode = existing_path_string(&paths.tty_devnode(port_name));
    let has_device = tty_path.join("device").exists();

    if devnode.is_none() && !has_device {
        return Ok(None);
    }

    let dev_major_minor = read_trimmed(&tty_path.join("dev")).map_err(|error| {
        format!(
            "failed to read Linux UART dev tuple for '{}' at '{}': {error}",
            port_name,
            tty_path.display()
        )
    })?;
    let modalias = read_trimmed(&tty_path.join("device").join("modalias")).map_err(|error| {
        format!(
            "failed to read Linux UART modalias for '{}' at '{}': {error}",
            port_name,
            tty_path.display()
        )
    })?;
    let driver = read_link_name(&tty_path.join("device").join("driver")).map_err(|error| {
        format!(
            "failed to read Linux UART driver link for '{}' at '{}': {error}",
            port_name,
            tty_path.display()
        )
    })?;

    let mut builder = DeviceDescriptor::builder_for_kind(
        format!("linux.uart.port.{port_name}"),
        DeviceKind::UartPort,
    )
    .map_err(|error| format!("failed to start UART descriptor for '{port_name}': {error}"))?
    .display_name(port_name.to_string())
    .summary("Linux UART port")
    .address(DeviceAddress::UartPort {
        port: port_name.to_string(),
    })
    .driver_hint("lemnos.uart.generic")
    .label("backend", "linux")
    .label("port", port_name.to_string())
    .property("port", port_name.to_string())
    .property("sysfs_path", tty_path.display().to_string())
    .capability(uart_capability("uart.read", CapabilityAccess::READ))
    .capability(uart_capability("uart.write", CapabilityAccess::WRITE))
    .capability(uart_capability(
        "uart.configure",
        CapabilityAccess::CONFIGURE,
    ))
    .capability(uart_capability("uart.flush", CapabilityAccess::WRITE))
    .capability(uart_capability(
        "uart.get_configuration",
        CapabilityAccess::READ,
    ));

    if let Some(dev_major_minor) = dev_major_minor {
        builder = builder.property("dev", dev_major_minor);
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
        .map(Some)
        .map_err(|error| format!("failed to build UART descriptor for '{port_name}': {error}"))
}

fn uart_capability(id: &str, access: CapabilityAccess) -> CapabilityDescriptor {
    CapabilityDescriptor::new(id, access).expect("static UART capability identifiers are valid")
}

fn probe_failed(probe: &str, path: &Path, action: &str, error: std::io::Error) -> DiscoveryError {
    DiscoveryError::ProbeFailed {
        probe: probe.to_string(),
        message: format!("{action} at '{}': {error}", path.display()),
    }
}
