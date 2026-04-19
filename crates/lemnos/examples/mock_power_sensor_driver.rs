#![allow(clippy::print_stdout)]

use lemnos::core::{
    CapabilityId, CustomInteractionResponse, DeviceAddress, DeviceDescriptor, DeviceKind,
    DeviceLifecycleState, DeviceStateSnapshot, InteractionRequest, InteractionResponse,
    OperationRecord, OperationStatus, Value, ValueMap,
};
use lemnos::driver::{
    BoundDevice, CONFIG_ADDRESS, CONFIG_ADDRESS_HEX, CONFIG_BUS, CustomInteraction, Driver,
    DriverBindContext, DriverError, DriverManifest, DriverMatch, DriverMatchLevel, DriverPriority,
    DriverResult, I2cDeviceIo, I2cSession, MatchCondition, MatchRule, SessionAccess, i2c,
    interaction_name,
};
use lemnos::mock::{MockHardware, MockI2cDevice};
use lemnos::prelude::*;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::error::Error;

const SAMPLE_INTERACTION: &str = "sensor.power.sample";
const CONFIG_LABEL: &str = "label";
const CONFIG_SHUNT_OHMS: &str = "shunt_ohms";
const OUTPUT_LABEL: &str = "label";
const TELEMETRY_BUS_VOLTAGE_V: &str = "bus_voltage_v";
const TELEMETRY_CURRENT_A: &str = "current_a";
const TELEMETRY_POWER_W: &str = "power_w";
const TELEMETRY_SHUNT_VOLTAGE_V: &str = "shunt_voltage_v";

#[derive(Debug, Clone)]
struct PowerSensorBinding {
    bus: u32,
    address: u16,
    label: String,
    shunt_ohms: f64,
}

impl PowerSensorBinding {
    fn new(bus: u32, address: u16, label: impl Into<String>, shunt_ohms: f64) -> Self {
        Self {
            bus,
            address,
            label: label.into(),
            shunt_ohms,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PowerSample {
    bus_voltage_v: f64,
    current_a: f64,
    power_w: f64,
    shunt_voltage_v: f64,
}

struct ExampleIna260Driver {
    bindings: BTreeMap<(u32, u16), PowerSensorBinding>,
}

impl ExampleIna260Driver {
    const DRIVER_ID: &str = "example.sensor.ina260";
    const MANUFACTURER_ID_REGISTER: u8 = 0xFE;
    const MANUFACTURER_ID_TI: u16 = 0x5449;

    fn from_configs(configs: impl IntoIterator<Item = PowerSensorBinding>) -> Self {
        let bindings = configs
            .into_iter()
            .map(|config| ((config.bus, config.address), config))
            .collect();
        Self { bindings }
    }

    fn binding_for(&self, device: &DeviceDescriptor) -> Option<&PowerSensorBinding> {
        let (bus, address) = i2c_address(device)?;
        self.bindings.get(&(bus, address))
    }

    fn binding_for_device(&self, device: &DeviceDescriptor) -> DriverResult<&PowerSensorBinding> {
        self.binding_for(device)
            .ok_or_else(|| DriverError::BindRejected {
                driver_id: self.id().to_string(),
                device_id: device.id.clone(),
                reason: "device is not configured for this driver".into(),
            })
    }
}

impl Driver for ExampleIna260Driver {
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
                "Example INA260-style power sensor driver",
                vec![InterfaceKind::I2c],
            )
            .with_description(
                "Shows how a lib-sensors-style I2C device backend becomes a Lemnos driver.",
            )
            .with_priority(DriverPriority::Preferred)
            .with_kind(DeviceKind::I2cDevice)
            .with_custom_interaction(SAMPLE_INTERACTION, "Read a typed power sample")
            .with_rule(
                MatchRule::new(100)
                    .described("requires I2C write_read support")
                    .require(MatchCondition::Kind(DeviceKind::I2cDevice))
                    .require(MatchCondition::Capability(
                        CapabilityId::new(i2c::WRITE_READ_INTERACTION)
                            .expect("write_read capability id"),
                    )),
            )
            .with_tag("sensor")
            .with_tag("power"),
        )
    }

    fn matches(&self, device: &DeviceDescriptor) -> DriverMatch {
        let base: DriverMatch = self.manifest_ref().match_device(device).into();
        if !base.is_supported() {
            return base;
        }

        let Some(binding) = self.binding_for(device) else {
            return DriverMatch::unsupported(
                "device is not listed in the sensor binding configuration",
            );
        };

        DriverMatch {
            level: DriverMatchLevel::Exact,
            score: base.score + 300,
            reasons: vec![format!(
                "configured sensor '{}' matched i2c bus {} address 0x{:02x}",
                binding.label, binding.bus, binding.address
            )],
            matched_rule: base.matched_rule,
        }
    }

    fn bind(
        &self,
        device: &DeviceDescriptor,
        context: &DriverBindContext<'_>,
    ) -> DriverResult<Box<dyn BoundDevice>> {
        if device.kind != DeviceKind::I2cDevice {
            return Err(DriverError::BindRejected {
                driver_id: self.id().to_string(),
                device_id: device.id.clone(),
                reason: format!("expected i2c-device, found {}", device.kind),
            });
        }

        let binding = self.binding_for_device(device)?.clone();

        let mut session = context.open_i2c(self.id(), device, SessionAccess::Shared)?;
        {
            let mut io = Ina260Io::new(
                self.id(),
                device.id.clone(),
                &mut *session,
                binding.shunt_ohms,
            );
            io.verify_identity()?;
        }

        let interaction = CustomInteraction::new(
            SAMPLE_INTERACTION,
            "Read volts, amps, watts, and shunt voltage",
        )
        .map_err(|source| DriverError::BindFailed {
            driver_id: self.id().to_string(),
            device_id: device.id.clone(),
            reason: source.to_string(),
        })?;

        Ok(Box::new(ExampleIna260BoundDevice {
            driver_id: self.id().to_string(),
            session,
            binding,
            interactions: vec![interaction],
        }))
    }
}

struct ExampleIna260BoundDevice {
    driver_id: String,
    session: Box<dyn I2cSession>,
    binding: PowerSensorBinding,
    interactions: Vec<CustomInteraction>,
}

impl PowerSensorBinding {
    fn apply_state_config(&self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_config(CONFIG_LABEL, self.label.clone())
            .with_config(CONFIG_BUS, u64::from(self.bus))
            .with_config(CONFIG_ADDRESS, u64::from(self.address))
            .with_config(CONFIG_ADDRESS_HEX, format!("0x{:02x}", self.address))
            .with_config(CONFIG_SHUNT_OHMS, self.shunt_ohms)
    }
}

impl PowerSample {
    fn into_value(self, label: &str) -> Value {
        let mut output = ValueMap::new();
        output.insert(OUTPUT_LABEL.into(), Value::from(label));
        output.insert(
            TELEMETRY_BUS_VOLTAGE_V.into(),
            Value::from(self.bus_voltage_v),
        );
        output.insert(TELEMETRY_CURRENT_A.into(), Value::from(self.current_a));
        output.insert(TELEMETRY_POWER_W.into(), Value::from(self.power_w));
        output.insert(
            TELEMETRY_SHUNT_VOLTAGE_V.into(),
            Value::from(self.shunt_voltage_v),
        );
        Value::from(output)
    }

    fn apply_telemetry(self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_telemetry(TELEMETRY_BUS_VOLTAGE_V, self.bus_voltage_v)
            .with_telemetry(TELEMETRY_CURRENT_A, self.current_a)
            .with_telemetry(TELEMETRY_POWER_W, self.power_w)
            .with_telemetry(TELEMETRY_SHUNT_VOLTAGE_V, self.shunt_voltage_v)
    }
}

struct Ina260Io<'a> {
    driver_id: &'a str,
    device_id: lemnos::core::DeviceId,
    session: &'a mut dyn I2cSession,
    shunt_ohms: f64,
}

impl<'a> Ina260Io<'a> {
    const BUS_VOLTAGE_REGISTER: u8 = 0x02;
    const CURRENT_REGISTER: u8 = 0x01;
    const POWER_REGISTER: u8 = 0x03;

    const BUS_VOLTAGE_LSB_V: f64 = 0.00125;
    const CURRENT_LSB_A: f64 = 0.00125;
    const POWER_LSB_W: f64 = 0.01;

    fn new(
        driver_id: &'a str,
        device_id: lemnos::core::DeviceId,
        session: &'a mut dyn I2cSession,
        shunt_ohms: f64,
    ) -> Self {
        Self {
            driver_id,
            device_id,
            session,
            shunt_ohms,
        }
    }

    fn device(&mut self) -> I2cDeviceIo<'_> {
        I2cDeviceIo::with_device_id(self.session, self.driver_id, self.device_id.clone())
    }

    fn verify_identity(&mut self) -> DriverResult<()> {
        let manufacturer = self
            .device()
            .read_u16_be(ExampleIna260Driver::MANUFACTURER_ID_REGISTER)?;
        if manufacturer != ExampleIna260Driver::MANUFACTURER_ID_TI {
            return Err(DriverError::BindRejected {
                driver_id: self.driver_id.to_string(),
                device_id: self.device_id.clone(),
                reason: format!(
                    "unexpected manufacturer id 0x{manufacturer:04x}; expected 0x{:04x}",
                    ExampleIna260Driver::MANUFACTURER_ID_TI
                ),
            });
        }
        Ok(())
    }

    fn sample(&mut self) -> DriverResult<PowerSample> {
        let bus_voltage_raw = self.device().read_u16_be(Self::BUS_VOLTAGE_REGISTER)?;
        let current_raw = self.device().read_i16_be(Self::CURRENT_REGISTER)?;
        let power_raw = self.device().read_u16_be(Self::POWER_REGISTER)?;

        let bus_voltage_v = f64::from(bus_voltage_raw) * Self::BUS_VOLTAGE_LSB_V;
        let current_a = f64::from(current_raw) * Self::CURRENT_LSB_A;
        let power_w = f64::from(power_raw) * Self::POWER_LSB_W;
        let shunt_voltage_v = current_a * self.shunt_ohms;

        Ok(PowerSample {
            bus_voltage_v,
            current_a,
            power_w,
            shunt_voltage_v,
        })
    }
}

impl ExampleIna260BoundDevice {
    fn io(&mut self) -> Ina260Io<'_> {
        Ina260Io::new(
            &self.driver_id,
            self.session.device().id.clone(),
            &mut *self.session,
            self.binding.shunt_ohms,
        )
    }

    fn sample_response(&mut self) -> DriverResult<CustomInteractionResponse> {
        let sample = self.io().sample()?;
        Ok(
            CustomInteractionResponse::new(self.interactions[0].id.clone())
                .with_output(sample.into_value(&self.binding.label)),
        )
    }

    fn state_snapshot(&mut self) -> DriverResult<DeviceStateSnapshot> {
        let sample = self.io().sample()?;
        let state = self.binding.apply_state_config(
            DeviceStateSnapshot::new(self.session.device().id.clone())
                .with_lifecycle(DeviceLifecycleState::Idle),
        );

        Ok(sample.apply_telemetry(state).with_last_operation(
            OperationRecord::new(SAMPLE_INTERACTION, OperationStatus::Succeeded)
                .with_summary(format!(
                    "{} reports {:.2} W at {:.2} V",
                    self.binding.label, sample.power_w, sample.bus_voltage_v
                ))
                .with_output(sample.into_value(&self.binding.label)),
        ))
    }
}

impl BoundDevice for ExampleIna260BoundDevice {
    fn device(&self) -> &DeviceDescriptor {
        self.session.device()
    }

    fn driver_id(&self) -> &str {
        self.driver_id.as_str()
    }

    fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
        Ok(Some(self.state_snapshot()?))
    }

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        request
            .validate()
            .map_err(|source| DriverError::InvalidRequest {
                driver_id: self.driver_id.clone(),
                device_id: self.session.device().id.clone(),
                source,
            })?;

        match request {
            InteractionRequest::Custom(request) if request.id.as_str() == SAMPLE_INTERACTION => {
                Ok(InteractionResponse::Custom(self.sample_response()?))
            }
            _ => Err(DriverError::UnsupportedAction {
                driver_id: self.driver_id.clone(),
                device_id: self.session.device().id.clone(),
                action: interaction_name(request).into_owned(),
            }),
        }
    }

    fn custom_interactions(&self) -> &[CustomInteraction] {
        self.interactions.as_slice()
    }
}

fn i2c_address(device: &DeviceDescriptor) -> Option<(u32, u16)> {
    match &device.address {
        Some(DeviceAddress::I2cDevice { bus, address }) => Some((*bus, *address)),
        _ => None,
    }
}

fn mock_ina260() -> MockI2cDevice {
    MockI2cDevice::new(1, 0x40)
        .with_display_name("main-battery-monitor")
        .with_be_u16(0xFE, 0x5449)
        .with_be_u16(0x02, 0x0C80)
        .with_be_u16(0x01, 0x0320)
        .with_be_u16(0x03, 0x0190)
}

fn print_custom_sample(response: &InteractionResponse) {
    let InteractionResponse::Custom(response) = response else {
        return;
    };
    let Some(Value::Map(output)) = response.output.as_ref() else {
        return;
    };

    println!(
        "sample: label={} volts={:.2} amps={:.2} watts={:.2}",
        output
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>"),
        output
            .get("bus_voltage_v")
            .and_then(Value::as_f64)
            .unwrap_or_default(),
        output
            .get("current_a")
            .and_then(Value::as_f64)
            .unwrap_or_default(),
        output
            .get("power_w")
            .and_then(Value::as_f64)
            .unwrap_or_default(),
    );
}

fn main() -> Result<(), Box<dyn Error>> {
    let hardware = MockHardware::builder()
        .with_i2c_device(mock_ina260())
        .build();

    let mut lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_builtin_drivers()?
        .with_driver(ExampleIna260Driver::from_configs([
            PowerSensorBinding::new(1, 0x40, "main-battery", 0.002),
        ]))?
        .build();

    lemnos.refresh_with_mock(&DiscoveryContext::new(), &hardware)?;

    let device_id = lemnos
        .inventory()
        .first_id_by_kind(DeviceKind::I2cDevice)
        .expect("mock I2C device should be present");

    let response = lemnos.request_custom(device_id.clone(), SAMPLE_INTERACTION)?;
    print_custom_sample(&response.interaction);

    let state = lemnos
        .refresh_state(&device_id)?
        .expect("driver should publish a state snapshot");
    println!(
        "cached telemetry: power_w={:?} shunt_voltage_v={:?}",
        state.telemetry.get("power_w"),
        state.telemetry.get("shunt_voltage_v"),
    );

    Ok(())
}
