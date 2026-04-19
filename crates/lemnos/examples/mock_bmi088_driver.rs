#![allow(clippy::print_stdout)]

use lemnos::core::{
    ConfiguredGpioSignal, CustomInteractionResponse, DeviceDescriptor, DeviceKind,
    DeviceLifecycleState, DeviceStateSnapshot, GpioEdge, InteractionRequest, InteractionResponse,
    InterfaceKind, OperationRecord, OperationStatus, Value, ValueMap,
};
use lemnos::driver::{
    BoundDevice, CustomInteraction, Driver, DriverBindContext, DriverError, DriverManifest,
    DriverMatch, DriverMatchLevel, DriverPriority, DriverResult, I2cControllerIo,
    I2cControllerSession, SessionAccess, interaction_name,
};
use lemnos::macros::{ConfiguredDevice, enum_values};
use lemnos::mock::{MockGpioLine, MockHardware, MockI2cDevice};
use lemnos::prelude::*;
use std::borrow::Cow;
use std::error::Error;

const SAMPLE_INTERACTION: &str = "sensor.imu.sample";
const ACC_CHIP_ID_REGISTER: u8 = 0x00;
const ACC_EXPECTED_ID: u8 = 0x1E;
const GYR_CHIP_ID_REGISTER: u8 = 0x00;
const GYR_EXPECTED_ID: u8 = 0x0F;
const ACC_PWR_CONF: u8 = 0x7C;
const ACC_PWR_CTRL: u8 = 0x7D;
const ACC_CONF: u8 = 0x40;
const ACC_RANGE: u8 = 0x41;
const GYR_RANGE: u8 = 0x0F;
const GYR_BANDWIDTH: u8 = 0x10;
const GYR_LPM1: u8 = 0x11;
const ACC_DATA_START: u8 = 0x12;
const GYR_DATA_START: u8 = 0x02;

#[enum_values(reg_value: u8, max_dps: f64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bmi088GyroRange {
    #[lemnos(reg_value = 0x04, max_dps = 125.0)]
    Dps125,
    #[lemnos(reg_value = 0x03, max_dps = 250.0)]
    Dps250,
    #[lemnos(reg_value = 0x02, max_dps = 500.0)]
    Dps500,
}

impl Bmi088GyroRange {
    const fn scale(self) -> f64 {
        self.max_dps() / 32_768.0
    }
}

#[enum_values(reg_value: u8, hz: f64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bmi088GyroBandwidth {
    #[lemnos(reg_value = 0x04, hz = 200.0)]
    Odr200Hz23,
    #[lemnos(reg_value = 0x05, hz = 100.0)]
    Odr100Hz12,
}

#[enum_values(reg_value: u8, max_g: f64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bmi088AccelRange {
    #[lemnos(reg_value = 0x00, max_g = 3.0)]
    G3,
    #[lemnos(reg_value = 0x01, max_g = 6.0)]
    G6,
    #[lemnos(reg_value = 0x02, max_g = 12.0)]
    G12,
}

impl Bmi088AccelRange {
    const fn scale(self) -> f64 {
        self.max_g() / 32_768.0
    }
}

#[enum_values(reg_value: u8, hz: f64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bmi088AccelOdr {
    #[lemnos(reg_value = 0x08, hz = 100.0)]
    Hz100,
    #[lemnos(reg_value = 0x09, hz = 200.0)]
    Hz200,
}

#[enum_values(reg_value: u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bmi088AccelBandwidth {
    #[lemnos(reg_value = 0x09)]
    Osr2,
    #[lemnos(reg_value = 0x0A)]
    Normal,
}

#[derive(Debug, Clone, PartialEq, ConfiguredDevice)]
#[lemnos(
    interface = I2c,
    id = "example.bmi088",
    driver = "example.sensor.bmi088",
    summary = "Configured BMI088 IMU"
)]
struct Bmi088Config {
    #[lemnos(bus(i2c))]
    bus: u32,
    #[lemnos(endpoint(i2c, name = "accel"))]
    accel_address: u16,
    #[lemnos(endpoint(i2c, name = "gyro"))]
    gyro_address: u16,
    #[lemnos(display_name, label)]
    label: String,
    #[lemnos(property)]
    accel_range: Bmi088AccelRange,
    #[lemnos(property = "accel_odr_hz")]
    accel_odr: Bmi088AccelOdr,
    #[lemnos(property)]
    accel_bandwidth: Bmi088AccelBandwidth,
    #[lemnos(property)]
    gyro_range: Bmi088GyroRange,
    #[lemnos(property = "gyro_bandwidth_hz")]
    gyro_bandwidth: Bmi088GyroBandwidth,
    #[lemnos(signal(gpio, name = "accel_int"))]
    accel_int: Option<ConfiguredGpioSignal>,
    #[lemnos(signal(gpio, name = "gyro_int"))]
    gyro_int: Option<ConfiguredGpioSignal>,
}

#[derive(Debug, Clone, PartialEq)]
struct Bmi088Binding {
    logical_device_id: String,
    bus: u32,
    accel_address: u16,
    gyro_address: u16,
    label: String,
    accel_range: Bmi088AccelRange,
    accel_odr: Bmi088AccelOdr,
    accel_bandwidth: Bmi088AccelBandwidth,
    gyro_range: Bmi088GyroRange,
    gyro_bandwidth: Bmi088GyroBandwidth,
    accel_int: Option<ConfiguredGpioSignal>,
    gyro_int: Option<ConfiguredGpioSignal>,
}

impl Bmi088Binding {
    fn with_accel_int(mut self, signal: ConfiguredGpioSignal) -> Self {
        self.accel_int = Some(signal);
        self
    }

    fn with_gyro_int(mut self, signal: ConfiguredGpioSignal) -> Self {
        self.gyro_int = Some(signal);
        self
    }

    fn apply_state_config(&self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_config("label", self.label.clone())
            .with_config("bus", u64::from(self.bus))
            .with_config("accel_address", u64::from(self.accel_address))
            .with_config("gyro_address", u64::from(self.gyro_address))
            .with_config("accel_odr_hz", self.accel_odr.hz())
            .with_config("gyro_bandwidth_hz", self.gyro_bandwidth.hz())
            .with_config("accel_interrupt_wired", self.accel_int.is_some())
            .with_config("gyro_interrupt_wired", self.gyro_int.is_some())
    }
}

impl From<&Bmi088Config> for Bmi088Binding {
    fn from(value: &Bmi088Config) -> Self {
        let mut binding = Self {
            logical_device_id: value.configured_device_id(),
            bus: value.bus,
            accel_address: value.accel_address,
            gyro_address: value.gyro_address,
            label: value.label.clone(),
            accel_range: value.accel_range,
            accel_odr: value.accel_odr,
            accel_bandwidth: value.accel_bandwidth,
            gyro_range: value.gyro_range,
            gyro_bandwidth: value.gyro_bandwidth,
            accel_int: None,
            gyro_int: None,
        };
        if let Some(signal) = value.accel_int.clone() {
            binding = binding.with_accel_int(signal);
        }
        if let Some(signal) = value.gyro_int.clone() {
            binding = binding.with_gyro_int(signal);
        }
        binding
    }
}

struct ExampleBmi088Driver {
    bindings: Vec<Bmi088Binding>,
}

impl ExampleBmi088Driver {
    const DRIVER_ID: &str = "example.sensor.bmi088";

    fn new(bindings: impl IntoIterator<Item = Bmi088Binding>) -> Self {
        Self {
            bindings: bindings.into_iter().collect(),
        }
    }

    fn binding_for(&self, device: &DeviceDescriptor) -> Option<&Bmi088Binding> {
        self.bindings
            .iter()
            .find(|binding| binding.logical_device_id == device.id.as_str())
    }

    fn binding_for_device(&self, device: &DeviceDescriptor) -> DriverResult<&Bmi088Binding> {
        self.binding_for(device)
            .ok_or_else(|| DriverError::BindRejected {
                driver_id: self.id().to_string(),
                device_id: device.id.clone(),
                reason: "device is not configured for this BMI088 driver".into(),
            })
    }
}

impl Driver for ExampleBmi088Driver {
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
                "Configured BMI088 example driver",
                vec![InterfaceKind::I2c],
            )
            .with_description("Example out-of-tree style BMI088 multi-address IMU driver.")
            .with_priority(DriverPriority::Exact)
            .with_kind(DeviceKind::Unspecified(InterfaceKind::I2c))
            .with_custom_interaction(
                SAMPLE_INTERACTION,
                "Read accel and gyro samples from the BMI088",
            )
            .with_tag("sensor")
            .with_tag("imu")
            .with_tag("bmi088")
            .with_tag("example"),
        )
    }

    fn matches(&self, device: &DeviceDescriptor) -> DriverMatch {
        let base: DriverMatch = self.manifest_ref().match_device(device).into();
        if !base.is_supported() {
            return base;
        }

        let Some(binding) = self.binding_for(device) else {
            return DriverMatch::unsupported("device is not listed in the BMI088 binding set");
        };

        DriverMatch {
            level: DriverMatchLevel::Exact,
            score: base.score + 500,
            reasons: vec![format!(
                "configured BMI088 '{}' matched logical device '{}'",
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
            let mut io = Bmi088Io::new(
                self.id(),
                device.id.clone(),
                &mut *controller,
                binding.accel_address,
                binding.gyro_address,
                binding.accel_range.scale(),
                binding.gyro_range.scale(),
            );
            io.verify_ids()?;
            io.configure(&binding)?;
        }

        let interaction = CustomInteraction::new(SAMPLE_INTERACTION, "Read accel_g and gyro_dps")
            .map_err(|source| DriverError::BindFailed {
            driver_id: self.id().to_string(),
            device_id: device.id.clone(),
            reason: source.to_string(),
        })?;

        Ok(Box::new(ExampleBmi088BoundDevice {
            driver_id: self.id().to_string(),
            device: device.clone(),
            controller,
            binding,
            interactions: vec![interaction],
        }))
    }
}

struct ExampleBmi088BoundDevice {
    driver_id: String,
    device: DeviceDescriptor,
    controller: Box<dyn I2cControllerSession>,
    binding: Bmi088Binding,
    interactions: Vec<CustomInteraction>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Bmi088Sample {
    accel_g: [f64; 3],
    gyro_dps: [f64; 3],
}

impl Bmi088Sample {
    fn into_value(self, label: &str) -> Value {
        let mut map = ValueMap::new();
        map.insert("label".into(), Value::from(label));
        map.insert("accel_g".into(), vec3_value(self.accel_g));
        map.insert("gyro_dps".into(), vec3_value(self.gyro_dps));
        Value::from(map)
    }

    fn apply_telemetry(self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_telemetry("accel_g", vec3_value(self.accel_g))
            .with_telemetry("gyro_dps", vec3_value(self.gyro_dps))
    }
}

struct Bmi088Io<'a> {
    driver_id: &'a str,
    device_id: lemnos::core::DeviceId,
    controller: &'a mut dyn I2cControllerSession,
    accel_address: u16,
    gyro_address: u16,
    accel_scale: f64,
    gyro_scale: f64,
}

impl<'a> Bmi088Io<'a> {
    fn new(
        driver_id: &'a str,
        device_id: lemnos::core::DeviceId,
        controller: &'a mut dyn I2cControllerSession,
        accel_address: u16,
        gyro_address: u16,
        accel_scale: f64,
        gyro_scale: f64,
    ) -> Self {
        Self {
            driver_id,
            device_id,
            controller,
            accel_address,
            gyro_address,
            accel_scale,
            gyro_scale,
        }
    }

    fn with_accel<T>(
        &mut self,
        action: impl FnOnce(&mut lemnos::driver::I2cControllerTarget<'_>) -> DriverResult<T>,
    ) -> DriverResult<T> {
        let mut bus = I2cControllerIo::with_device_id(
            &mut *self.controller,
            self.driver_id,
            self.device_id.clone(),
        );
        let mut accel = bus.target(self.accel_address);
        action(&mut accel)
    }

    fn with_gyro<T>(
        &mut self,
        action: impl FnOnce(&mut lemnos::driver::I2cControllerTarget<'_>) -> DriverResult<T>,
    ) -> DriverResult<T> {
        let mut bus = I2cControllerIo::with_device_id(
            &mut *self.controller,
            self.driver_id,
            self.device_id.clone(),
        );
        let mut gyro = bus.target(self.gyro_address);
        action(&mut gyro)
    }

    fn verify_ids(&mut self) -> DriverResult<()> {
        let accel_id = self.with_accel(|accel| accel.read_u8(ACC_CHIP_ID_REGISTER))?;
        if accel_id != ACC_EXPECTED_ID {
            return Err(DriverError::BindRejected {
                driver_id: self.driver_id.to_string(),
                device_id: self.device_id.clone(),
                reason: format!("unexpected accel chip id 0x{accel_id:02x}"),
            });
        }

        let gyro_id = self.with_gyro(|gyro| gyro.read_u8(GYR_CHIP_ID_REGISTER))?;
        if gyro_id != GYR_EXPECTED_ID {
            return Err(DriverError::BindRejected {
                driver_id: self.driver_id.to_string(),
                device_id: self.device_id.clone(),
                reason: format!("unexpected gyro chip id 0x{gyro_id:02x}"),
            });
        }

        Ok(())
    }

    fn configure(&mut self, binding: &Bmi088Binding) -> DriverResult<()> {
        self.with_accel(|accel| {
            accel.write_u8(ACC_PWR_CONF, 0x00)?;
            accel.write_u8(ACC_PWR_CTRL, 0x04)?;
            accel.write_u8(ACC_RANGE, binding.accel_range.reg_value())?;
            accel.write_u8(
                ACC_CONF,
                (binding.accel_bandwidth.reg_value() << 4) | binding.accel_odr.reg_value(),
            )?;
            Ok(())
        })?;

        self.with_gyro(|gyro| {
            gyro.write_u8(GYR_RANGE, binding.gyro_range.reg_value())?;
            gyro.write_u8(GYR_BANDWIDTH, binding.gyro_bandwidth.reg_value())?;
            gyro.write_u8(GYR_LPM1, 0x00)?;
            Ok(())
        })
    }

    fn sample(&mut self) -> DriverResult<Bmi088Sample> {
        let accel_bytes = self.with_accel(|accel| accel.read_exact_block::<6>(ACC_DATA_START))?;
        let gyro_bytes = self.with_gyro(|gyro| gyro.read_exact_block::<6>(GYR_DATA_START))?;

        let accel_raw = decode_vec3_i16_le(&accel_bytes, self.driver_id, &self.device_id)?;
        let gyro_raw = decode_vec3_i16_le(&gyro_bytes, self.driver_id, &self.device_id)?;

        Ok(Bmi088Sample {
            accel_g: [
                f64::from(accel_raw[0]) * self.accel_scale,
                f64::from(accel_raw[1]) * self.accel_scale,
                f64::from(accel_raw[2]) * self.accel_scale,
            ],
            gyro_dps: [
                f64::from(gyro_raw[0]) * self.gyro_scale,
                f64::from(gyro_raw[1]) * self.gyro_scale,
                f64::from(gyro_raw[2]) * self.gyro_scale,
            ],
        })
    }
}

impl ExampleBmi088BoundDevice {
    fn io(&mut self) -> Bmi088Io<'_> {
        Bmi088Io::new(
            &self.driver_id,
            self.device.id.clone(),
            &mut *self.controller,
            self.binding.accel_address,
            self.binding.gyro_address,
            self.binding.accel_range.scale(),
            self.binding.gyro_range.scale(),
        )
    }
}

impl BoundDevice for ExampleBmi088BoundDevice {
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

fn decode_vec3_i16_le(
    bytes: &[u8],
    driver_id: &str,
    device_id: &lemnos::core::DeviceId,
) -> DriverResult<[i16; 3]> {
    let chunk: [u8; 6] = bytes
        .try_into()
        .map_err(|_| DriverError::InvariantViolation {
            driver_id: driver_id.to_string(),
            device_id: device_id.clone(),
            reason: "short vec3 read".into(),
        })?;
    Ok([
        i16::from_le_bytes([chunk[0], chunk[1]]),
        i16::from_le_bytes([chunk[2], chunk[3]]),
        i16::from_le_bytes([chunk[4], chunk[5]]),
    ])
}

fn vec3_value(values: [f64; 3]) -> Value {
    Value::from(vec![
        Value::from(values[0]),
        Value::from(values[1]),
        Value::from(values[2]),
    ])
}

fn exercise_enum_metadata() {
    let _ = Bmi088GyroRange::Dps125.scale();
    let _ = Bmi088GyroRange::Dps250.reg_value();
    let _ = Bmi088GyroBandwidth::Odr100Hz12.hz();
    let _ = Bmi088AccelRange::G3.scale();
    let _ = Bmi088AccelRange::G6.reg_value();
    let _ = Bmi088AccelOdr::Hz100.hz();
    let _ = Bmi088AccelBandwidth::Osr2.reg_value();
}

fn main() -> Result<(), Box<dyn Error>> {
    exercise_enum_metadata();

    let accel_interrupt =
        ConfiguredGpioSignal::by_device_id("mock.gpio.board-imu.5")?.with_edge(GpioEdge::Rising);

    let config = Bmi088Config::builder()
        .bus(4_u32)
        .accel_address(0x18_u16)
        .gyro_address(0x68_u16)
        .label("board-imu")
        .accel_range(Bmi088AccelRange::G12)
        .accel_odr(Bmi088AccelOdr::Hz200)
        .accel_bandwidth(Bmi088AccelBandwidth::Normal)
        .gyro_range(Bmi088GyroRange::Dps500)
        .gyro_bandwidth(Bmi088GyroBandwidth::Odr200Hz23)
        .accel_int(accel_interrupt)
        .build()
        .map_err(std::io::Error::other)?;

    let probe = Bmi088Config::configured_probe("example-configured-bmi088", vec![config.clone()]);
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("board-imu", 5).with_line_name("bmi088-accel-int"))
        .with_i2c_device(
            MockI2cDevice::new(config.bus, config.accel_address)
                .with_u8(0x00, 0x1E)
                .with_bytes(0x12, [0x00, 0x08, 0x00, 0xFC, 0x00, 0x10]),
        )
        .with_i2c_device(
            MockI2cDevice::new(config.bus, config.gyro_address)
                .with_u8(0x00, 0x0F)
                .with_bytes(0x02, [0x00, 0x02, 0x00, 0xFF, 0x00, 0x04]),
        )
        .build();
    let driver = ExampleBmi088Driver::new([Bmi088Binding::from(&config)]);

    let mut lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_driver(driver)?
        .build();

    lemnos.refresh(&DiscoveryContext::new(), &[&hardware, &probe])?;

    let device_id = config.logical_device_id()?;
    lemnos.bind(&device_id)?;

    let response = lemnos.request_custom(device_id.clone(), SAMPLE_INTERACTION)?;
    let state = lemnos.refresh_state(&device_id)?.cloned();

    println!("BMI088 response: {response:#?}");
    println!("BMI088 state: {state:#?}");

    Ok(())
}
