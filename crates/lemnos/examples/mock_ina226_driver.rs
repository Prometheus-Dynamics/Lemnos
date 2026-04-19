#![allow(clippy::print_stdout)]

use lemnos::core::{
    CustomInteractionResponse, DeviceDescriptor, DeviceKind, DeviceLifecycleState,
    DeviceStateSnapshot, InteractionRequest, InteractionResponse, InterfaceKind, OperationRecord,
    OperationStatus, Value, ValueMap,
};
use lemnos::driver::{
    BoundDevice, CustomInteraction, Driver, DriverBindContext, DriverError, DriverManifest,
    DriverMatch, DriverMatchLevel, DriverPriority, DriverResult, I2cControllerIo,
    I2cControllerSession, SessionAccess, interaction_name,
};
use lemnos::macros::ConfiguredDevice;
use lemnos::mock::{MockHardware, MockI2cDevice};
use lemnos::prelude::*;
use std::borrow::Cow;
use std::error::Error;

const SAMPLE_INTERACTION: &str = "sensor.power.sample";

#[derive(Debug, Clone, PartialEq, ConfiguredDevice)]
#[lemnos(
    interface = I2c,
    id = "example.ina226",
    driver = "example.sensor.ina226",
    summary = "Configured INA226 power monitor"
)]
struct Ina226Config {
    #[lemnos(bus(i2c))]
    bus: u32,
    #[lemnos(endpoint(i2c, name = "sensor"))]
    address: u16,
    #[lemnos(display_name, label)]
    label: String,
    #[lemnos(property = "shunt_resistance_ohms")]
    shunt_resistance_ohms: f64,
    #[lemnos(property = "max_current_a")]
    max_current_a: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct Ina226Binding {
    logical_device_id: String,
    bus: u32,
    address: u16,
    label: String,
    shunt_resistance_ohms: f64,
    max_current_a: f64,
}

impl Ina226Binding {
    fn new(
        logical_device_id: impl Into<String>,
        bus: u32,
        address: u16,
        label: impl Into<String>,
        shunt_resistance_ohms: f64,
        max_current_a: f64,
    ) -> Self {
        Self {
            logical_device_id: logical_device_id.into(),
            bus,
            address,
            label: label.into(),
            shunt_resistance_ohms,
            max_current_a,
        }
    }

    fn current_lsb(&self) -> f64 {
        self.max_current_a / 32_768.0
    }

    fn calibration(&self) -> u16 {
        (0.00512_f64 / (self.current_lsb() * self.shunt_resistance_ohms)).round() as u16
    }

    fn apply_state_config(&self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_config("label", self.label.clone())
            .with_config("bus", u64::from(self.bus))
            .with_config("address", u64::from(self.address))
            .with_config("address_hex", format!("0x{:02x}", self.address))
            .with_config("shunt_resistance_ohms", self.shunt_resistance_ohms)
            .with_config("max_current_a", self.max_current_a)
    }
}

impl From<&Ina226Config> for Ina226Binding {
    fn from(value: &Ina226Config) -> Self {
        Self::new(
            value.configured_device_id(),
            value.bus,
            value.address,
            value.label.clone(),
            value.shunt_resistance_ohms,
            value.max_current_a,
        )
    }
}

struct ExampleIna226Driver {
    bindings: Vec<Ina226Binding>,
}

impl ExampleIna226Driver {
    const DRIVER_ID: &str = "example.sensor.ina226";

    fn new(bindings: impl IntoIterator<Item = Ina226Binding>) -> Self {
        Self {
            bindings: bindings.into_iter().collect(),
        }
    }

    fn binding_for(&self, device: &DeviceDescriptor) -> Option<&Ina226Binding> {
        self.bindings
            .iter()
            .find(|binding| binding.logical_device_id == device.id.as_str())
    }

    fn binding_for_device(&self, device: &DeviceDescriptor) -> DriverResult<&Ina226Binding> {
        self.binding_for(device)
            .ok_or_else(|| DriverError::BindRejected {
                driver_id: self.id().to_string(),
                device_id: device.id.clone(),
                reason: "device is not configured for this INA226 driver".into(),
            })
    }
}

impl Driver for ExampleIna226Driver {
    fn id(&self) -> &str {
        Self::DRIVER_ID
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::I2c
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(
                self.id(),
                "Configured INA226 example driver",
                vec![InterfaceKind::I2c],
            )
            .with_description(
                "Example out-of-tree style INA226 driver backed by a controller-scoped I2C session.",
            )
            .with_priority(DriverPriority::Exact)
            .with_kind(DeviceKind::Unspecified(InterfaceKind::I2c))
            .with_custom_interaction(
                SAMPLE_INTERACTION,
                "Read bus voltage, shunt voltage, current, and power",
            )
            .with_tag("sensor")
            .with_tag("power")
            .with_tag("ina226")
            .with_tag("example"),
        )
    }

    fn matches(&self, device: &DeviceDescriptor) -> DriverMatch {
        let base: DriverMatch = self.manifest_ref().match_device(device).into();
        if !base.is_supported() {
            return base;
        }

        let Some(binding) = self.binding_for(device) else {
            return DriverMatch::unsupported(
                "device is not listed in the configured INA226 binding set",
            );
        };

        DriverMatch {
            level: DriverMatchLevel::Exact,
            score: base.score + 400,
            reasons: vec![format!(
                "configured INA226 '{}' matched logical device '{}'",
                binding.label, binding.logical_device_id
            )],
            matched_rule: base.matched_rule,
        }
    }

    fn bind(
        &self,
        device: &DeviceDescriptor,
        context: &DriverBindContext<'_>,
    ) -> DriverResult<Box<dyn BoundDevice>> {
        let binding = self.binding_for_device(device)?.clone();
        let mut controller = context.open_i2c_controller(
            self.id(),
            device,
            binding.bus,
            SessionAccess::ExclusiveController,
        )?;
        {
            let mut io = Ina226Io::new(
                self.id(),
                device.id.clone(),
                &mut *controller,
                binding.address,
                binding.current_lsb(),
            );
            io.configure(&binding)?;
        }

        let interaction = CustomInteraction::new(
            SAMPLE_INTERACTION,
            "Read INA226 bus voltage, shunt voltage, current, and power",
        )
        .map_err(|source| DriverError::BindFailed {
            driver_id: self.id().to_string(),
            device_id: device.id.clone(),
            reason: source.to_string(),
        })?;

        Ok(Box::new(ExampleIna226BoundDevice {
            driver_id: self.id().to_string(),
            device: device.clone(),
            controller,
            binding,
            interactions: vec![interaction],
        }))
    }
}

struct ExampleIna226BoundDevice {
    driver_id: String,
    device: DeviceDescriptor,
    controller: Box<dyn I2cControllerSession>,
    binding: Ina226Binding,
    interactions: Vec<CustomInteraction>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Ina226Sample {
    bus_voltage_v: f64,
    shunt_voltage_v: f64,
    current_a: f64,
    power_w: f64,
}

impl Ina226Sample {
    fn into_value(self, label: &str) -> Value {
        let mut map = ValueMap::new();
        map.insert("label".into(), Value::from(label));
        map.insert("bus_voltage_v".into(), Value::from(self.bus_voltage_v));
        map.insert("shunt_voltage_v".into(), Value::from(self.shunt_voltage_v));
        map.insert("current_a".into(), Value::from(self.current_a));
        map.insert("power_w".into(), Value::from(self.power_w));
        Value::from(map)
    }

    fn apply_telemetry(self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_telemetry("bus_voltage_v", self.bus_voltage_v)
            .with_telemetry("shunt_voltage_v", self.shunt_voltage_v)
            .with_telemetry("current_a", self.current_a)
            .with_telemetry("power_w", self.power_w)
    }
}

struct Ina226Io<'a> {
    driver_id: &'a str,
    device_id: lemnos::core::DeviceId,
    controller: &'a mut dyn I2cControllerSession,
    address: u16,
    current_lsb: f64,
}

impl<'a> Ina226Io<'a> {
    const CONFIG_REGISTER: u8 = 0x00;
    const SHUNT_VOLTAGE_REGISTER: u8 = 0x01;
    const BUS_VOLTAGE_REGISTER: u8 = 0x02;
    const POWER_REGISTER: u8 = 0x03;
    const CURRENT_REGISTER: u8 = 0x04;
    const CALIBRATION_REGISTER: u8 = 0x05;
    const DEFAULT_CONFIGURATION: u16 = 0x4527;

    fn new(
        driver_id: &'a str,
        device_id: lemnos::core::DeviceId,
        controller: &'a mut dyn I2cControllerSession,
        address: u16,
        current_lsb: f64,
    ) -> Self {
        Self {
            driver_id,
            device_id,
            controller,
            address,
            current_lsb,
        }
    }

    fn with_sensor<T>(
        &mut self,
        action: impl FnOnce(&mut lemnos::driver::I2cControllerTarget<'_>) -> DriverResult<T>,
    ) -> DriverResult<T> {
        let mut bus = I2cControllerIo::with_device_id(
            &mut *self.controller,
            self.driver_id,
            self.device_id.clone(),
        );
        let mut sensor = bus.target(self.address);
        action(&mut sensor)
    }

    fn configure(&mut self, binding: &Ina226Binding) -> DriverResult<()> {
        let calibration = binding.calibration();
        self.with_sensor(|sensor| {
            sensor.write_u16_be(Self::CALIBRATION_REGISTER, calibration)?;
            sensor.write_u16_be(Self::CONFIG_REGISTER, Self::DEFAULT_CONFIGURATION)?;
            Ok(())
        })
    }

    fn sample(&mut self) -> DriverResult<Ina226Sample> {
        let (shunt_raw, bus_raw, power_raw, current_raw) = self.with_sensor(|sensor| {
            Ok((
                sensor.read_i16_be(Self::SHUNT_VOLTAGE_REGISTER)?,
                sensor.read_u16_be(Self::BUS_VOLTAGE_REGISTER)?,
                sensor.read_u16_be(Self::POWER_REGISTER)?,
                sensor.read_i16_be(Self::CURRENT_REGISTER)?,
            ))
        })?;

        Ok(Ina226Sample {
            bus_voltage_v: f64::from(bus_raw) * 1.25e-3,
            shunt_voltage_v: f64::from(shunt_raw) * 2.5e-6,
            current_a: f64::from(current_raw) * self.current_lsb,
            power_w: f64::from(power_raw) * self.current_lsb * 25.0,
        })
    }
}

impl ExampleIna226BoundDevice {
    fn io(&mut self) -> Ina226Io<'_> {
        Ina226Io::new(
            &self.driver_id,
            self.device.id.clone(),
            &mut *self.controller,
            self.binding.address,
            self.binding.current_lsb(),
        )
    }
}

impl BoundDevice for ExampleIna226BoundDevice {
    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn driver_id(&self) -> &str {
        &self.driver_id
    }

    fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
        let sample = self.io().sample()?;
        let state = self.binding.apply_state_config(
            DeviceStateSnapshot::new(self.device.id.clone())
                .with_lifecycle(DeviceLifecycleState::Idle),
        );

        Ok(Some(
            sample.apply_telemetry(state).with_last_operation(
                OperationRecord::new(SAMPLE_INTERACTION, OperationStatus::Succeeded)
                    .with_output(sample.into_value(&self.binding.label)),
            ),
        ))
    }

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        match request {
            InteractionRequest::Custom(request) if request.id.as_str() == SAMPLE_INTERACTION => {
                let sample = self.io().sample()?;
                Ok(InteractionResponse::Custom(
                    CustomInteractionResponse::new(self.interactions[0].id.clone())
                        .with_output(sample.into_value(&self.binding.label)),
                ))
            }
            _ => Err(DriverError::UnsupportedAction {
                driver_id: self.driver_id.clone(),
                device_id: self.device.id.clone(),
                action: interaction_name(request).into_owned(),
            }),
        }
    }

    fn custom_interactions(&self) -> &[CustomInteraction] {
        self.interactions.as_slice()
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let config = Ina226Config::builder()
        .bus(1_u32)
        .address(0x40_u16)
        .label("battery-rail")
        .shunt_resistance_ohms(0.002_f64)
        .max_current_a(8.0_f64)
        .build()
        .map_err(std::io::Error::other)?;

    let probe = Ina226Config::configured_probe("example-configured-ina226", vec![config.clone()]);
    let hardware = MockHardware::builder()
        .with_i2c_device(
            MockI2cDevice::new(config.bus, config.address)
                .with_be_i16(0x01, 500)
                .with_be_u16(0x02, 0x0C80)
                .with_be_u16(0x03, 0x0040)
                .with_be_u16(0x04, 0x0200),
        )
        .build();
    let driver = ExampleIna226Driver::new([Ina226Binding::from(&config)]);

    let mut lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_driver(driver)?
        .build();

    lemnos.refresh(&DiscoveryContext::new(), &[&hardware, &probe])?;

    let device_id = config.logical_device_id()?;
    lemnos.bind(&device_id)?;

    let response = lemnos.request_custom(device_id.clone(), SAMPLE_INTERACTION)?;
    let state = lemnos.refresh_state(&device_id)?.cloned();

    println!("INA226 response: {response:#?}");
    println!("INA226 state: {state:#?}");

    Ok(())
}
