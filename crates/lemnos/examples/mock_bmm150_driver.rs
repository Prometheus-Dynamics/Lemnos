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
use lemnos::macros::{ConfiguredDevice, enum_values};
use lemnos::mock::{MockHardware, MockI2cDevice};
use lemnos::prelude::*;
use std::borrow::Cow;
use std::error::Error;

const SAMPLE_INTERACTION: &str = "sensor.magnetometer.sample";
const CHIP_ID_REGISTER: u8 = 0x40;
const CHIP_ID_EXPECTED: u8 = 0x32;
const POWER_CONTROL_REGISTER: u8 = 0x4B;
const OP_MODE_REGISTER: u8 = 0x4C;
const AXES_ENABLE_REGISTER: u8 = 0x4E;
const REP_XY_REGISTER: u8 = 0x51;
const REP_Z_REGISTER: u8 = 0x52;
const DATA_REGISTER: u8 = 0x42;

#[enum_values(bits: u8, hertz: f64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bmm150DataRate {
    #[lemnos(bits = 0x00, hertz = 10.0)]
    Hz10,
    #[lemnos(bits = 0x07, hertz = 30.0)]
    Hz30,
}

#[enum_values(bits: u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bmm150OperationMode {
    #[lemnos(bits = 0x00)]
    Normal,
    #[lemnos(bits = 0x01)]
    Forced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bmm150Preset {
    Regular,
    HighAccuracy,
}

impl Bmm150Preset {
    const fn xy_repetitions(self) -> u8 {
        match self {
            Self::Regular => 0x04,
            Self::HighAccuracy => 0x17,
        }
    }

    const fn z_repetitions(self) -> u8 {
        match self {
            Self::Regular => 0x07,
            Self::HighAccuracy => 0x29,
        }
    }
}

#[derive(Debug, Clone, PartialEq, ConfiguredDevice)]
#[lemnos(
    interface = I2c,
    id = "example.bmm150",
    driver = "example.sensor.bmm150",
    summary = "Configured BMM150 magnetometer"
)]
struct Bmm150Config {
    #[lemnos(bus(i2c))]
    bus: u32,
    #[lemnos(endpoint(i2c, name = "sensor"))]
    address: u16,
    #[lemnos(display_name, label)]
    label: String,
    #[lemnos(property)]
    data_rate: Bmm150DataRate,
    #[lemnos(property)]
    preset: Bmm150Preset,
    #[lemnos(property)]
    mode: Bmm150OperationMode,
}

#[derive(Debug, Clone, PartialEq)]
struct Bmm150Binding {
    logical_device_id: String,
    bus: u32,
    address: u16,
    label: String,
    data_rate: Bmm150DataRate,
    preset: Bmm150Preset,
    mode: Bmm150OperationMode,
}

impl Bmm150Binding {
    fn new(
        logical_device_id: impl Into<String>,
        bus: u32,
        address: u16,
        label: impl Into<String>,
        data_rate: Bmm150DataRate,
        preset: Bmm150Preset,
        mode: Bmm150OperationMode,
    ) -> Self {
        Self {
            logical_device_id: logical_device_id.into(),
            bus,
            address,
            label: label.into(),
            data_rate,
            preset,
            mode,
        }
    }

    fn apply_state_config(&self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_config("label", self.label.clone())
            .with_config("bus", u64::from(self.bus))
            .with_config("address", u64::from(self.address))
            .with_config("data_rate_hz", self.data_rate.hertz())
            .with_config("preset", format!("{:?}", self.preset))
            .with_config("mode", format!("{:?}", self.mode))
    }
}

impl From<&Bmm150Config> for Bmm150Binding {
    fn from(value: &Bmm150Config) -> Self {
        Self::new(
            value.configured_device_id(),
            value.bus,
            value.address,
            value.label.clone(),
            value.data_rate,
            value.preset,
            value.mode,
        )
    }
}

struct ExampleBmm150Driver {
    bindings: Vec<Bmm150Binding>,
}

impl ExampleBmm150Driver {
    const DRIVER_ID: &str = "example.sensor.bmm150";

    fn new(bindings: impl IntoIterator<Item = Bmm150Binding>) -> Self {
        Self {
            bindings: bindings.into_iter().collect(),
        }
    }

    fn binding_for(&self, device: &DeviceDescriptor) -> Option<&Bmm150Binding> {
        self.bindings
            .iter()
            .find(|binding| binding.logical_device_id == device.id.as_str())
    }

    fn binding_for_device(&self, device: &DeviceDescriptor) -> DriverResult<&Bmm150Binding> {
        self.binding_for(device)
            .ok_or_else(|| DriverError::BindRejected {
                driver_id: self.id().to_string(),
                device_id: device.id.clone(),
                reason: "device is not configured for this BMM150 driver".into(),
            })
    }
}

impl Driver for ExampleBmm150Driver {
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
                "Configured BMM150 example driver",
                vec![InterfaceKind::I2c],
            )
            .with_description("Example out-of-tree style BMM150 magnetometer driver.")
            .with_priority(DriverPriority::Exact)
            .with_kind(DeviceKind::Unspecified(InterfaceKind::I2c))
            .with_custom_interaction(SAMPLE_INTERACTION, "Read a raw BMM150 sample")
            .with_tag("sensor")
            .with_tag("magnetometer")
            .with_tag("bmm150")
            .with_tag("example"),
        )
    }

    fn matches(&self, device: &DeviceDescriptor) -> DriverMatch {
        let base: DriverMatch = self.manifest_ref().match_device(device).into();
        if !base.is_supported() {
            return base;
        }

        let Some(binding) = self.binding_for(device) else {
            return DriverMatch::unsupported("device is not listed in the BMM150 binding set");
        };

        DriverMatch {
            level: DriverMatchLevel::Exact,
            score: base.score + 400,
            reasons: vec![format!(
                "configured BMM150 '{}' matched logical device '{}'",
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
            let mut io = Bmm150Io::new(
                self.id(),
                device.id.clone(),
                &mut *controller,
                binding.address,
            );
            io.verify_id()?;
            io.configure(&binding)?;
        }

        let interaction = CustomInteraction::new(
            SAMPLE_INTERACTION,
            "Read raw x/y/z/rhall values from the BMM150",
        )
        .map_err(|source| DriverError::BindFailed {
            driver_id: self.id().to_string(),
            device_id: device.id.clone(),
            reason: source.to_string(),
        })?;

        Ok(Box::new(ExampleBmm150BoundDevice {
            driver_id: self.id().to_string(),
            device: device.clone(),
            controller,
            binding,
            interactions: vec![interaction],
        }))
    }
}

struct ExampleBmm150BoundDevice {
    driver_id: String,
    device: DeviceDescriptor,
    controller: Box<dyn I2cControllerSession>,
    binding: Bmm150Binding,
    interactions: Vec<CustomInteraction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bmm150Sample {
    x_raw: i16,
    y_raw: i16,
    z_raw: i16,
    rhall_raw: u16,
}

impl Bmm150Sample {
    fn into_value(self, label: &str) -> Value {
        let mut map = ValueMap::new();
        map.insert("label".into(), Value::from(label));
        map.insert("x_raw".into(), Value::from(i64::from(self.x_raw)));
        map.insert("y_raw".into(), Value::from(i64::from(self.y_raw)));
        map.insert("z_raw".into(), Value::from(i64::from(self.z_raw)));
        map.insert("rhall_raw".into(), Value::from(u64::from(self.rhall_raw)));
        Value::from(map)
    }

    fn apply_telemetry(self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_telemetry("x_raw", i64::from(self.x_raw))
            .with_telemetry("y_raw", i64::from(self.y_raw))
            .with_telemetry("z_raw", i64::from(self.z_raw))
            .with_telemetry("rhall_raw", u64::from(self.rhall_raw))
    }
}

struct Bmm150Io<'a> {
    driver_id: &'a str,
    device_id: lemnos::core::DeviceId,
    controller: &'a mut dyn I2cControllerSession,
    address: u16,
}

impl<'a> Bmm150Io<'a> {
    fn new(
        driver_id: &'a str,
        device_id: lemnos::core::DeviceId,
        controller: &'a mut dyn I2cControllerSession,
        address: u16,
    ) -> Self {
        Self {
            driver_id,
            device_id,
            controller,
            address,
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

    fn verify_id(&mut self) -> DriverResult<()> {
        let chip_id = self.with_sensor(|sensor| sensor.read_u8(CHIP_ID_REGISTER))?;
        if chip_id != CHIP_ID_EXPECTED {
            return Err(DriverError::BindRejected {
                driver_id: self.driver_id.to_string(),
                device_id: self.device_id.clone(),
                reason: format!("unexpected chip id 0x{chip_id:02x}"),
            });
        }
        Ok(())
    }

    fn configure(&mut self, binding: &Bmm150Binding) -> DriverResult<()> {
        self.with_sensor(|sensor| {
            sensor.write_u8(POWER_CONTROL_REGISTER, 0x01)?;
            sensor.write_u8(AXES_ENABLE_REGISTER, 0x00)?;
            sensor.write_u8(REP_XY_REGISTER, binding.preset.xy_repetitions())?;
            sensor.write_u8(REP_Z_REGISTER, binding.preset.z_repetitions())?;
            let op_mode_value = (binding.data_rate.bits() << 3) | (binding.mode.bits() << 1);
            sensor.write_u8(OP_MODE_REGISTER, op_mode_value)?;
            Ok(())
        })
    }

    fn sample(&mut self) -> DriverResult<Bmm150Sample> {
        let bytes = self.with_sensor(|sensor| sensor.read_exact_block::<8>(DATA_REGISTER))?;

        Ok(Bmm150Sample {
            x_raw: convert_xy(bytes[1], bytes[0]),
            y_raw: convert_xy(bytes[3], bytes[2]),
            z_raw: convert_z(bytes[5], bytes[4]),
            rhall_raw: convert_rhall(bytes[7], bytes[6]),
        })
    }
}

impl ExampleBmm150BoundDevice {
    fn io(&mut self) -> Bmm150Io<'_> {
        Bmm150Io::new(
            &self.driver_id,
            self.device.id.clone(),
            &mut *self.controller,
            self.binding.address,
        )
    }
}

impl BoundDevice for ExampleBmm150BoundDevice {
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

fn convert_xy(msb: u8, lsb: u8) -> i16 {
    sign_extend(((msb as i16) << 5) | ((lsb as i16) >> 3), 13)
}

fn convert_z(msb: u8, lsb: u8) -> i16 {
    sign_extend(((msb as i16) << 7) | ((lsb as i16) >> 1), 15)
}

fn convert_rhall(msb: u8, lsb: u8) -> u16 {
    ((msb as u16) << 6) | ((lsb as u16) >> 2)
}

fn sign_extend(value: i16, bits: u8) -> i16 {
    let shift = 16 - bits;
    (value << shift) >> shift
}

fn exercise_enum_metadata() {
    let _ = Bmm150DataRate::Hz10.bits();
    let _ = Bmm150DataRate::Hz30.hertz();
    let _ = Bmm150OperationMode::Normal.bits();
    let _ = Bmm150Preset::HighAccuracy.xy_repetitions();
}

fn main() -> Result<(), Box<dyn Error>> {
    exercise_enum_metadata();

    let config = Bmm150Config::builder()
        .bus(2_u32)
        .address(0x10_u16)
        .label("deck-mag")
        .data_rate(Bmm150DataRate::Hz30)
        .preset(Bmm150Preset::Regular)
        .mode(Bmm150OperationMode::Forced)
        .build()
        .map_err(std::io::Error::other)?;

    let probe = Bmm150Config::configured_probe("example-configured-bmm150", vec![config.clone()]);
    let hardware = MockHardware::builder()
        .with_i2c_device(
            MockI2cDevice::new(config.bus, config.address)
                .with_u8(0x40, 0x32)
                .with_bytes(0x42, [0x20, 0x03, 0x70, 0xFE, 0x90, 0x01, 0xC0, 0x12]),
        )
        .build();
    let driver = ExampleBmm150Driver::new([Bmm150Binding::from(&config)]);

    let mut lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_driver(driver)?
        .build();

    lemnos.refresh(&DiscoveryContext::new(), &[&hardware, &probe])?;

    let device_id = config.logical_device_id()?;
    lemnos.bind(&device_id)?;

    let response = lemnos.request_custom(device_id.clone(), SAMPLE_INTERACTION)?;
    let state = lemnos.refresh_state(&device_id)?.cloned();

    println!("BMM150 response: {response:#?}");
    println!("BMM150 state: {state:#?}");

    Ok(())
}
