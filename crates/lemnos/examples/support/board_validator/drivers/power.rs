use crate::support::board_validator::config::PowerSensorKind;
use lemnos::core::{
    CustomInteractionResponse, DeviceDescriptor, DeviceKind, DeviceLifecycleState,
    DeviceStateSnapshot, InteractionRequest, InteractionResponse, InterfaceKind, OperationRecord,
    OperationStatus, Value, ValueMap,
};
use lemnos::driver::{
    BoundDevice, CONFIG_ADDRESS, CONFIG_BUS, CustomInteraction, Driver, DriverBindContext,
    DriverError, DriverManifest, DriverMatch, DriverMatchLevel, DriverPriority, DriverResult,
    I2cControllerIo, I2cControllerSession, SessionAccess, interaction_name,
};
use lemnos::macros::ConfiguredDevice;
use std::borrow::Cow;

const POWER_SAMPLE_INTERACTION: &str = "sensor.power.sample";
const CONFIG_LABEL: &str = "label";
const CONFIG_KIND: &str = "kind";
const OUTPUT_LABEL: &str = "label";
const OUTPUT_KIND: &str = "kind";
const TELEMETRY_KIND: &str = "kind";
const TELEMETRY_BUS_VOLTAGE_V: &str = "bus_voltage_v";
const TELEMETRY_SHUNT_VOLTAGE_V: &str = "shunt_voltage_v";
const TELEMETRY_CURRENT_A: &str = "current_a";
const TELEMETRY_POWER_W: &str = "power_w";
const TELEMETRY_DIE_TEMPERATURE_C: &str = "die_temperature_c";

#[derive(Debug, Clone, PartialEq, ConfiguredDevice)]
#[lemnos(
    interface = I2c,
    id = "validator.power",
    driver = "validator.sensor.power",
    summary = "Configured power sensor validator"
)]
pub struct PowerSensorConfig {
    #[lemnos(bus(i2c))]
    bus: u32,
    #[lemnos(endpoint(i2c, name = "sensor"))]
    address: u16,
    #[lemnos(display_name, label)]
    label: String,
    kind: PowerSensorKind,
    shunt_resistance_ohms: f64,
    max_current_a: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct PowerSensorBinding {
    logical_device_id: String,
    bus: u32,
    address: u16,
    label: String,
    kind: PowerSensorKind,
    shunt_resistance_ohms: f64,
    max_current_a: f64,
}

impl PowerSensorBinding {
    fn current_lsb(&self) -> f64 {
        self.max_current_a / 32_768.0
    }

    fn calibration(&self) -> u16 {
        (0.00512_f64 / (self.current_lsb() * self.shunt_resistance_ohms)).round() as u16
    }

    fn apply_state_config(&self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_config(CONFIG_LABEL, self.label.clone())
            .with_config(CONFIG_BUS, u64::from(self.bus))
            .with_config(CONFIG_ADDRESS, u64::from(self.address))
            .with_config(CONFIG_KIND, format!("{:?}", self.kind))
            .with_config("shunt_resistance_ohms", self.shunt_resistance_ohms)
            .with_config("max_current_a", self.max_current_a)
    }
}

impl From<&PowerSensorConfig> for PowerSensorBinding {
    fn from(value: &PowerSensorConfig) -> Self {
        Self {
            logical_device_id: value.configured_device_id(),
            bus: value.bus,
            address: value.address,
            label: value.label.clone(),
            kind: value.kind,
            shunt_resistance_ohms: value.shunt_resistance_ohms,
            max_current_a: value.max_current_a,
        }
    }
}

pub struct PowerSensorDriver {
    bindings: Vec<PowerSensorBinding>,
}

impl PowerSensorDriver {
    const DRIVER_ID: &str = "validator.sensor.power";

    pub fn from_configs(configs: impl IntoIterator<Item = PowerSensorConfig>) -> Self {
        Self {
            bindings: configs.into_iter().map(|config| (&config).into()).collect(),
        }
    }

    fn binding_for(&self, device: &DeviceDescriptor) -> Option<&PowerSensorBinding> {
        self.bindings
            .iter()
            .find(|binding| binding.logical_device_id == device.id.as_str())
    }

    fn binding_for_device(&self, device: &DeviceDescriptor) -> DriverResult<&PowerSensorBinding> {
        self.binding_for(device)
            .ok_or_else(|| DriverError::BindRejected {
                driver_id: self.id().to_string(),
                device_id: device.id.clone(),
                reason: "device is not configured for this power sensor driver".into(),
            })
    }
}

impl Driver for PowerSensorDriver {
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
                "Configured power sensor validator driver",
                vec![InterfaceKind::I2c],
            )
            .with_description("Board validator driver for configured INA226/INA238/INA260 sensors.")
            .with_priority(DriverPriority::Exact)
            .with_kind(DeviceKind::Unspecified(InterfaceKind::I2c))
            .with_custom_interaction(
                POWER_SAMPLE_INTERACTION,
                "Read a power sensor sample from the configured device",
            )
            .with_tag("validator")
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
                "device is not listed in the power sensor binding set",
            );
        };

        DriverMatch {
            level: DriverMatchLevel::Exact,
            score: base.score + 425,
            reasons: vec![format!(
                "configured {:?} '{}' matched logical device '{}'",
                binding.kind, binding.label, binding.logical_device_id
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
            let mut io = PowerSensorIo::new(self.id(), device.clone(), &mut *controller, &binding);
            io.configure_and_verify()?;
        }

        let interaction = CustomInteraction::new(
            POWER_SAMPLE_INTERACTION,
            "Read bus voltage, current, power, and related metrics",
        )
        .map_err(|source| DriverError::BindFailed {
            driver_id: self.id().to_string(),
            device_id: device.id.clone(),
            reason: source.to_string(),
        })?;

        Ok(Box::new(PowerSensorBoundDevice {
            driver_id: self.id().to_string(),
            device: device.clone(),
            controller,
            binding,
            interactions: vec![interaction],
        }))
    }
}

struct PowerSensorBoundDevice {
    driver_id: String,
    device: DeviceDescriptor,
    controller: Box<dyn I2cControllerSession>,
    binding: PowerSensorBinding,
    interactions: Vec<CustomInteraction>,
}

#[derive(Debug, Clone, PartialEq)]
struct PowerSensorSample {
    kind: PowerSensorKind,
    bus_voltage_v: f64,
    shunt_voltage_v: f64,
    current_a: f64,
    power_w: f64,
    die_temperature_c: Option<f64>,
}

impl PowerSensorSample {
    fn to_value(&self, label: &str) -> Value {
        let mut map = ValueMap::new();
        map.insert(OUTPUT_LABEL.into(), Value::from(label));
        map.insert(OUTPUT_KIND.into(), Value::from(format!("{:?}", self.kind)));
        map.insert(
            TELEMETRY_BUS_VOLTAGE_V.into(),
            Value::from(self.bus_voltage_v),
        );
        map.insert(
            TELEMETRY_SHUNT_VOLTAGE_V.into(),
            Value::from(self.shunt_voltage_v),
        );
        map.insert(TELEMETRY_CURRENT_A.into(), Value::from(self.current_a));
        map.insert(TELEMETRY_POWER_W.into(), Value::from(self.power_w));
        if let Some(temperature) = self.die_temperature_c {
            map.insert(TELEMETRY_DIE_TEMPERATURE_C.into(), Value::from(temperature));
        }
        Value::from(map)
    }

    fn apply_telemetry(&self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        let state = state
            .with_telemetry(TELEMETRY_BUS_VOLTAGE_V, self.bus_voltage_v)
            .with_telemetry(TELEMETRY_SHUNT_VOLTAGE_V, self.shunt_voltage_v)
            .with_telemetry(TELEMETRY_CURRENT_A, self.current_a)
            .with_telemetry(TELEMETRY_POWER_W, self.power_w)
            .with_telemetry(TELEMETRY_KIND, format!("{:?}", self.kind));
        if let Some(temperature) = self.die_temperature_c {
            state.with_telemetry(TELEMETRY_DIE_TEMPERATURE_C, temperature)
        } else {
            state
        }
    }
}

struct PowerSensorIo<'a> {
    driver_id: &'a str,
    device: DeviceDescriptor,
    controller: &'a mut dyn I2cControllerSession,
    binding: &'a PowerSensorBinding,
}

impl<'a> PowerSensorIo<'a> {
    fn new(
        driver_id: &'a str,
        device: DeviceDescriptor,
        controller: &'a mut dyn I2cControllerSession,
        binding: &'a PowerSensorBinding,
    ) -> Self {
        Self {
            driver_id,
            device,
            controller,
            binding,
        }
    }

    fn configure_and_verify(&mut self) -> DriverResult<()> {
        match self.binding.kind {
            PowerSensorKind::Ina226 => self.configure_ina226(),
            PowerSensorKind::Ina238 => self.configure_ina238(),
            PowerSensorKind::Ina260 => self.verify_ina260(),
        }
    }

    fn sample(&mut self) -> DriverResult<PowerSensorSample> {
        match self.binding.kind {
            PowerSensorKind::Ina226 => self.sample_ina226(),
            PowerSensorKind::Ina238 => self.sample_ina238(),
            PowerSensorKind::Ina260 => self.sample_ina260(),
        }
    }

    fn configure_ina226(&mut self) -> DriverResult<()> {
        let calibration = self.binding.calibration();
        let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
        let mut sensor = bus.target(self.binding.address);
        sensor.write_u16_be(0x05, calibration)?;
        sensor.write_u16_be(0x00, 0x4527)?;
        Ok(())
    }

    fn configure_ina238(&mut self) -> DriverResult<()> {
        let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
        let mut sensor = bus.target(self.binding.address);
        sensor.write_u16_be(0x00, 0x0000)?;
        sensor.write_u16_be(0x01, 0xFB6A)?;
        sensor.write_u16_be(0x02, 0x4000)?;
        sensor.write_u16_be(0x0B, 0x2000)?;
        Ok(())
    }

    fn verify_ina260(&mut self) -> DriverResult<()> {
        let manufacturer = {
            let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
            let mut sensor = bus.target(self.binding.address);
            sensor.read_u16_be(0xFE)?
        };
        if manufacturer != 0x5449 {
            return Err(DriverError::BindRejected {
                driver_id: self.driver_id.to_string(),
                device_id: self.device.id.clone(),
                reason: format!(
                    "unexpected INA260 manufacturer id 0x{manufacturer:04x} (expected 0x5449)"
                ),
            });
        }
        Ok(())
    }

    fn sample_ina226(&mut self) -> DriverResult<PowerSensorSample> {
        let (shunt_voltage, bus_voltage, current, power) = {
            let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
            let mut sensor = bus.target(self.binding.address);
            let shunt_voltage =
                i16::from_be_bytes(sensor.read_u16_be(0x01)?.to_be_bytes()) as f64 * 2.5e-6_f64;
            let bus_voltage = f64::from(sensor.read_u16_be(0x02)?) * 1.25e-3_f64;
            let current = i16::from_be_bytes(sensor.read_u16_be(0x04)?.to_be_bytes()) as f64
                * self.binding.current_lsb();
            let power = f64::from(sensor.read_u16_be(0x03)?) * self.binding.current_lsb() * 25.0;
            (shunt_voltage, bus_voltage, current, power)
        };

        Ok(PowerSensorSample {
            kind: PowerSensorKind::Ina226,
            bus_voltage_v: bus_voltage,
            shunt_voltage_v: shunt_voltage,
            current_a: current,
            power_w: power,
            die_temperature_c: None,
        })
    }

    fn sample_ina238(&mut self) -> DriverResult<PowerSensorSample> {
        let (shunt_voltage, bus_voltage, current, power, die_temperature) = {
            let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
            let mut sensor = bus.target(self.binding.address);
            let shunt_voltage =
                i16::from_be_bytes(sensor.read_u16_be(0x04)?.to_be_bytes()) as f64 * 5.0e-6_f64;
            let bus_voltage = f64::from(sensor.read_u16_be(0x05)?) * 3.125e-3_f64;
            let current = i16::from_be_bytes(sensor.read_u16_be(0x07)?.to_be_bytes()) as f64
                * 4.0_f64
                * 5.0e-6_f64
                / self.binding.shunt_resistance_ohms;
            let power_bytes = sensor.read_exact_block::<3>(0x08)?;
            let power_raw = (u32::from(power_bytes[0]) << 16)
                | (u32::from(power_bytes[1]) << 8)
                | u32::from(power_bytes[2]);
            let power =
                f64::from(power_raw) * 4.0_f64 * 1.0e-6_f64 / self.binding.shunt_resistance_ohms;
            let die_temperature =
                (i16::from_be_bytes(sensor.read_u16_be(0x06)?.to_be_bytes()) >> 4) as f64 * 0.125;
            (shunt_voltage, bus_voltage, current, power, die_temperature)
        };

        Ok(PowerSensorSample {
            kind: PowerSensorKind::Ina238,
            bus_voltage_v: bus_voltage,
            shunt_voltage_v: shunt_voltage,
            current_a: current,
            power_w: power,
            die_temperature_c: Some(die_temperature),
        })
    }

    fn sample_ina260(&mut self) -> DriverResult<PowerSensorSample> {
        let (bus_voltage, current, power) = {
            let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
            let mut sensor = bus.target(self.binding.address);
            let bus_voltage = f64::from(sensor.read_u16_be(0x02)?) * 0.00125_f64;
            let current =
                i16::from_be_bytes(sensor.read_u16_be(0x01)?.to_be_bytes()) as f64 * 0.00125_f64;
            let power = f64::from(sensor.read_u16_be(0x03)?) * 0.01_f64;
            (bus_voltage, current, power)
        };

        Ok(PowerSensorSample {
            kind: PowerSensorKind::Ina260,
            bus_voltage_v: bus_voltage,
            shunt_voltage_v: current * 0.002_f64,
            current_a: current,
            power_w: power,
            die_temperature_c: None,
        })
    }
}

impl BoundDevice for PowerSensorBoundDevice {
    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn driver_id(&self) -> &str {
        &self.driver_id
    }

    fn custom_interactions(&self) -> &[CustomInteraction] {
        &self.interactions
    }

    fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
        let sample = {
            let mut io = PowerSensorIo::new(
                &self.driver_id,
                self.device.clone(),
                &mut *self.controller,
                &self.binding,
            );
            io.sample()?
        };
        let state = self.binding.apply_state_config(
            DeviceStateSnapshot::new(self.device.id.clone())
                .with_lifecycle(DeviceLifecycleState::Idle),
        );
        let output = sample.to_value(&self.binding.label);
        Ok(Some(
            sample.apply_telemetry(state).with_last_operation(
                OperationRecord::new(POWER_SAMPLE_INTERACTION, OperationStatus::Succeeded)
                    .with_output(output),
            ),
        ))
    }

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        match request {
            InteractionRequest::Custom(request)
                if request.id.as_str() == POWER_SAMPLE_INTERACTION =>
            {
                let sample = {
                    let mut io = PowerSensorIo::new(
                        &self.driver_id,
                        self.device.clone(),
                        &mut *self.controller,
                        &self.binding,
                    );
                    io.sample()?
                };
                Ok(InteractionResponse::Custom(
                    CustomInteractionResponse::new(self.interactions[0].id.clone())
                        .with_output(sample.to_value(&self.binding.label)),
                ))
            }
            _ => Err(DriverError::UnsupportedAction {
                driver_id: self.driver_id.clone(),
                device_id: self.device.id.clone(),
                action: interaction_name(request).into_owned(),
            }),
        }
    }
}
