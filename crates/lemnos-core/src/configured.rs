use crate::{
    CapabilityAccess, CapabilityDescriptor, CoreResult, DeviceAddress, DeviceDescriptor,
    DeviceDescriptorBuilder, DeviceId, DeviceKind, DeviceLink, DeviceRelation, GpioEdge,
};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

pub trait ConfiguredDeviceModel {
    fn configured_interfaces() -> &'static [crate::InterfaceKind]
    where
        Self: Sized;

    fn configured_descriptors(&self) -> CoreResult<Vec<DeviceDescriptor>>;
}

const LABEL_CONFIGURED_OWNER: &str = "configured_owner";
const LABEL_ENDPOINT: &str = "endpoint";
const LABEL_SIGNAL: &str = "signal";
const LINK_ATTR_NAME: &str = "name";
const LINK_ATTR_INTERFACE: &str = "interface";
const INTERFACE_GPIO: &str = "gpio";
const INTERFACE_I2C: &str = "i2c";
const INTERFACE_SPI: &str = "spi";

const I2C_CONFIGURED_CAPABILITIES: &[(&str, CapabilityAccess)] = &[
    ("i2c.read", CapabilityAccess::READ),
    ("i2c.write", CapabilityAccess::WRITE),
    ("i2c.write_read", CapabilityAccess::READ_WRITE),
    ("i2c.transaction", CapabilityAccess::FULL),
];

const SPI_CONFIGURED_CAPABILITIES: &[(&str, CapabilityAccess)] = &[
    ("spi.transfer", CapabilityAccess::READ_WRITE),
    ("spi.write", CapabilityAccess::WRITE),
    ("spi.configure", CapabilityAccess::CONFIGURE),
    ("spi.get_configuration", CapabilityAccess::READ),
];

const GPIO_SIGNAL_CAPABILITIES: &[(&str, CapabilityAccess)] = &[
    ("gpio.read", CapabilityAccess::READ),
    ("gpio.get_configuration", CapabilityAccess::READ),
    ("gpio.configure", CapabilityAccess::CONFIGURE),
];

fn configured_child_builder(
    owner: &DeviceDescriptor,
    id: impl Into<String>,
    kind: DeviceKind,
    summary: impl Into<String>,
    name: &str,
    label_key: &'static str,
    interface_attr: &'static str,
) -> CoreResult<DeviceDescriptorBuilder> {
    Ok(DeviceDescriptor::builder_for_kind(id, kind)?
        .summary(summary)
        .label(LABEL_CONFIGURED_OWNER, owner.id.as_str())
        .label(label_key, name.to_owned())
        .link(
            DeviceLink::new(owner.id.clone(), DeviceRelation::Parent)
                .with_attribute(LINK_ATTR_NAME, name.to_owned())
                .with_attribute(LINK_ATTR_INTERFACE, interface_attr),
        ))
}

fn configured_builder_with_owner_name(
    mut builder: DeviceDescriptorBuilder,
    owner: &DeviceDescriptor,
    name: &str,
) -> DeviceDescriptorBuilder {
    if let Some(owner_name) = &owner.display_name {
        builder = builder.display_name(format!("{owner_name} {name}"));
    }
    builder
}

fn configured_builder_with_capabilities(
    builder: DeviceDescriptorBuilder,
    capabilities: &[(&str, CapabilityAccess)],
) -> DeviceDescriptorBuilder {
    capabilities.iter().fold(builder, |builder, (id, access)| {
        builder.capability(
            CapabilityDescriptor::new(*id, *access)
                .expect("configured capability identifiers are static and valid"),
        )
    })
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredI2cEndpoint {
    pub name: String,
    pub bus: u32,
    pub address: u16,
}

impl ConfiguredI2cEndpoint {
    pub fn new(name: impl Into<String>, bus: u32, address: u16) -> Self {
        Self {
            name: name.into(),
            bus,
            address,
        }
    }

    pub fn descriptor_id_for_owner(&self, owner_id: &DeviceId) -> String {
        format!("{}.endpoint.{}", owner_id.as_str(), self.name)
    }

    pub fn descriptor_for_owner(
        &self,
        owner: &DeviceDescriptor,
        driver_hint: Option<&str>,
    ) -> CoreResult<DeviceDescriptor> {
        let mut builder = configured_builder_with_capabilities(
            configured_child_builder(
                owner,
                self.descriptor_id_for_owner(&owner.id),
                DeviceKind::I2cDevice,
                format!("Configured I2C endpoint '{}'", self.name),
                &self.name,
                LABEL_ENDPOINT,
                INTERFACE_I2C,
            )?
            .address(DeviceAddress::I2cDevice {
                bus: self.bus,
                address: self.address,
            })
            .property("bus", u64::from(self.bus))
            .property("address", u64::from(self.address)),
            I2C_CONFIGURED_CAPABILITIES,
        );

        builder = configured_builder_with_owner_name(builder, owner, &self.name);

        if let Some(driver_hint) = driver_hint {
            builder = builder.driver_hint(driver_hint);
        }

        builder.build()
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredSpiEndpoint {
    pub name: String,
    pub bus: u32,
    pub chip_select: u16,
}

impl ConfiguredSpiEndpoint {
    pub fn new(name: impl Into<String>, bus: u32, chip_select: u16) -> Self {
        Self {
            name: name.into(),
            bus,
            chip_select,
        }
    }

    pub fn descriptor_id_for_owner(&self, owner_id: &DeviceId) -> String {
        format!("{}.endpoint.{}", owner_id.as_str(), self.name)
    }

    pub fn descriptor_for_owner(
        &self,
        owner: &DeviceDescriptor,
        driver_hint: Option<&str>,
    ) -> CoreResult<DeviceDescriptor> {
        let mut builder = configured_builder_with_capabilities(
            configured_child_builder(
                owner,
                self.descriptor_id_for_owner(&owner.id),
                DeviceKind::SpiDevice,
                format!("Configured SPI endpoint '{}'", self.name),
                &self.name,
                LABEL_ENDPOINT,
                INTERFACE_SPI,
            )?
            .address(DeviceAddress::SpiDevice {
                bus: self.bus,
                chip_select: self.chip_select,
            })
            .property("bus", u64::from(self.bus))
            .property("chip_select", u64::from(self.chip_select)),
            SPI_CONFIGURED_CAPABILITIES,
        );

        builder = configured_builder_with_owner_name(builder, owner, &self.name);

        if let Some(driver_hint) = driver_hint {
            builder = builder.driver_hint(driver_hint);
        }

        builder.build()
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfiguredGpioSignalTarget {
    Device(DeviceId),
    GlobalLine(u32),
    ChipLine {
        chip_name: String,
        offset: u32,
        global_line: Option<u32>,
        devnode: Option<String>,
    },
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredGpioSignal {
    pub target: ConfiguredGpioSignalTarget,
    pub edge: Option<GpioEdge>,
    pub active_low: bool,
    pub required: bool,
}

impl ConfiguredGpioSignal {
    pub fn by_device_id(id: impl Into<String>) -> CoreResult<Self> {
        Ok(Self {
            target: ConfiguredGpioSignalTarget::Device(DeviceId::new(id)?),
            edge: None,
            active_low: false,
            required: false,
        })
    }

    pub fn by_global_line(global_line: u32) -> Self {
        Self {
            target: ConfiguredGpioSignalTarget::GlobalLine(global_line),
            edge: None,
            active_low: false,
            required: false,
        }
    }

    pub fn by_chip_line(chip_name: impl Into<String>, offset: u32) -> Self {
        Self {
            target: ConfiguredGpioSignalTarget::ChipLine {
                chip_name: chip_name.into(),
                offset,
                global_line: None,
                devnode: None,
            },
            edge: None,
            active_low: false,
            required: false,
        }
    }

    pub fn with_global_line(mut self, global_line: u32) -> Self {
        if let ConfiguredGpioSignalTarget::ChipLine {
            global_line: stored,
            ..
        } = &mut self.target
        {
            *stored = Some(global_line);
        }
        self
    }

    pub fn with_devnode(mut self, devnode: impl Into<String>) -> Self {
        if let ConfiguredGpioSignalTarget::ChipLine {
            devnode: stored, ..
        } = &mut self.target
        {
            *stored = Some(devnode.into());
        }
        self
    }

    pub fn with_edge(mut self, edge: GpioEdge) -> Self {
        self.edge = Some(edge);
        self
    }

    pub fn with_active_low(mut self, active_low: bool) -> Self {
        self.active_low = active_low;
        self
    }

    pub fn with_required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredGpioSignalBinding {
    pub name: String,
    pub signal: ConfiguredGpioSignal,
}

impl ConfiguredGpioSignalBinding {
    pub fn new(name: impl Into<String>, signal: ConfiguredGpioSignal) -> Self {
        Self {
            name: name.into(),
            signal,
        }
    }

    pub fn descriptor_id_for_owner(&self, owner_id: &DeviceId) -> String {
        format!("{}.signal.{}", owner_id.as_str(), self.name)
    }

    pub fn link_target_id(&self, owner_id: &DeviceId) -> CoreResult<DeviceId> {
        match &self.signal.target {
            ConfiguredGpioSignalTarget::Device(id) => Ok(id.clone()),
            ConfiguredGpioSignalTarget::GlobalLine(_)
            | ConfiguredGpioSignalTarget::ChipLine { .. } => {
                DeviceId::new(self.descriptor_id_for_owner(owner_id))
            }
        }
    }

    pub fn descriptor_for_owner(
        &self,
        owner: &DeviceDescriptor,
    ) -> CoreResult<Option<DeviceDescriptor>> {
        let target = &self.signal.target;
        if matches!(target, ConfiguredGpioSignalTarget::Device(_)) {
            return Ok(None);
        }

        let mut builder = configured_builder_with_capabilities(
            configured_child_builder(
                owner,
                self.descriptor_id_for_owner(&owner.id),
                DeviceKind::GpioLine,
                format!("Configured GPIO signal '{}'", self.name),
                &self.name,
                LABEL_SIGNAL,
                INTERFACE_GPIO,
            )?
            .property("required", self.signal.required)
            .property("active_low", self.signal.active_low),
            GPIO_SIGNAL_CAPABILITIES,
        );

        builder = configured_builder_with_owner_name(builder, owner, &self.name);

        if let Some(edge) = self.signal.edge {
            builder = builder.property("edge", gpio_edge_name(edge));
        }

        match target {
            ConfiguredGpioSignalTarget::Device(_) => {}
            ConfiguredGpioSignalTarget::GlobalLine(global_line) => {
                builder = builder.property("global_line", u64::from(*global_line));
            }
            ConfiguredGpioSignalTarget::ChipLine {
                chip_name,
                offset,
                global_line,
                devnode,
            } => {
                builder = builder
                    .address(DeviceAddress::GpioLine {
                        chip_name: chip_name.clone(),
                        offset: *offset,
                    })
                    .property("offset", u64::from(*offset));

                if let Some(global_line) = global_line {
                    builder = builder.property("global_line", u64::from(*global_line));
                }

                if let Some(devnode) = devnode {
                    builder = builder.property("devnode", devnode.clone());
                }
            }
        }

        builder.build().map(Some)
    }
}

const fn gpio_edge_name(edge: GpioEdge) -> &'static str {
    match edge {
        GpioEdge::Rising => "rising",
        GpioEdge::Falling => "falling",
        GpioEdge::Both => "both",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InterfaceKind, Value};

    #[test]
    fn configured_i2c_endpoint_builds_child_descriptor() {
        let owner =
            DeviceDescriptor::builder("helios.bmi088.bus4.accel0x18.gyro0x68", InterfaceKind::I2c)
                .expect("owner builder")
                .display_name("board imu")
                .build()
                .expect("owner descriptor");

        let endpoint = ConfiguredI2cEndpoint::new("accel", 4, 0x18);
        let descriptor = endpoint
            .descriptor_for_owner(&owner, Some("helios.sensor.bmi088"))
            .expect("endpoint descriptor");

        assert_eq!(
            descriptor.address,
            Some(DeviceAddress::I2cDevice {
                bus: 4,
                address: 0x18
            })
        );
        assert_eq!(descriptor.display_name.as_deref(), Some("board imu accel"));
        assert_eq!(
            descriptor.match_hints.driver_hint.as_deref(),
            Some("helios.sensor.bmi088")
        );
    }

    #[test]
    fn configured_gpio_signal_builds_synthetic_descriptor_when_needed() {
        let owner =
            DeviceDescriptor::builder("helios.bmi088.bus4.accel0x18.gyro0x68", InterfaceKind::I2c)
                .expect("owner builder")
                .display_name("board imu")
                .build()
                .expect("owner descriptor");

        let binding = ConfiguredGpioSignalBinding::new(
            "accel_int",
            ConfiguredGpioSignal::by_chip_line("gpiochip4", 23)
                .with_global_line(311)
                .with_edge(GpioEdge::Rising)
                .with_required(true),
        );

        let descriptor = binding
            .descriptor_for_owner(&owner)
            .expect("signal descriptor result")
            .expect("synthetic signal descriptor");

        assert_eq!(descriptor.kind, DeviceKind::GpioLine);
        assert_eq!(
            descriptor.address,
            Some(DeviceAddress::GpioLine {
                chip_name: "gpiochip4".into(),
                offset: 23
            })
        );
        assert_eq!(
            descriptor.properties.get("global_line"),
            Some(&Value::from(311_u64))
        );
        assert_eq!(
            descriptor.properties.get("edge"),
            Some(&Value::from("rising"))
        );
    }

    #[test]
    fn configured_gpio_signal_can_link_to_existing_device() {
        let owner_id = DeviceId::new("helios.bmi088.bus4.accel0x18.gyro0x68").expect("owner id");
        let binding = ConfiguredGpioSignalBinding::new(
            "gyro_int",
            ConfiguredGpioSignal::by_device_id("linux.gpio.line.gpiochip4.24")
                .expect("direct signal"),
        );

        let target = binding
            .link_target_id(&owner_id)
            .expect("existing target id");
        assert_eq!(target.as_str(), "linux.gpio.line.gpiochip4.24");
    }

    #[test]
    fn configured_spi_endpoint_builds_child_descriptor() {
        let owner = DeviceDescriptor::builder("configured.flash.bus2.cs0", InterfaceKind::Spi)
            .expect("owner builder")
            .display_name("board flash")
            .build()
            .expect("owner descriptor");

        let endpoint = ConfiguredSpiEndpoint::new("flash", 2, 0);
        let descriptor = endpoint
            .descriptor_for_owner(&owner, Some("example.flash"))
            .expect("endpoint descriptor");

        assert_eq!(
            descriptor.address,
            Some(DeviceAddress::SpiDevice {
                bus: 2,
                chip_select: 0
            })
        );
        assert_eq!(
            descriptor.display_name.as_deref(),
            Some("board flash flash")
        );
        assert_eq!(
            descriptor.match_hints.driver_hint.as_deref(),
            Some("example.flash")
        );
    }
}
