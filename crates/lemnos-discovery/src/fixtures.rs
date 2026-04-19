use crate::{DiscoveryResult, InventoryDiff, InventorySnapshot};
use lemnos_core::{
    CapabilityDescriptor, CoreResult, DeviceAddress, DeviceDescriptor, DeviceDescriptorBuilder,
    DeviceHealth, DeviceKind, DeviceLink, TimestampMs, Value,
};

/// Reusable descriptor builder helpers for downstream inventory-facing tests.
pub struct DeviceFixtureBuilder {
    builder: DeviceDescriptorBuilder,
}

impl DeviceFixtureBuilder {
    pub fn new(id: impl Into<String>, kind: DeviceKind) -> CoreResult<Self> {
        Ok(Self {
            builder: DeviceDescriptor::builder_for_kind(id, kind)?,
        })
    }

    pub fn gpio_chip(
        id: impl Into<String>,
        chip_name: impl Into<String>,
        base_line: Option<u32>,
    ) -> CoreResult<Self> {
        let chip_name = chip_name.into();
        Ok(Self {
            builder: DeviceDescriptor::builder_for_kind(id, DeviceKind::GpioChip)?.address(
                DeviceAddress::GpioChip {
                    chip_name,
                    base_line,
                },
            ),
        })
    }

    pub fn gpio_line(
        id: impl Into<String>,
        chip_name: impl Into<String>,
        offset: u32,
    ) -> CoreResult<Self> {
        let chip_name = chip_name.into();
        Ok(Self {
            builder: DeviceDescriptor::builder_for_kind(id, DeviceKind::GpioLine)?
                .address(DeviceAddress::GpioLine { chip_name, offset }),
        })
    }

    pub fn pwm_chip(id: impl Into<String>, chip_name: impl Into<String>) -> CoreResult<Self> {
        let chip_name = chip_name.into();
        Ok(Self {
            builder: DeviceDescriptor::builder_for_kind(id, DeviceKind::PwmChip)?
                .address(DeviceAddress::PwmChip { chip_name }),
        })
    }

    pub fn pwm_channel(
        id: impl Into<String>,
        chip_name: impl Into<String>,
        channel: u32,
    ) -> CoreResult<Self> {
        let chip_name = chip_name.into();
        Ok(Self {
            builder: DeviceDescriptor::builder_for_kind(id, DeviceKind::PwmChannel)?
                .address(DeviceAddress::PwmChannel { chip_name, channel }),
        })
    }

    pub fn i2c_bus(id: impl Into<String>, bus: u32) -> CoreResult<Self> {
        Ok(Self {
            builder: DeviceDescriptor::builder_for_kind(id, DeviceKind::I2cBus)?
                .address(DeviceAddress::I2cBus { bus }),
        })
    }

    pub fn i2c_device(id: impl Into<String>, bus: u32, address: u16) -> CoreResult<Self> {
        Ok(Self {
            builder: DeviceDescriptor::builder_for_kind(id, DeviceKind::I2cDevice)?
                .address(DeviceAddress::I2cDevice { bus, address }),
        })
    }

    pub fn spi_bus(id: impl Into<String>, bus: u32) -> CoreResult<Self> {
        Ok(Self {
            builder: DeviceDescriptor::builder_for_kind(id, DeviceKind::SpiBus)?
                .address(DeviceAddress::SpiBus { bus }),
        })
    }

    pub fn spi_device(id: impl Into<String>, bus: u32, chip_select: u16) -> CoreResult<Self> {
        Ok(Self {
            builder: DeviceDescriptor::builder_for_kind(id, DeviceKind::SpiDevice)?
                .address(DeviceAddress::SpiDevice { bus, chip_select }),
        })
    }

    pub fn uart_port(id: impl Into<String>, port: impl Into<String>) -> CoreResult<Self> {
        Ok(Self {
            builder: DeviceDescriptor::builder_for_kind(id, DeviceKind::UartPort)?
                .address(DeviceAddress::UartPort { port: port.into() }),
        })
    }

    pub fn uart_device(
        id: impl Into<String>,
        port: impl Into<String>,
        unit: Option<String>,
    ) -> CoreResult<Self> {
        Ok(Self {
            builder: DeviceDescriptor::builder_for_kind(id, DeviceKind::UartDevice)?.address(
                DeviceAddress::UartDevice {
                    port: port.into(),
                    unit,
                },
            ),
        })
    }

    pub fn usb_bus(id: impl Into<String>, bus: u16) -> CoreResult<Self> {
        Ok(Self {
            builder: DeviceDescriptor::builder_for_kind(id, DeviceKind::UsbBus)?
                .address(DeviceAddress::UsbBus { bus }),
        })
    }

    pub fn usb_device(
        id: impl Into<String>,
        bus: u16,
        ports: Vec<u8>,
        vendor_id: Option<u16>,
        product_id: Option<u16>,
    ) -> CoreResult<Self> {
        Ok(Self {
            builder: DeviceDescriptor::builder_for_kind(id, DeviceKind::UsbDevice)?.address(
                DeviceAddress::UsbDevice {
                    bus,
                    ports,
                    vendor_id,
                    product_id,
                },
            ),
        })
    }

    pub fn usb_interface(
        id: impl Into<String>,
        bus: u16,
        ports: Vec<u8>,
        interface_number: u8,
        vendor_id: Option<u16>,
        product_id: Option<u16>,
    ) -> CoreResult<Self> {
        Ok(Self {
            builder: DeviceDescriptor::builder_for_kind(id, DeviceKind::UsbInterface)?.address(
                DeviceAddress::UsbInterface {
                    bus,
                    ports,
                    interface_number,
                    vendor_id,
                    product_id,
                },
            ),
        })
    }

    pub fn display_name(mut self, value: impl Into<String>) -> Self {
        self.builder = self.builder.display_name(value);
        self
    }

    pub fn summary(mut self, value: impl Into<String>) -> Self {
        self.builder = self.builder.summary(value);
        self
    }

    pub fn health(mut self, value: DeviceHealth) -> Self {
        self.builder = self.builder.health(value);
        self
    }

    pub fn driver_hint(mut self, value: impl Into<String>) -> Self {
        self.builder = self.builder.driver_hint(value);
        self
    }

    pub fn modalias(mut self, value: impl Into<String>) -> Self {
        self.builder = self.builder.modalias(value);
        self
    }

    pub fn compatible(mut self, value: impl Into<String>) -> Self {
        self.builder = self.builder.compatible(value);
        self
    }

    pub fn vendor(mut self, value: impl Into<String>) -> Self {
        self.builder = self.builder.vendor(value);
        self
    }

    pub fn model(mut self, value: impl Into<String>) -> Self {
        self.builder = self.builder.model(value);
        self
    }

    pub fn revision(mut self, value: impl Into<String>) -> Self {
        self.builder = self.builder.revision(value);
        self
    }

    pub fn serial_number(mut self, value: impl Into<String>) -> Self {
        self.builder = self.builder.serial_number(value);
        self
    }

    pub fn hardware_id(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.builder = self.builder.hardware_id(key, value);
        self
    }

    pub fn label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.builder = self.builder.label(key, value);
        self
    }

    pub fn property(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.builder = self.builder.property(key, value);
        self
    }

    pub fn capability(mut self, value: CapabilityDescriptor) -> Self {
        self.builder = self.builder.capability(value);
        self
    }

    pub fn link(mut self, value: DeviceLink) -> Self {
        self.builder = self.builder.link(value);
        self
    }

    pub fn build(self) -> CoreResult<DeviceDescriptor> {
        self.builder.build()
    }
}

#[derive(Default)]
pub struct InventoryFixtureBuilder {
    observed_at: Option<TimestampMs>,
    devices: Vec<DeviceDescriptor>,
}

impl InventoryFixtureBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_observed_at(mut self, observed_at: TimestampMs) -> Self {
        self.observed_at = Some(observed_at);
        self
    }

    pub fn with_device(mut self, device: DeviceDescriptor) -> Self {
        self.devices.push(device);
        self
    }

    pub fn with_fixture(mut self, fixture: DeviceFixtureBuilder) -> CoreResult<Self> {
        self.devices.push(fixture.build()?);
        Ok(self)
    }

    pub fn with_gpio_line(
        self,
        id: impl Into<String>,
        chip_name: impl Into<String>,
        offset: u32,
    ) -> CoreResult<Self> {
        self.with_fixture(DeviceFixtureBuilder::gpio_line(id, chip_name, offset)?)
    }

    pub fn with_i2c_device(
        self,
        id: impl Into<String>,
        bus: u32,
        address: u16,
    ) -> CoreResult<Self> {
        self.with_fixture(DeviceFixtureBuilder::i2c_device(id, bus, address)?)
    }

    pub fn with_spi_device(
        self,
        id: impl Into<String>,
        bus: u32,
        chip_select: u16,
    ) -> CoreResult<Self> {
        self.with_fixture(DeviceFixtureBuilder::spi_device(id, bus, chip_select)?)
    }

    pub fn with_uart_port(
        self,
        id: impl Into<String>,
        port: impl Into<String>,
    ) -> CoreResult<Self> {
        self.with_fixture(DeviceFixtureBuilder::uart_port(id, port)?)
    }

    pub fn with_usb_interface(
        self,
        id: impl Into<String>,
        bus: u16,
        ports: Vec<u8>,
        interface_number: u8,
        vendor_id: Option<u16>,
        product_id: Option<u16>,
    ) -> CoreResult<Self> {
        self.with_fixture(DeviceFixtureBuilder::usb_interface(
            id,
            bus,
            ports,
            interface_number,
            vendor_id,
            product_id,
        )?)
    }

    pub fn build(self) -> DiscoveryResult<InventorySnapshot> {
        InventorySnapshot::with_observed_at(self.devices, self.observed_at)
    }
}

pub struct InventoryDiffFixture {
    pub current: InventorySnapshot,
    pub next: InventorySnapshot,
    pub diff: InventoryDiff,
}

#[derive(Default)]
pub struct InventoryDiffFixtureBuilder {
    current: InventoryFixtureBuilder,
    next: InventoryFixtureBuilder,
}

impl InventoryDiffFixtureBuilder {
    pub fn new() -> Self {
        Self {
            current: InventoryFixtureBuilder::new(),
            next: InventoryFixtureBuilder::new(),
        }
    }

    pub fn current(mut self, current: InventoryFixtureBuilder) -> Self {
        self.current = current;
        self
    }

    pub fn next(mut self, next: InventoryFixtureBuilder) -> Self {
        self.next = next;
        self
    }

    pub fn build(self) -> DiscoveryResult<InventoryDiffFixture> {
        let current = self.current.build()?;
        let next = self.next.build()?;
        let diff = current.diff(&next);
        Ok(InventoryDiffFixture {
            current,
            next,
            diff,
        })
    }
}
